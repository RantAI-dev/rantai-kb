//! `GraphStore` over the Postgres graph tables.
//!
//! Storage is plain tables (`KbEntity`, `KbMention`, `KbRelation`,
//! `KbGraphVersion`, added by `prisma/migrations/20261017120000_graph_tables`) and
//! traversal is Rust, over the CSR `kb_core::graph` builds. That is the decision
//! in `docs/architecture-target.md` §11, and this adapter is where it is enforced:
//!
//! * **The scope filter is on the row.** Both graph reads filter
//!   `"documentId" = ANY(scope)`, so an out-of-scope relation is never fetched and
//!   therefore can never become a path through the graph. Filtering the *result*
//!   would leave the edge in the CSR and leak the shape of a document the caller
//!   cannot see.
//! * **Ids are derived, not generated.** An entity id is a hash of
//!   `(tenant, key)` and a relation id of `(tenant, src, dst, type, document)`, so
//!   re-ingesting a document replaces rows instead of inserting new ones, and two
//!   runs of the same ingest write byte-identical rows. A `uuid()` default would
//!   make every re-ingest a different row set, which is the bug
//!   `reingest_leaves_no_stale_tail` exists to catch on the retrieval port.
//! * **Writes are one transaction.** Entities, mentions, relations and the version
//!   counter move together, so a reader that sees a new version also sees the rows
//!   that version describes. A version bumped by a transaction that then rolled
//!   back would tell every cached CSR to reload and find nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use async_trait::async_trait;
use kb_core::graph::{Csr, Edge, NodeMap};
use kb_ports::{
    ChunkId, DocumentId, DocumentIntelligenceData, DocumentScope, EntityMentions, EntityStore,
    EntityUpsert, EntityView, GraphStore, PortError, RelationUpsert, RelationView, TenantGraph,
    TenantId,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

use crate::graph_queries::{
    BUMP_VERSION, COUNT_EXISTING_ENTITIES, DELETE_DOCUMENT_MENTIONS, DELETE_DOCUMENT_RELATIONS,
    DELETE_ORPHANED_ENTITIES, INSERT_MENTION, INSERT_RELATION,
    SELECT_DOCUMENT_INTELLIGENCE_ENTITIES, SELECT_DOCUMENT_INTELLIGENCE_RELATIONS,
    SELECT_DOCUMENT_MENTIONED_ENTITY_IDS, SELECT_SCOPED_ENTITIES, SELECT_SCOPED_ENTITY_LABELS,
    SELECT_SCOPED_ENTITY_MENTIONS, SELECT_SCOPED_MENTION_ROWS, SELECT_SCOPED_RELATION_ROWS,
    SELECT_VERSION, UPSERT_ENTITY,
};
use crate::store::PgSettings;

/// Postgres-backed entity graph.
#[derive(Clone)]
pub struct PgGraphStore {
    pool: PgPool,
}

impl PgGraphStore {
    /// Construct from an existing pool, so one process can share a connection pool
    /// between the document store, the job store and the graph.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connect using the same settings as the document store.
    ///
    /// # Errors
    ///
    /// Fails when the pool cannot be opened, naming the driver error.
    pub async fn connect(settings: &PgSettings) -> Result<Self, PortError> {
        Ok(Self::from_pool(crate::store::connect_pool(settings).await?))
    }

    /// The pool, for a caller that wants to run its own statement in the same
    /// transaction as a graph write.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Load a scoped graph: three statements, then a CSR.
    ///
    /// The three statements are the entities the scope can see, the mention rows
    /// inside it, and the relation rows inside it — all unjoined and unsorted. The
    /// join and the sort were measured and both were expensive: sorting 100k
    /// relations by entity key in SQL cost 526 ms of a 628 ms load, and the two
    /// `KbEntity` joins cost an index lookup per row. Both are moved into Rust,
    /// where a `HashMap` resolve and a `BTreeMap` insert are cheap and, more to the
    /// point, are the same code the in-memory adapter runs, so the two adapters
    /// cannot drift on what "the same graph" means.
    ///
    /// Three round trips at ~100k relations, not one: the alternative is a single
    /// joined query, and it is the one that was slow.
    ///
    /// # What it costs, measured
    ///
    /// 10,000 entities, 100,000 relations over 100 documents, on the `kb-smoke`
    /// Postgres, p50 over 20 samples: **855 ms** (p95 992 ms). The split matters
    /// more than the total, because it says where the time is not:
    ///
    /// | Stage | ms | Note |
    /// |---|---:|---|
    /// | Postgres executing and sending all three queries | ~71 | psql, same queries |
    /// | sqlx building `PgRow`s for 120k rows | ~470 | the single largest cost |
    /// | Resolving 200k relation endpoints to node ids | ~235 | |
    /// | `Csr::from_edges` over 100k edges | ~65 | |
    /// | Mentions, interning, owning the result | ~65 | |
    ///
    /// So the database is ~8% of this load and the driver is ~55%. Two consequences
    /// for anyone reading this before optimising it again:
    ///
    /// * **This is a cache-miss cost, not a query cost.** `docs/architecture-target.md`
    ///   §11 puts the CSR behind a per-tenant version counter, so this runs once per
    ///   graph mutation and every read after it is free. A load that were on the
    ///   per-query path would be unsalvageable; amortised, it pays for itself after
    ///   one query. Making the *walk* fit in 10 ms is what matters, and it does.
    /// * **The next lever is the driver, not the schema.** Decoding into positional
    ///   tuples (`query_as`) or `COPY` would take the ~470 ms `PgRow` cost down
    ///   without touching the tables. Nothing else in the profile is worth more.
    /// * **Columns are read by index, not by name.** `try_get("src_id")` searches
    ///   the row's column names on every call, which cost 240 ms of the load before
    ///   it was fixed. The select lists are therefore load-bearing, and
    ///   `the_scoped_statements_keep_the_column_order_the_indices_assume` pins them.
    async fn build_graph(
        pool: &PgPool,
        tenant: &TenantId,
        document_ids: &[String],
    ) -> Result<TenantGraph, PortError> {
        // 1-3. The three reads do not depend on each other, so they run
        //      concurrently: server-side they are ~4 ms (mentions), ~15 ms
        //      (entities) and ~18 ms (relations) on a 100k-relation tenant, and
        //      running them in sequence puts all three latencies on the critical
        //      path for no reason. Resolution — ids to keys, keys to node ids —
        //      happens after, because that is the part that must be ordered.
        let entities = sqlx::query(SELECT_SCOPED_ENTITIES)
            .bind(tenant.as_str())
            .bind(document_ids)
            .fetch_all(pool);
        let mentions = sqlx::query(SELECT_SCOPED_MENTION_ROWS)
            .bind(tenant.as_str())
            .bind(document_ids)
            .fetch_all(pool);
        let relations = sqlx::query(SELECT_SCOPED_RELATION_ROWS)
            .bind(tenant.as_str())
            .bind(document_ids)
            .fetch_all(pool);
        let (entity_rows, mention_rows, relation_rows) =
            tokio::try_join!(entities, mentions, relations)
                .map_err(|e| PortError::Unavailable(format!("pg scoped graph read: {e}")))?;

        // Everything below borrows out of the fetched rows instead of decoding
        // into owned `String`s. It is not a stylistic choice: at 100k relations
        // the owned version allocated ~230k strings per load — two per relation
        // endpoint, one per mention chunk, one per entity key — and that decode
        // measured 670 ms against 480 ms for the fetch itself. The keys and chunk
        // ids are copied once, at the end, into the [`TenantGraph`] that owns
        // them; the id-to-key map, the mention sets and the edge list are all
        // working state that dies with this function.
        //
        // The `&str`s borrow the `PgRow` buffers, which the three `Vec`s own for
        // the rest of the function, so nothing dangles; the one thing that must not
        // happen is a mutation of those vectors while a borrow is live, and
        // nothing below does.
        let mut key_by_id: HashMap<&str, &str> = HashMap::with_capacity(entity_rows.len());
        for row in &entity_rows {
            let id: &str = row
                .try_get(0)
                .map_err(|e| PortError::Unavailable(format!("pg entity id: {e}")))?;
            let key: &str = row
                .try_get(1)
                .map_err(|e| PortError::Unavailable(format!("pg entity key: {e}")))?;
            key_by_id.insert(id, key);
        }

        // Mentions resolved to keys. A `BTreeMap`, so a node id is a function of the
        // scoped row set rather than of the order Postgres returned rows in.
        let mut scoped: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for row in &mention_rows {
            let entity_id: &str = row
                .try_get(0)
                .map_err(|e| PortError::Unavailable(format!("pg mention entity_id: {e}")))?;
            // A mention for an entity the scope cannot see is not in this graph.
            // That is the scope rule, applied before anything else looks at it.
            let Some(&key) = key_by_id.get(entity_id) else {
                continue;
            };
            let chunk: &str = row
                .try_get(1)
                .map_err(|e| PortError::Unavailable(format!("pg mention chunk_id: {e}")))?;
            scoped.entry(key).or_default().insert(chunk);
        }

        let mut map = NodeMap::new();
        for key in scoped.keys() {
            if map.intern(key).is_none() {
                return Err(PortError::Malformed(format!(
                    "entity key `{key}` does not fit the dense node numbering"
                )));
            }
        }

        // Relations resolved to node ids, through one lookup per endpoint.
        //
        // The obvious version of this loop resolves each endpoint twice: once
        // from the relation's id to the entity key, once from the key to the node
        // id. That measured 310 ms for 100k relations — the largest single Rust
        // cost in the load — because each row paid four hash probes into two
        // 10k-entry string-keyed maps, and every probe lands on a different cache
        // line. Collapsing id → node into one `HashMap` built after interning
        // halves the probes and makes the map a third of the size.
        //
        // `id_by_node` is also what the scope rule needs to be enforced on: it only
        // holds ids the scope interned, so an endpoint the scope cannot see has no
        // entry and its edge is dropped, rather than defaulted into a node.
        let mut node_by_id: HashMap<&str, u32> = HashMap::with_capacity(entity_rows.len());
        for row in &entity_rows {
            let id: &str = row
                .try_get(0)
                .map_err(|e| PortError::Unavailable(format!("pg entity id: {e}")))?;
            let key: &str = row
                .try_get(1)
                .map_err(|e| PortError::Unavailable(format!("pg entity key: {e}")))?;
            if let Some(node) = map.id_of(key) {
                node_by_id.insert(id, node);
            }
        }

        // An endpoint the scope cannot see is dropped rather than defaulted, so a
        // row that points at a deleted or out-of-scope entity cannot invent a node.
        let mut edges: Vec<Edge> = Vec::with_capacity(relation_rows.len());
        for row in &relation_rows {
            let src_id: &str = row
                .try_get(0)
                .map_err(|e| PortError::Unavailable(format!("pg relation src_id: {e}")))?;
            let dst_id: &str = row
                .try_get(1)
                .map_err(|e| PortError::Unavailable(format!("pg relation dst_id: {e}")))?;
            // `DOUBLE PRECISION` in the schema, so read `f64` and narrow: asking
            // sqlx for `f32` is a type error at runtime, not a conversion.
            let weight: f64 = row
                .try_get(2)
                .map_err(|e| PortError::Unavailable(format!("pg relation weight: {e}")))?;
            let (Some(&src), Some(&dst)) = (node_by_id.get(src_id), node_by_id.get(dst_id)) else {
                continue;
            };
            edges.push(Edge::new(src, dst, weight.max(0.0) as f32));
        }
        let csr = Csr::from_edges(map.len(), &edges);

        // The one place the borrowed working state becomes owned data.
        let mentions: Vec<EntityMentions> = scoped
            .into_iter()
            .map(|(key, chunks)| EntityMentions {
                key: key.to_string(),
                chunk_ids: chunks.into_iter().map(|c| ChunkId(c.to_string())).collect(),
            })
            .collect();
        Ok(TenantGraph::new(map, csr, mentions))
    }
}

/// A deterministic id from the parts that identify a row.
///
/// SHA-256 over the parts, truncated to 16 bytes and formatted as a UUID-shaped
/// string. Not a v4 UUID and not a v5 UUID: there is no namespace to name and no
/// upstream UUID to hash, so the only thing that matters is that the same parts
/// give the same string on every machine and every run. Colons are kept because
/// Prisma's `uuid()` default produces that shape and the column is TEXT.
fn derived_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        // A NUL separator so ("ab", "c") and ("a", "bc") cannot collide.
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let hex = hex::encode(&digest[..16]);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Entity id for `(tenant, key)`.
fn entity_id(tenant: &TenantId, key: &str) -> String {
    derived_id(&["kb-entity", tenant.as_str(), key])
}

/// Mention id for `(tenant, entity key, chunk)`.
fn mention_id(tenant: &TenantId, key: &str, chunk_id: &str) -> String {
    derived_id(&["kb-mention", tenant.as_str(), key, chunk_id])
}

/// Relation id for `(tenant, src, dst, type, document)`. The document is part of
/// the identity on purpose: the same relation asserted by two documents is two
/// pieces of evidence, and each is scoped to the document it came from.
fn relation_id(
    tenant: &TenantId,
    src_key: &str,
    dst_key: &str,
    relation_type: &str,
    document_id: &DocumentId,
) -> String {
    derived_id(&[
        "kb-relation",
        tenant.as_str(),
        src_key,
        dst_key,
        relation_type,
        document_id.as_str(),
    ])
}

#[async_trait]
impl GraphStore for PgGraphStore {
    async fn upsert_entities(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        entities: Vec<EntityUpsert>,
    ) -> Result<(), PortError> {
        for entity in &entities {
            if entity.key.trim().is_empty() {
                return Err(PortError::Malformed(
                    "entity key must not be empty".to_string(),
                ));
            }
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg begin: {e}")))?;

        for entity in &entities {
            let key = entity.key.trim();
            let id: String = sqlx::query_scalar(UPSERT_ENTITY)
                .bind(entity_id(tenant, key))
                .bind(tenant.as_str())
                .bind(key)
                .bind(&entity.name)
                .bind(&entity.entity_type)
                .bind(&entity.aliases)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| PortError::Unavailable(format!("pg upsert entity: {e}")))?;

            for mention in &entity.mentions {
                sqlx::query(INSERT_MENTION)
                    .bind(mention_id(tenant, key, mention.as_str()))
                    .bind(tenant.as_str())
                    .bind(&id)
                    .bind(mention.as_str())
                    .bind(document_id.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| PortError::Unavailable(format!("pg insert mention: {e}")))?;
            }
        }

        sqlx::query(BUMP_VERSION)
            .bind(tenant.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg bump version: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg commit: {e}")))
    }

    async fn relate(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        relations: Vec<RelationUpsert>,
    ) -> Result<(), PortError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg begin: {e}")))?;

        // Whole-batch validation before any write: half a batch is a relation set
        // no extraction could explain, and it is far harder to notice than a
        // rejected request.
        //
        // Note the empty batch falls through to the version bump below rather than
        // returning early. It stores nothing, but "nothing was stored" is not "no
        // write happened": the caller's CSR cache is about to be invalidated by
        // the ingest that produced this call, and an adapter that quietly skipped
        // the bump on the empty case would leave a stale graph for exactly the
        // documents whose extraction found no relations. The in-memory reference
        // bumps here, and two adapters that disagree here are two adapters.
        let wanted: BTreeSet<String> = relations
            .iter()
            .flat_map(|r| [r.src_key.clone(), r.dst_key.clone()])
            .collect();
        let wanted_vec: Vec<String> = wanted.iter().cloned().collect();
        let rows = sqlx::query(COUNT_EXISTING_ENTITIES)
            .bind(tenant.as_str())
            .bind(&wanted_vec)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg relation endpoints: {e}")))?;
        let found: BTreeSet<String> = rows
            .iter()
            .filter_map(|row| row.try_get::<String, _>("key").ok())
            .collect();
        if let Some(missing) = wanted.difference(&found).next() {
            return Err(PortError::Malformed(format!(
                "relation endpoint `{missing}` does not exist in this tenant"
            )));
        }

        for relation in &relations {
            let src = entity_id(tenant, relation.src_key.trim());
            let dst = entity_id(tenant, relation.dst_key.trim());
            sqlx::query(INSERT_RELATION)
                .bind(relation_id(
                    tenant,
                    relation.src_key.trim(),
                    relation.dst_key.trim(),
                    &relation.relation_type,
                    document_id,
                ))
                .bind(tenant.as_str())
                .bind(src)
                .bind(dst)
                .bind(&relation.relation_type)
                .bind(f64::from(relation.weight))
                .bind(relation.chunk_id.as_ref().map(ChunkId::as_str))
                .bind(document_id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|e| PortError::Unavailable(format!("pg insert relation: {e}")))?;
        }

        sqlx::query(BUMP_VERSION)
            .bind(tenant.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg bump version: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg commit: {e}")))
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<(), PortError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg begin: {e}")))?;

        // Which entities this document mentioned, captured first: after the
        // mentions are gone that question has no answer, and the orphan sweep needs
        // it to avoid deleting an entity that was never mentioned at all.
        let entity_rows = sqlx::query(SELECT_DOCUMENT_MENTIONED_ENTITY_IDS)
            .bind(tenant.as_str())
            .bind(document_id.as_str())
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg mentioned entity ids: {e}")))?;
        let mut entity_ids: Vec<String> = entity_rows
            .iter()
            .filter_map(|row| row.try_get::<String, _>("entityId").ok())
            .filter(|id| !id.is_empty())
            .collect();
        entity_ids.sort();
        entity_ids.dedup();

        sqlx::query(DELETE_DOCUMENT_RELATIONS)
            .bind(tenant.as_str())
            .bind(document_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg delete relations: {e}")))?;
        sqlx::query(DELETE_DOCUMENT_MENTIONS)
            .bind(tenant.as_str())
            .bind(document_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg delete mentions: {e}")))?;
        if !entity_ids.is_empty() {
            sqlx::query(DELETE_ORPHANED_ENTITIES)
                .bind(tenant.as_str())
                .bind(&entity_ids)
                .execute(&mut *tx)
                .await
                .map_err(|e| PortError::Unavailable(format!("pg delete orphans: {e}")))?;
        }

        sqlx::query(BUMP_VERSION)
            .bind(tenant.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg bump version: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg commit: {e}")))
    }

    async fn load_tenant_graph(&self, scope: &DocumentScope) -> Result<TenantGraph, PortError> {
        let document_ids: Vec<String> = scope
            .document_ids()
            .iter()
            .map(ToString::to_string)
            .collect();
        if document_ids.is_empty() {
            return Ok(TenantGraph::empty());
        }
        PgGraphStore::build_graph(&self.pool, scope.tenant(), &document_ids).await
    }

    async fn graph_version(&self, tenant: &TenantId) -> Result<u64, PortError> {
        let row = sqlx::query(SELECT_VERSION)
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg graph version: {e}")))?;
        match row {
            // Missing row means "never written", which is version 0.
            None => Ok(0),
            Some(row) => {
                let version: i64 = row
                    .try_get("version")
                    .map_err(|e| PortError::Unavailable(format!("pg version column: {e}")))?;
                u64::try_from(version).map_err(|_| {
                    PortError::Unavailable(format!("pg version {version} is negative"))
                })
            }
        }
    }

    async fn match_entities(
        &self,
        scope: &DocumentScope,
        query_terms: &[String],
    ) -> Result<Vec<(String, f32)>, PortError> {
        let terms: Vec<String> = query_terms
            .iter()
            .map(|term| term.trim().to_lowercase())
            .filter(|term| !term.is_empty())
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let document_ids: Vec<String> = scope
            .document_ids()
            .iter()
            .map(ToString::to_string)
            .collect();
        if document_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(SELECT_SCOPED_ENTITY_LABELS)
            .bind(scope.tenant().as_str())
            .bind(&document_ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg entity labels: {e}")))?;

        // Matching in Rust, not SQL: the in-memory adapter matches the same way,
        // and a second definition of "matches" would make the two adapters answer
        // differently on the same data — which is exactly what the contract suite
        // exists to make visible.
        let mut out: Vec<(String, f32)> = Vec::new();
        for row in &rows {
            let key: String = row
                .try_get("entity_key")
                .map_err(|e| PortError::Unavailable(format!("pg label key: {e}")))?;
            let name: String = row
                .try_get("name")
                .map_err(|e| PortError::Unavailable(format!("pg label name: {e}")))?;
            let aliases: Vec<String> = row
                .try_get("aliases")
                .map_err(|e| PortError::Unavailable(format!("pg label aliases: {e}")))?;
            let mut hits = 0.0f32;
            for term in &terms {
                if name.to_lowercase().contains(term)
                    || aliases
                        .iter()
                        .any(|alias| alias.to_lowercase().contains(term))
                {
                    hits += 1.0;
                }
            }
            if hits > 0.0 {
                out.push((key, hits / terms.len() as f32));
            }
        }
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        Ok(out)
    }

    async fn entity_mentions(
        &self,
        scope: &DocumentScope,
        key: &str,
    ) -> Result<Vec<ChunkId>, PortError> {
        let document_ids: Vec<String> = scope
            .document_ids()
            .iter()
            .map(ToString::to_string)
            .collect();
        if document_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(SELECT_SCOPED_ENTITY_MENTIONS)
            .bind(scope.tenant().as_str())
            .bind(key)
            .bind(&document_ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg entity mentions: {e}")))?;
        let mut out: Vec<ChunkId> = rows
            .iter()
            .filter_map(|row| row.try_get::<String, _>("chunk_id").ok())
            .map(ChunkId)
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }
}

#[async_trait]
impl EntityStore for PgGraphStore {
    /// `GET /v1/documents/:id/intelligence` from the graph tables.
    ///
    /// The graph does not store an extractor confidence or free-form metadata
    /// (a Postgres row is an assertion, not a scored candidate), so entities
    /// report `confidence: 1.0` and relations their stored edge weight. Both
    /// queries filter on tenant *and* document: the route has already checked
    /// visibility, and this keeps the read safe on its own.
    async fn get_document_intelligence(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<DocumentIntelligenceData, PortError> {
        let entity_rows = sqlx::query(SELECT_DOCUMENT_INTELLIGENCE_ENTITIES)
            .bind(tenant.as_str())
            .bind(document_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg intelligence entities: {e}")))?;
        let relation_rows = sqlx::query(SELECT_DOCUMENT_INTELLIGENCE_RELATIONS)
            .bind(tenant.as_str())
            .bind(document_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg intelligence relations: {e}")))?;

        let mut entities = Vec::with_capacity(entity_rows.len());
        for row in &entity_rows {
            let id: String = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("pg intelligence entity id: {e}")))?;
            let key: String = row
                .try_get("entity_key")
                .map_err(|e| PortError::Unavailable(format!("pg intelligence entity key: {e}")))?;
            let name: String = row
                .try_get("name")
                .map_err(|e| PortError::Unavailable(format!("pg intelligence entity name: {e}")))?;
            let entity_type: String = row
                .try_get("entity_type")
                .map_err(|e| PortError::Unavailable(format!("pg intelligence entity type: {e}")))?;
            let aliases: Vec<String> = row.try_get("aliases").unwrap_or_default();
            entities.push(EntityView {
                id,
                name,
                r#type: entity_type,
                confidence: 1.0,
                metadata: serde_json::json!({ "key": key, "aliases": aliases }),
            });
        }

        let mut relations = Vec::with_capacity(relation_rows.len());
        for row in &relation_rows {
            let id: String = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("pg intelligence relation id: {e}")))?;
            let src_key: String = row.try_get("src_key").map_err(|e| {
                PortError::Unavailable(format!("pg intelligence relation src: {e}"))
            })?;
            let dst_key: String = row.try_get("dst_key").map_err(|e| {
                PortError::Unavailable(format!("pg intelligence relation dst: {e}"))
            })?;
            let relation_type: String = row.try_get("relation_type").map_err(|e| {
                PortError::Unavailable(format!("pg intelligence relation type: {e}"))
            })?;
            let weight: f64 = row.try_get("weight").unwrap_or(1.0);
            relations.push(RelationView {
                id,
                from_id: src_key,
                out: dst_key,
                relation_type,
                confidence: weight as f32,
                metadata: serde_json::json!({}),
            });
        }

        Ok(DocumentIntelligenceData {
            entities,
            relations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_ids_are_deterministic_and_part_separated() {
        let a = derived_id(&["a", "b"]);
        let b = derived_id(&["a", "b"]);
        assert_eq!(a, b, "the same parts must give the same id on every run");
        assert_ne!(a, derived_id(&["b", "a"]), "order matters");
        // Without the separator these two would collide.
        assert_ne!(derived_id(&["ab", "c"]), derived_id(&["a", "bc"]));
        assert_eq!(a.len(), 36, "uuid-shaped: 8-4-4-4-12");
        assert_eq!(a.matches('-').count(), 4);
    }

    #[test]
    fn entity_and_relation_ids_are_scoped_to_their_tenant() {
        let a = TenantId::new_unchecked("tenant-a");
        let b = TenantId::new_unchecked("tenant-b");
        assert_ne!(entity_id(&a, "candy"), entity_id(&b, "candy"));
        assert_eq!(entity_id(&a, "candy"), entity_id(&a, "candy"));
        let doc = DocumentId::new_unchecked("doc-1");
        let other = DocumentId::new_unchecked("doc-2");
        assert_ne!(
            relation_id(&a, "x", "y", "T", &doc),
            relation_id(&a, "x", "y", "T", &other),
            "the same relation from two documents is two rows: each is scoped to its own document"
        );
    }

    #[test]
    fn graph_statements_filter_on_the_document() {
        // The scope rule is the `"documentId" = ANY($n)` predicate. If a future edit
        // drops it, these fail before the contract suite does.
        assert!(SELECT_SCOPED_MENTION_ROWS.contains(r#""documentId" = ANY($2)"#));
        assert!(SELECT_SCOPED_RELATION_ROWS.contains(r#""documentId" = ANY($2)"#));
        assert!(SELECT_SCOPED_ENTITIES.contains(r#""documentId" = ANY($2)"#));
        assert!(SELECT_SCOPED_ENTITY_LABELS.contains(r#""documentId" = ANY($2)"#));
        assert!(SELECT_SCOPED_ENTITY_MENTIONS.contains(r#""documentId" = ANY($3)"#));
        // The intelligence view is tenant *and* document scoped: a document id
        // alone must never be enough to read another tenant's entities.
        assert!(SELECT_DOCUMENT_INTELLIGENCE_ENTITIES.contains(r#"m."tenantId" = $1"#));
        assert!(SELECT_DOCUMENT_INTELLIGENCE_ENTITIES.contains(r#"m."documentId" = $2"#));
        assert!(SELECT_DOCUMENT_INTELLIGENCE_RELATIONS.contains(r#"r."tenantId" = $1"#));
        assert!(SELECT_DOCUMENT_INTELLIGENCE_RELATIONS.contains(r#"r."documentId" = $2"#));
    }

    #[test]
    fn the_orphan_sweep_is_scoped_to_the_ids_it_was_given() {
        // A blanket "delete every entity with no mentions" would delete entities
        // that were never mentioned at all, which is why the ids are captured first.
        assert!(DELETE_ORPHANED_ENTITIES.contains(r#"e."id" = ANY($2)"#));
        assert!(DELETE_ORPHANED_ENTITIES.contains("NOT EXISTS"));
    }

    /// `build_graph` reads the three scoped statements by column *index*, because
    /// `try_get` by name searches the row's column names on every call and that
    /// cost 240 ms of a load. That makes the select lists load-bearing, so they are
    /// pinned here: reordering a column without updating the index would otherwise
    /// decode the wrong field, or fail at runtime on the `f64` weight.

    #[test]
    fn the_scoped_statements_keep_the_column_order_the_indices_assume() {
        for (statement, expected) in [
            (SELECT_SCOPED_ENTITIES, vec!["id", "entity_key"]),
            (SELECT_SCOPED_MENTION_ROWS, vec!["entity_id", "chunk_id"]),
            (
                SELECT_SCOPED_RELATION_ROWS,
                vec!["src_id", "dst_id", "weight"],
            ),
        ] {
            // Parsed rather than asserted with `contains`, because the order is the
            // thing being pinned and `contains` cannot see it. A parse that returns
            // nothing yields an empty list, which fails the comparison below just
            // as loudly as a reordered list does.
            let columns: Vec<String> = statement
                .split("SELECT")
                .nth(1)
                .and_then(|rest| rest.split("FROM").next())
                .map(|list| {
                    list.split(',')
                        .map(|part| {
                            part.rsplit(" AS ")
                                .next()
                                .unwrap_or_default()
                                .trim()
                                .trim_matches('"')
                                .to_string()
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(columns, expected, "column order changed in:\n{statement}");
        }
    }
}
