//! The in-memory `GraphStore`: the reference adapter.
//!
//! This is the implementation the Postgres adapter is measured against, so it is
//! written for legibility over speed. Everything the port promises is visible here
//! as plain data:
//!
//! * rows live under their real tenant, keyed by entity key and by document, which
//!   is the same keying the SQL adapter uses;
//! * [`InMemoryGraphStore::load_tenant_graph`] builds a [`kb_core::graph::NodeMap`]
//!   in sorted key order and a [`kb_core::graph::Csr`] over it, **after** dropping
//!   every mention and relation outside the scope. The scope filter is on the edge
//!   set, not on the output, because filtering output is the leak this port exists
//!   to prevent;
//! * the version counter is a plain `u64` per tenant, bumped by every write
//!   including one that changes nothing.
//!
//! It lives in `kb-engine`'s fakes rather than in `kb-ports` because it is a test
//! double and the engine is where the other fakes are.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use kb_core::graph::{Csr, Edge, NodeMap};
use kb_ports::{
    ChunkId, DocumentId, DocumentScope, EntityMentions, EntityUpsert, GraphStore, PortError,
    RelationUpsert, TenantGraph, TenantId,
};

/// One stored entity. The port's `key` is the identity; `name` and `aliases` are
/// surface forms that merge rather than replace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StoredEntity {
    name: String,
    entity_type: String,
    /// Sorted, deduplicated, and never emptied by a re-ingest that did not mention
    /// every alias.
    aliases: Vec<String>,
}

/// One stored relation, with the document it came from. That document is the whole
/// reason the scope rule is enforceable: a relation without provenance would have
/// to be trusted or ignored, and neither is safe.
#[derive(Debug, Clone, PartialEq)]
struct StoredRelation {
    src_key: String,
    dst_key: String,
    relation_type: String,
    weight: f32,
    chunk_id: Option<ChunkId>,
    document_id: DocumentId,
}

/// Everything one tenant owns.
#[derive(Debug, Clone, Default)]
struct TenantData {
    entities: BTreeMap<String, StoredEntity>,
    /// Mention edges: entity key -> chunk id -> owning document.
    mentions: BTreeMap<String, BTreeMap<String, DocumentId>>,
    /// Relations, deduplicated by (src, dst, type, document).
    relations: Vec<StoredRelation>,
}

/// In-memory `GraphStore`. `Default` is a usable, empty store, and `Clone` shares
/// state, so a clone is a second handle on the same tenant.
#[derive(Debug, Clone, Default)]
pub struct InMemoryGraphStore {
    tenants: Arc<RwLock<HashMap<TenantId, TenantData>>>,
    versions: Arc<RwLock<HashMap<TenantId, u64>>>,
}

impl InMemoryGraphStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Entity keys a tenant owns, in key order. Lets a test check dedupe and
    /// cascade behaviour without going through a scope.
    #[must_use]
    pub fn entity_keys(&self, tenant: &TenantId) -> Vec<String> {
        self.read(tenant, |data| data.entities.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Every document id this tenant has any trace of, ascending.
    ///
    /// Exists for the deliberately leaking test double, which needs to widen a
    /// scope to "everything this tenant has" in order to leak through it, and for
    /// tests that want the tenant's whole-tenant scope without listing ids.
    #[must_use]
    pub fn known_documents(&self, tenant: &TenantId) -> Vec<DocumentId> {
        self.read(tenant, |data| {
            let mut ids: BTreeSet<String> = BTreeSet::new();
            for chunks in data.mentions.values() {
                for owner in chunks.values() {
                    ids.insert(owner.0.clone());
                }
            }
            for relation in &data.relations {
                ids.insert(relation.document_id.0.clone());
            }
            ids.into_iter().map(DocumentId).collect()
        })
        .unwrap_or_default()
    }

    /// Relations a tenant owns, in a deterministic order.
    #[must_use]
    pub fn relations(&self, tenant: &TenantId) -> Vec<(String, String, String, DocumentId)> {
        self.read(tenant, |data| {
            let mut out: Vec<(String, String, String, DocumentId)> = data
                .relations
                .iter()
                .map(|r| {
                    (
                        r.src_key.clone(),
                        r.dst_key.clone(),
                        r.relation_type.clone(),
                        r.document_id.clone(),
                    )
                })
                .collect();
            out.sort();
            out
        })
        .unwrap_or_default()
    }

    /// Read one tenant's data. `None` when the tenant has never been written to.
    fn read<T>(&self, tenant: &TenantId, f: impl FnOnce(&TenantData) -> T) -> Option<T> {
        // A poisoned lock means another thread panicked while holding it. The data
        // is still structurally intact, and a fake that propagates a panic into an
        // unrelated test is worse than one that reports "nothing here".
        let guard = self.tenants.read().ok()?;
        let data = guard.get(tenant)?;
        Some(f(data))
    }

    fn write(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, HashMap<TenantId, TenantData>>, PortError> {
        self.tenants
            .write()
            .map_err(|_| PortError::Unavailable("in-memory graph store lock poisoned".to_string()))
    }

    /// Bump the tenant's version. Every write calls this, including a write that
    /// changed nothing: a cache that is not invalidated reads as "the answer
    /// changed and nobody knows why".
    fn bump(&self, tenant: &TenantId) -> Result<(), PortError> {
        let mut guard = self.versions.write().map_err(|_| {
            PortError::Unavailable("in-memory graph store lock poisoned".to_string())
        })?;
        *guard.entry(tenant.clone()).or_insert(0) += 1;
        Ok(())
    }
}

#[async_trait]
impl GraphStore for InMemoryGraphStore {
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
        let mut guard = self.write()?;
        let data = guard.entry(tenant.clone()).or_default();
        for entity in entities {
            let key = entity.key.trim().to_string();
            let slot = data
                .entities
                .entry(key.clone())
                .or_insert_with(|| StoredEntity {
                    name: entity.name.clone(),
                    entity_type: entity.entity_type.clone(),
                    aliases: Vec::new(),
                });
            // Last writer wins on the display fields; aliases merge, because a
            // re-ingest that happens not to know an alias must not delete it.
            slot.name = entity.name.clone();
            slot.entity_type = entity.entity_type.clone();
            let merged: BTreeSet<String> = slot
                .aliases
                .iter()
                .cloned()
                .chain(entity.aliases.iter().cloned())
                .collect();
            slot.aliases = merged.into_iter().collect();

            let mentions = data.mentions.entry(key).or_default();
            for chunk in entity.mentions {
                mentions.insert(chunk.0, document_id.clone());
            }
        }
        drop(guard);
        self.bump(tenant)
    }

    async fn relate(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        relations: Vec<RelationUpsert>,
    ) -> Result<(), PortError> {
        let mut guard = self.write()?;
        let data = guard.entry(tenant.clone()).or_default();
        // Whole-batch validation: a relation to an entity this tenant does not have
        // cannot be stored, and storing half a batch would leave a relation set no
        // extraction could explain.
        for relation in &relations {
            for key in [&relation.src_key, &relation.dst_key] {
                if !data.entities.contains_key(key) {
                    return Err(PortError::Malformed(format!(
                        "relation endpoint `{key}` does not exist in this tenant"
                    )));
                }
            }
        }
        for relation in relations {
            let stored = StoredRelation {
                src_key: relation.src_key.clone(),
                dst_key: relation.dst_key.clone(),
                relation_type: relation.relation_type.clone(),
                weight: relation.weight,
                chunk_id: relation.chunk_id,
                document_id: document_id.clone(),
            };
            let duplicate = data.relations.iter().any(|existing| {
                existing.src_key == stored.src_key
                    && existing.dst_key == stored.dst_key
                    && existing.relation_type == stored.relation_type
                    && existing.document_id == stored.document_id
            });
            if !duplicate {
                data.relations.push(stored);
            }
        }
        drop(guard);
        self.bump(tenant)
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<(), PortError> {
        let mut guard = self.write()?;
        if let Some(data) = guard.get_mut(tenant) {
            // Mentions first, so the orphan sweep below sees the truth.
            for mentions in data.mentions.values_mut() {
                mentions.retain(|_, owner| owner != document_id);
            }
            data.mentions.retain(|_, mentions| !mentions.is_empty());
            data.relations
                .retain(|relation| &relation.document_id != document_id);
            // An entity nothing mentions any more is an orphan: it is not a fact
            // any more, and leaving it would keep its relations alive as paths.
            let live: BTreeSet<String> = data.mentions.keys().cloned().collect();
            data.entities.retain(|key, _| live.contains(key));
        }
        drop(guard);
        self.bump(tenant)
    }

    async fn load_tenant_graph(&self, scope: &DocumentScope) -> Result<TenantGraph, PortError> {
        let tenant = scope.tenant();
        let visible: BTreeSet<String> = scope
            .document_ids()
            .iter()
            .map(ToString::to_string)
            .collect();
        let data = self.read(tenant, |data| data.clone()).unwrap_or_default();

        // 1. Mentions inside the scope, per entity key. An entity whose every
        //    mention is out of scope is not in this graph at all, which is the rule
        //    that makes "no path through an out-of-scope document" true.
        let mut scoped_mentions: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for (key, chunks) in &data.mentions {
            let in_scope: BTreeSet<String> = chunks
                .iter()
                .filter(|(_, owner)| visible.contains(&owner.0))
                .map(|(chunk, _)| chunk.clone())
                .collect();
            if !in_scope.is_empty() {
                scoped_mentions.insert(key.as_str(), in_scope);
            }
        }

        // 2. Intern keys in sorted order, so a node id is a function of the scoped
        //    row set rather than of insertion order. Only keys with in-scope
        //    mentions are interned.
        let mut map = NodeMap::new();
        for key in scoped_mentions.keys() {
            if map.intern(key).is_none() {
                return Err(PortError::Malformed(format!(
                    "entity key `{key}` does not fit the dense node numbering"
                )));
            }
        }

        // 3. Edges inside the scope. Both halves of the filter matter: dropping a
        //    relation whose source document is out of scope, and dropping one whose
        //    endpoint is not itself in the graph, are different rules and both are
        //    needed for an out-of-scope relation not to become a path.
        let mut edges: Vec<Edge> = Vec::new();
        for relation in &data.relations {
            if !visible.contains(&relation.document_id.0) {
                continue;
            }
            let (Some(src), Some(dst)) =
                (map.id_of(&relation.src_key), map.id_of(&relation.dst_key))
            else {
                continue;
            };
            edges.push(Edge::new(src, dst, relation.weight.max(0.0)));
        }
        let csr = Csr::from_edges(map.len(), &edges);

        let mentions: Vec<EntityMentions> = (0..map.len() as u32)
            .filter_map(|id| {
                let key = map.key_of(id)?;
                let chunks = scoped_mentions.get(key)?;
                Some(EntityMentions {
                    key: key.to_string(),
                    chunk_ids: chunks.iter().cloned().map(ChunkId).collect(),
                })
            })
            .collect();

        Ok(TenantGraph::new(map, csr, mentions))
    }

    async fn graph_version(&self, tenant: &TenantId) -> Result<u64, PortError> {
        let guard = self.versions.read().map_err(|_| {
            PortError::Unavailable("in-memory graph store lock poisoned".to_string())
        })?;
        Ok(guard.get(tenant).copied().unwrap_or(0))
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
        // The scoped graph is the universe: an entity with no in-scope mention is
        // not a seed for this caller, whatever its name matches.
        let graph = self.load_tenant_graph(scope).await?;
        let tenant = scope.tenant();
        let data = self
            .read(tenant, |data| data.entities.clone())
            .unwrap_or_default();
        let mut out: Vec<(String, f32)> = Vec::new();
        for key in graph.map().keys() {
            let Some(stored) = data.get(key) else {
                continue;
            };
            let mut hits = 0.0f32;
            for term in &terms {
                let name_matches = stored.name.to_lowercase().contains(term);
                let alias_matches = stored
                    .aliases
                    .iter()
                    .any(|alias| alias.to_lowercase().contains(term));
                if name_matches || alias_matches {
                    hits += 1.0;
                }
            }
            if hits > 0.0 {
                out.push((key.clone(), hits / terms.len() as f32));
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
        Ok(self
            .load_tenant_graph(scope)
            .await?
            .chunks_for(key)
            .to_vec())
    }
}
