//! The `GraphStore` port: entity, mention and relation persistence plus the
//! scoped reads the graph retrieval arm needs.
//!
//! Storage-agnostic by construction: it speaks entity keys, [`ChunkId`]s,
//! [`DocumentId`]s and [`TenantId`]s, never table names, column lists or
//! surrogate keys. An adapter owns its own id encoding; engine code never
//! formats an identifier for a store.
//!
//! # Why this port exists
//!
//! `docs/architecture-target.md` §11: the entity graph moves out of SurrealDB
//! into three Postgres tables, and the traversal happens in Rust in memory
//! ([`kb_core::graph`]). This is the seam. The engine asks for a tenant's edges
//! under a [`DocumentScope`]; an adapter returns them; the walk is storage-free.
//!
//! # The scope rule, which is the whole contract
//!
//! **A mention or relation whose `document_id` is outside the scope does not
//! exist — not as a fact, and not as a path.**
//!
//! That second half is the part that is easy to get wrong and expensive to get
//! wrong. A scope that filters *results* but not *edges* still leaks: two
//! entities in the caller's documents, connected only by a relation extracted
//! from a document the caller cannot see, become connected in the scoped graph,
//! and the walk routes through them. The caller learns about the existence and
//! the shape of a document it was never shown. So the filter is on the edge set
//! itself, before the CSR is built, and the contract suite tests it directly
//! rather than inferring it from result filtering.
//!
//! [`TenantGraph`] is what an adapter returns for that reason: it is already
//! scoped, already deduplicated by entity key, and already built into the CSR the
//! walk wants. An adapter cannot return a graph it has not filtered, because
//! filtering is the shape of the return value.
//!
//! [`GraphStore::graph_version`] is the cache key. The architecture puts a
//! per-tenant in-memory CSR cache behind a version counter that ingest bumps; a
//! write that does not bump the version is a stale cache, which reads as
//! "the answer changed and nobody knows why", so the contract suite checks the
//! counter on every write shape.

use async_trait::async_trait;
use kb_core::graph::{Csr, ExpandedNode, NodeMap, ScoredNode, WalkOptions};
use serde::{Deserialize, Serialize};

use crate::error::PortError;
use crate::ids::{ChunkId, DocumentId, TenantId};
use crate::scope::DocumentScope;

/// One entity as ingest extracted it, keyed by the key ingest canonicalised.
///
/// `key` is the dedupe identity: two extractions of "Kyoko Mizuki" that resolve
/// to the same key are the same entity, in every document of the tenant, and a
/// re-ingest of the same document must not create a second one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityUpsert {
    /// Canonical key, unique within `(tenant, key)`.
    pub key: String,
    /// Display name, for a future alias/label surface. Not the identity.
    pub name: String,
    /// Extractor's type label (`person`, `work`, `place`, …).
    pub entity_type: String,
    /// Other surface forms of the same entity. Merged, deduplicated, order-stable.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Chunks this entity was mentioned in, in this document. Empty is legal
    /// (an entity extracted from a summary has no single mention).
    #[serde(default)]
    pub mentions: Vec<ChunkId>,
}

/// One relation, as a pair of entity *keys* rather than ids.
///
/// Keys, not ids: the adapter resolves them to rows inside the same tenant, and a
/// relation to an entity that does not exist in this tenant cannot be stored, so
/// `relate` may reject the whole batch rather than inventing a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationUpsert {
    pub src_key: String,
    pub dst_key: String,
    /// Extractor's relation label, normalised by the adapter to its own convention.
    pub relation_type: String,
    /// Non-negative confidence. A zero-weight relation is not a relation: see
    /// [`kb_core::graph`], where a zero weight is an absent edge.
    pub weight: f32,
    /// The chunk the relation came from, when it came from one.
    pub chunk_id: Option<ChunkId>,
}

/// One scoped mention: an entity key, the chunks it was seen in, and the
/// documents those chunks live in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityMentions {
    pub key: String,
    /// Ascending, deduplicated. Ascending so a read is byte-identical across runs
    /// and so a diff of two loads is readable.
    pub chunk_ids: Vec<ChunkId>,
}

/// A tenant's entity graph, already scoped and already built for the walk.
///
/// Owns its [`NodeMap`], so an adapter hands back entity keys and the caller
/// resolves them against the same numbering the scores use.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TenantGraph {
    map: NodeMap,
    csr: Csr,
    mentions: Vec<EntityMentions>,
}

impl TenantGraph {
    /// An empty graph.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A graph from an id map, a CSR and the mention lists, all over the same
    /// numbering.
    #[must_use]
    pub fn new(map: NodeMap, csr: Csr, mentions: Vec<EntityMentions>) -> Self {
        Self { map, csr, mentions }
    }

    /// Entity count (entities *in scope*, which can be fewer than the tenant's).
    #[must_use]
    pub fn entity_count(&self) -> usize {
        self.map.len()
    }

    /// Edge count, counting both directions of an undirected relation.
    #[must_use]
    pub fn arc_count(&self) -> usize {
        self.csr.arc_count()
    }

    /// The adjacency the walk runs on.
    #[must_use]
    pub fn csr(&self) -> &Csr {
        &self.csr
    }

    /// The id<->key map. Keys are in id order, so ids are reproducible for the
    /// same scoped row set.
    #[must_use]
    pub fn map(&self) -> &NodeMap {
        &self.map
    }

    /// Mention lists, in `NodeMap` key order.
    #[must_use]
    pub fn mentions(&self) -> &[EntityMentions] {
        &self.mentions
    }

    /// Chunks mentioned by an entity key, empty when the key is unknown or has no
    /// in-scope mention.
    #[must_use]
    pub fn chunks_for(&self, key: &str) -> &[ChunkId] {
        self.map
            .id_of(key)
            .and_then(|id| self.mentions.get(id as usize))
            .map(|m| m.chunk_ids.as_slice())
            .unwrap_or(&[])
    }

    /// Personalised PageRank over this scoped graph. Pure: no I/O, so the port
    /// method is the adapter's only job and the walk is the engine's.
    #[must_use]
    pub fn ppr(&self, seed_weights: &[(String, f32)], opts: &WalkOptions) -> Vec<ScoredNode> {
        let seeds: Vec<(u32, f32)> = seed_weights
            .iter()
            .filter_map(|(key, weight)| self.map.id_of(key).map(|id| (id, *weight)))
            .collect();
        kb_core::graph::ppr(&self.csr, &seeds, opts)
    }

    /// 1-2 hop expansion from a weighted seed set.
    #[must_use]
    pub fn expand(&self, seed_weights: &[(String, f32)], depth: usize) -> Vec<ExpandedNode> {
        let seeds: Vec<(u32, f32)> = seed_weights
            .iter()
            .filter_map(|(key, weight)| self.map.id_of(key).map(|id| (id, *weight)))
            .collect();
        kb_core::graph::expand(&self.csr, &seeds, depth)
    }

    /// Ranked entity keys with their PPR scores, `(score desc, key asc)`.
    ///
    /// Key ordering is the deterministic tiebreak at the port boundary: two nodes
    /// with the same score must come back in the same order on every adapter, and
    /// comparing strings is the one order every backend agrees on.
    #[must_use]
    pub fn ranked_keys(&self, scored: &[ScoredNode]) -> Vec<(String, f32)> {
        let mut out: Vec<(String, f32)> = scored
            .iter()
            .filter_map(|s| self.map.key_of(s.node).map(|k| (k.to_string(), s.score)))
            .collect();
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        out
    }

    /// Join a ranking against the mention lists: chunks ranked by the score of
    /// the entity that mentions them, best first, deduplicated.
    ///
    /// Deterministic end to end: entities by `(score desc, key asc)`, then chunks
    /// by `(entity score desc, entity key asc, chunk id asc)`. A chunk mentioned by
    /// two entities appears once, under the higher-scoring one, which is why the
    /// dedupe happens after the sort and not before it.
    #[must_use]
    pub fn chunks_ranked_by(&self, scored: &[ScoredNode]) -> Vec<ChunkId> {
        let mut joined: Vec<(f32, String, ChunkId)> = Vec::new();
        for node in scored {
            let Some(key) = self.map.key_of(node.node) else {
                continue;
            };
            for chunk in self.chunks_for(key) {
                joined.push((node.score, key.to_string(), chunk.clone()));
            }
        }
        joined.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
                .then(a.2.0.cmp(&b.2.0))
        });
        let mut out: Vec<ChunkId> = Vec::with_capacity(joined.len());
        let mut last: Option<&ChunkId> = None;
        for (_, _, chunk) in joined {
            if last != Some(&chunk) {
                out.push(chunk);
                last = Some(&out[out.len() - 1]);
            }
        }
        out
    }
}

/// The graph port.
#[async_trait]
pub trait GraphStore: Send + Sync {
    /// Insert or replace entities of one document.
    ///
    /// Dedupe is by `key` within the tenant, so re-ingesting a document updates
    /// rather than duplicates, and a second document mentioning the same entity
    /// adds a mention rather than a second entity. Aliases merge: a re-ingest with
    /// a subset of the previous aliases must not drop the others.
    ///
    /// Every call bumps [`GraphStore::graph_version`], including one that changes
    /// nothing.
    async fn upsert_entities(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        entities: Vec<EntityUpsert>,
    ) -> Result<(), PortError>;

    /// Store relations of one document.
    ///
    /// `src_key` and `dst_key` must already exist in this tenant, from
    /// [`GraphStore::upsert_entities`] or an earlier `relate`. A batch with an
    /// unknown endpoint is rejected whole: storing half of it would leave a
    /// relation set that no extraction could explain.
    ///
    /// A relation is scoped to the document it came from. That is what makes the
    /// scope rule above enforceable: an out-of-scope document's relations are not
    /// in the scoped graph, so they cannot be paths through it.
    async fn relate(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        relations: Vec<RelationUpsert>,
    ) -> Result<(), PortError>;

    /// Delete every mention, relation and orphaned entity of one document.
    ///
    /// An entity left with no mention in any other document is an orphan and goes
    /// with it: an entity nobody in the tenant mentions any more is not a fact, and
    /// leaving it would keep its relations alive as paths. Other documents, and
    /// other tenants' documents with the same id, are untouched. Bumps the version.
    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<(), PortError>;

    /// The tenant's graph, filtered to the scope's documents.
    ///
    /// The returned [`TenantGraph`] is already narrowed: no node, edge or mention
    /// outside the scope is in it, so no walk over it can reach out of scope.
    /// Returns an empty graph for an empty scope rather than erroring.
    async fn load_tenant_graph(&self, scope: &DocumentScope) -> Result<TenantGraph, PortError>;

    /// Monotonic per-tenant version, bumped by every write.
    ///
    /// The in-memory CSR cache is keyed on this: a cache entry whose version
    /// matches the store's is still valid, and any mismatch means reload. `0` means
    /// "nothing ever written", which must compare unequal to any written state.
    async fn graph_version(&self, tenant: &TenantId) -> Result<u64, PortError>;

    /// Seed lookup: entity keys matching `query_terms`, with a match weight.
    ///
    /// Simple name and alias matching for now — the port shape, not a search
    /// engine. The one rule it already has to keep is scope: an entity whose only
    /// in-scope evidence is out of scope does not match. Ordered by
    /// `(weight desc, key asc)`.
    async fn match_entities(
        &self,
        scope: &DocumentScope,
        query_terms: &[String],
    ) -> Result<Vec<(String, f32)>, PortError>;

    /// Mentions of one entity key within the scope, ascending by chunk id.
    ///
    /// An unknown key returns an empty list rather than an error: a caller must
    /// not be able to probe for another tenant's entity by timing an error.
    async fn entity_mentions(
        &self,
        scope: &DocumentScope,
        key: &str,
    ) -> Result<Vec<ChunkId>, PortError>;

    /// Liveness probe for the graph backend.
    async fn health_check(&self) -> Result<(), PortError> {
        Ok(())
    }
}
