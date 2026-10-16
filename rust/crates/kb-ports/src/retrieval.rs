//! `RetrievalIndex`: the one retrieval port.
//!
//! Storage-agnostic by construction: it speaks in domain ids ([`ChunkId`],
//! [`DocumentId`], [`TenantId`]) and in vectors, never in record ids, `IN`
//! lists or index names. The adapter owns the encoding; engine code never
//! formats an identifier for a store.
//!
//! # Scope is a parameter, not an option
//!
//! [`RetrievalIndex::search`], [`RetrievalIndex::fetch`] and
//! [`RetrievalIndex::neighbours`] all take a [`DocumentScope`]. There is
//! deliberately no unscoped method: `bm25_search_global` returned every
//! tenant's best matches and let the caller filter afterwards, which is the
//! leak the red-team matrix exists to catch (`docs/eval-baseline.md` §2).
//!
//! # Why keyword semantics are a declared capability, not an assumption
//!
//! Sparse keyword search is OR-of-terms: a question matches a document when
//! they share *any* term, scored by BM25. SurrealDB's `@@` operator is
//! boolean AND, so a natural-language question containing "siapakah" or "?"
//! returns zero hits — nDCG@10 0.414 against 0.744 for an independent BM25
//! (`docs/findings-bm25-collapse.md`). An adapter whose backend is AND-only
//! says so through [`IndexCapabilities::sparse`] instead of the caller
//! guessing, and the contract suite skips the OR-semantics test for it with the
//! reason printed.

use async_trait::async_trait;
use kb_core::sparse::SparseVector;
use serde::{Deserialize, Serialize};

use crate::error::PortError;
use crate::ids::{ChunkId, DocumentId, KnowledgeBaseId, TenantId};
use crate::scope::DocumentScope;
use crate::types::ChunkType;

/// What a retrieval arm is being asked for.
#[derive(Debug, Clone, Default)]
pub struct HybridQuery {
    /// Query embedding, when the dense arm should run.
    pub dense: Option<Vec<f32>>,
    /// Sparse query vector, from `kb_core::sparse::encode_query`. Keyword
    /// terms are hashed to u32 ids, never passed as text, so an adapter cannot
    /// re-analyse them differently from the documents.
    pub sparse: Option<SparseVector>,
    /// Candidates fetched per arm before fusion. Qdrant's prefetch limit.
    pub per_arm_k: usize,
    /// Candidates returned after fusion.
    pub k: usize,
    /// How the arms are combined.
    pub fusion: Fusion,
}

/// How two arm result lists become one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fusion {
    /// Reciprocal Rank Fusion: `sum over arms of 1 / (k + rank)`, rank 1-based.
    Rrf {
        /// The RRF constant. 60 is the value from the original paper and what
        /// the current engine uses.
        k: u32,
    },
    /// Do not fuse: return the arms' concatenation, arm order first. Used when
    /// exactly one arm is present, and by adapters that must not reorder.
    None,
}

impl Default for Fusion {
    fn default() -> Self {
        Self::Rrf { k: 60 }
    }
}

/// Per-arm scores and the fused score, for observability and for deciding
/// whether a hit is worth keeping.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ArmScores {
    /// Cosine (or dot) similarity from the dense arm, when it ran.
    pub dense: Option<f32>,
    /// BM25 score from the sparse arm, when it ran.
    pub sparse: Option<f32>,
    /// The score the candidate is ordered by. With [`Fusion::Rrf`] this is the
    /// RRF score, not a similarity, and is not comparable across queries.
    pub fused: f32,
}

/// Per-arm 0-based ranks inside their own arm's candidate list, plus the final
/// position. `None` for an arm that did not produce this candidate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArmRanks {
    pub dense: Option<u32>,
    pub sparse: Option<u32>,
    /// Final 0-based position in the returned list.
    pub fused: u32,
}

/// One retrieval unit as the engine asked for it to be stored. Everything the
/// three phases of retrieval need is here: the text, its provenance, and the
/// two arm vectors. An adapter stores what its backend can express and reports
/// the rest through [`IndexCapabilities`].
#[derive(Debug, Clone)]
pub struct IndexUnit {
    /// Stable unit id. The adapter encodes it (UUIDv5 for Qdrant, a record id
    /// for SurrealDB); the engine only guarantees it is stable and unique per
    /// tenant.
    pub id: ChunkId,
    pub document_id: DocumentId,
    pub tenant: TenantId,
    /// Knowledge bases this document belongs to, for scope filtering without a
    /// round trip to Postgres.
    pub knowledge_base_id: Option<KnowledgeBaseId>,
    pub category: Option<String>,
    pub chunk_index: u32,
    pub page: Option<u32>,
    pub chunk_type: ChunkType,
    pub content: String,
    /// Text actually embedded (`title > section path > chunk`), kept next to the
    /// content so a reranker or a citation can show what was embedded.
    pub contextual_prefix: Option<String>,
    /// Opaque JSON the caller wants returned verbatim.
    pub metadata: serde_json::Value,
    /// Dense vector. Empty means "this unit has no dense representation", which
    /// is legal for a table-cells unit that only exists in the sparse index.
    pub dense: Vec<f32>,
    /// Sparse vector from `kb_core::sparse::encode_document`.
    pub sparse: SparseVector,
}

/// One retrieved unit, with the scores that put it where it is.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Same id space as [`IndexUnit::id`], as this adapter encodes it. Used to
    /// `fetch` the full payload back when the caller needs more than is
    /// returned here.
    pub unit_id: ChunkId,
    pub document_id: DocumentId,
    pub tenant: TenantId,
    pub chunk_index: u32,
    pub page: Option<u32>,
    pub chunk_type: ChunkType,
    pub content: String,
    pub contextual_prefix: Option<String>,
    pub metadata: serde_json::Value,
    pub scores: ArmScores,
    pub ranks: ArmRanks,
}

/// How the backend's keyword arm matches. `None` means the adapter has no
/// keyword arm at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeywordSemantics {
    /// OR-of-terms: any shared term matches, BM25 scores it. What Qdrant
    /// sparse vectors with `Modifier::Idf` do.
    AnyTerm,
    /// Boolean AND: every query term must be present, or no hit at all. What
    /// SurrealDB's `@@` does. Declared honestly by the legacy adapter.
    AllTerms,
}

/// What this index can actually do. Read by the engine to decide which arms to
/// run and by the contract suite to decide which tests apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexCapabilities {
    /// Dimension of the dense vectors, or 0 when the arm is absent.
    pub dense_dims: u32,
    /// Keyword arm and its matching semantics. `None` = no keyword arm.
    pub sparse: Option<KeywordSemantics>,
    /// The backend fuses and ranks internally (Qdrant's prefetch + RRF), so the
    /// engine does not need a second round trip to merge arms.
    pub server_side_fusion: bool,
    /// Native multi-vector (MaxSim / late interaction) support.
    pub multivector: bool,
    /// True when the dense arm is approximate (HNSW, IVF, quantised) rather than
    /// an exact scan. The contract suite uses it to decide whether to demand an
    /// exact top-k or only a recall floor: an approximate index is allowed to
    /// miss a few, an exact one is not.
    pub approximate_dense: bool,
}

impl IndexCapabilities {
    /// A dense-only, no-keyword, no-fusion, exact index.
    #[must_use]
    pub const fn dense_only(dims: u32) -> Self {
        Self {
            dense_dims: dims,
            sparse: None,
            server_side_fusion: false,
            multivector: false,
            approximate_dense: false,
        }
    }

    /// A hybrid index: dense plus OR-of-terms keyword, fusing server-side.
    #[must_use]
    pub const fn hybrid(dims: u32, approximate_dense: bool) -> Self {
        Self {
            dense_dims: dims,
            sparse: Some(KeywordSemantics::AnyTerm),
            server_side_fusion: true,
            multivector: false,
            approximate_dense,
        }
    }
}

/// The retrieval port. Implemented by the in-memory reference, by Qdrant, and —
/// during migration — by the legacy SurrealDB store.
///
/// Read methods take a [`DocumentScope`] and must never return a unit outside
/// it. That is the whole contract, and it is what the contract suite exists to
/// prove for each adapter rather than trusting.
#[async_trait]
pub trait RetrievalIndex: Send + Sync {
    /// Insert or replace units. Ids are deterministic, so re-ingesting the same
    /// document overwrites instead of duplicating. An adapter must not leave a
    /// stale tail: `delete_document` before a shrinking re-ingest, or make
    /// upsert authoritative per document.
    async fn upsert(&self, units: Vec<IndexUnit>) -> Result<(), PortError>;

    /// Delete every unit of one document, within one tenant. Returns how many
    /// units went. Must never touch another tenant's units.
    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<u64, PortError>;

    /// Hybrid search, always scoped. Returns at most `query.k` candidates,
    /// ordered by fused score, with ties broken deterministically (see
    /// [`sort_candidates`]).
    async fn search(
        &self,
        query: &HybridQuery,
        scope: &DocumentScope,
    ) -> Result<Vec<Candidate>, PortError>;

    /// Read units back by id, still scoped. An id outside the scope returns
    /// nothing rather than an error: a caller must not be able to probe for the
    /// existence of another tenant's unit by timing an error.
    async fn fetch(
        &self,
        scope: &DocumentScope,
        ids: &[ChunkId],
    ) -> Result<Vec<Candidate>, PortError>;

    /// Adjacent chunks of one document, by chunk index, within scope. Used by
    /// the small-to-big expansion phase.
    ///
    /// "Adjacent" means index ± 1 of an anchor and nothing else: a caller must
    /// be able to bound the expansion without trusting the adapter, and an
    /// implementation that returned the whole document would make a 3,000-chunk
    /// page cost 3,000 unit reads. The anchors themselves are excluded, since a
    /// caller cannot tell its own hit from its expansion otherwise.
    async fn neighbours(
        &self,
        scope: &DocumentScope,
        document_id: &DocumentId,
        chunk_indices: &[u32],
    ) -> Result<Vec<Candidate>, PortError>;

    /// What this adapter supports. Static, cheap, and safe to call per request.
    fn capabilities(&self) -> IndexCapabilities;
}

/// Order candidates by fused score descending, breaking ties on document id
/// then chunk index, then unit id.
///
/// Deliberately **not** "the dense arm wins ties". That was the old behaviour:
/// vector and BM25 rank-1 hits both scored 1/61, the vector hit owned position
/// 1, and with a noisy dense arm that pinned Hit@1 at 0 (`docs/eval-baseline.md`
/// §4). A deterministic total order is enough; which arm *wins* is a tuning
/// question the eval should answer, not a property of the port.
pub fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by(|a, b| {
        b.scores
            .fused
            .partial_cmp(&a.scores.fused)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.document_id.as_str().cmp(b.document_id.as_str()))
            .then_with(|| a.chunk_index.cmp(&b.chunk_index))
            .then_with(|| a.unit_id.as_str().cmp(b.unit_id.as_str()))
    });
    for (i, candidate) in candidates.iter_mut().enumerate() {
        candidate.ranks.fused = i as u32;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn candidate(doc: &str, index: u32, fused: f32) -> Candidate {
        Candidate {
            unit_id: ChunkId::new_unchecked(format!("{doc}_{index}")),
            document_id: DocumentId::new_unchecked(doc),
            tenant: TenantId::new_unchecked("t"),
            chunk_index: index,
            page: None,
            chunk_type: ChunkType::Text,
            content: String::new(),
            contextual_prefix: None,
            metadata: serde_json::Value::Null,
            scores: ArmScores {
                dense: None,
                sparse: None,
                fused,
            },
            ranks: ArmRanks::default(),
        }
    }

    #[test]
    fn ties_break_on_document_id_then_chunk_index_not_on_insertion() {
        let mut v = vec![
            candidate("doc_b", 0, 1.0),
            candidate("doc_a", 7, 1.0),
            candidate("doc_a", 3, 1.0),
        ];
        sort_candidates(&mut v);
        let ids: Vec<String> = v
            .iter()
            .map(|c| format!("{}_{}", c.document_id, c.chunk_index))
            .collect();
        assert_eq!(ids, vec!["doc_a_3", "doc_a_7", "doc_b_0"]);
    }

    #[test]
    fn fused_score_sorts_before_the_tie_break() {
        let mut v = vec![candidate("doc_a", 0, 0.5), candidate("doc_z", 9, 0.9)];
        sort_candidates(&mut v);
        assert_eq!(v[0].document_id.as_str(), "doc_z");
    }

    #[test]
    fn final_ranks_are_written_back() {
        let mut v = vec![
            candidate("doc_a", 0, 1.0),
            candidate("doc_b", 0, 1.0),
            candidate("doc_c", 0, 1.0),
        ];
        sort_candidates(&mut v);
        assert_eq!(
            v.iter().map(|c| c.ranks.fused).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn dense_only_capabilities_report_no_keyword_arm() {
        let caps = IndexCapabilities::dense_only(1024);
        assert_eq!(caps.dense_dims, 1024);
        assert_eq!(caps.sparse, None);
        assert!(!caps.server_side_fusion);
    }

    #[test]
    fn default_fusion_is_rrf_at_sixty() {
        assert_eq!(Fusion::default(), Fusion::Rrf { k: 60 });
    }
}
