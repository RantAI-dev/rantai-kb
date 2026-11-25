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
//! they share *any* term, scored by BM25. The legacy SurrealDB adapter's `@@`
//! operator was boolean AND, so a natural-language question containing
//! "siapakah" or "?" returned zero hits — nDCG@10 0.414 against 0.744 for an
//! independent BM25 (`docs/findings-bm25-collapse.md`). An adapter whose
//! backend is AND-only says so through [`IndexCapabilities::sparse`] instead of
//! the caller guessing, and the contract suite skips the OR-semantics test for
//! it with the reason printed.

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
    /// Keyword arm input, when it should run.
    pub sparse: Option<KeywordQuery>,
    /// Candidates fetched per arm before fusion. Qdrant's prefetch limit.
    pub per_arm_k: usize,
    /// Candidates returned after fusion.
    pub k: usize,
    /// How the arms are combined.
    pub fusion: Fusion,
}

/// A keyword query in both shapes an index might need.
///
/// An index whose keyword arm is native sparse vectors consumes [`vector`]. An
/// index with its own text analyzer consumes [`terms`], because it will
/// re-analyse the text with *its* rules — and those rules are the thing
/// [`IndexCapabilities::sparse`] is reporting on. Carrying both is what lets one
/// port serve more than one backend without any of them re-deriving another's
/// vocabulary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeywordQuery {
    /// Hashed term ids at weight 1.0, from `kb_core::sparse::encode_query`.
    pub vector: SparseVector,
    /// The same terms, folded, lowercased and deduplicated, in first-seen order.
    /// Never empty when [`Self::vector`] is not.
    pub terms: Vec<String>,
}

impl KeywordQuery {
    /// Tokenise `text` once into both shapes, so a document and a query go
    /// through the same tokenizer by construction.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let mut terms: Vec<String> = Vec::new();
        for term in kb_core::sparse::tokenize(text) {
            if !terms.contains(&term) {
                terms.push(term);
            }
        }
        Self {
            vector: kb_core::sparse::encode_query(text),
            terms,
        }
    }

    /// True when there is nothing to search for.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vector.is_empty() && self.terms.is_empty()
    }
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
    /// Stable unit id. The adapter encodes it (UUIDv5 for Qdrant); the engine
    /// only guarantees it is stable and unique per tenant.
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

/// An inclusive chunk/page restriction for a vector-free metadata read
/// (`EVAL-9`). Both halves are ANDed when both are set; a caller that wants one
/// selector leaves the other `None`. A `None` half is "no restriction", so a
/// chunk range without a page range reads every chunk in the index range even
/// when some chunks carry no page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkRange {
    /// Inclusive `(start, end)` chunk indices.
    pub chunks: Option<(u32, u32)>,
    /// Inclusive `(start, end)` zero-based pages.
    pub pages: Option<(u32, u32)>,
}

impl ChunkRange {
    /// Whether one chunk's metadata is inside this range. A chunk with no page
    /// is outside a page range, matching Qdrant's integer range condition.
    #[must_use]
    pub fn allows(&self, meta: &ChunkMeta) -> bool {
        let in_chunks = self
            .chunks
            .is_none_or(|(lo, hi)| meta.chunk_index >= lo && meta.chunk_index <= hi);
        let in_pages = self
            .pages
            .is_none_or(|(lo, hi)| meta.page.is_some_and(|p| p >= lo && p <= hi));
        in_chunks && in_pages
    }
}

/// One chunk's metadata as the outline v2 and content routes need it, with no
/// vectors and no scores. Ordered by `chunk_index`; the spans are absent for
/// chunks ingested before `ING-13`, never a misleading zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMeta {
    pub chunk_index: u32,
    pub page: Option<u32>,
    /// The heading stored on the chunk, when ingest had one.
    pub section: Option<String>,
    pub content: String,
    pub char_start: Option<u32>,
    pub char_end: Option<u32>,
    pub overlap_prefix_len: Option<u32>,
    /// What kind of unit this is. The content route uses it to find figure
    /// refs without a second, search-shaped read. Additive: old callers that
    /// build a `ChunkMeta` by hand default to `Text`.
    #[serde(default)]
    pub chunk_type: ChunkType,
    /// The blob key of a figure crop recorded on the chunk, when it has one.
    #[serde(default)]
    pub asset_key: Option<String>,
}

/// Build [`ChunkMeta`] from a retrieved candidate. Shared by the scoped
/// default of [`RetrievalIndex::list_chunk_meta`] and by fakes.
#[must_use]
pub fn chunk_meta_from_candidate(candidate: &Candidate) -> ChunkMeta {
    let span = |key: &str| {
        candidate
            .metadata
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    };
    ChunkMeta {
        chunk_index: candidate.chunk_index,
        page: candidate.page,
        section: candidate
            .metadata
            .get("section")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        content: candidate.content.clone(),
        char_start: span("charStart"),
        char_end: span("charEnd"),
        overlap_prefix_len: span("overlapPrefixLen"),
        chunk_type: candidate.chunk_type,
        asset_key: candidate
            .metadata
            .get("assetKey")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    }
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
    /// the legacy SurrealDB adapter did; no current adapter declares it.
    AllTerms,
}

/// What this index can actually do. Read by the engine to decide which arms to
/// run and by the contract suite to decide which tests apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexCapabilities {
    /// Width of the dense vectors, or `None` when there is no dense arm.
    ///
    /// `None` means "no dense arm". A backend that *has* the arm but cannot read
    /// its own width back reports the width it was configured with. Collapsing
    /// the two cases into one number would make an adapter skip tests it can
    /// actually run, which is how a contract suite quietly stops testing things.
    pub dense_dims: Option<u32>,
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
            dense_dims: Some(dims),
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
            dense_dims: Some(dims),
            sparse: Some(KeywordSemantics::AnyTerm),
            server_side_fusion: true,
            multivector: false,
            approximate_dense,
        }
    }
}

/// The retrieval port. Implemented by the in-memory reference and by Qdrant,
/// the only production chunk store since 2026-10-25.
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

    /// Retrieve figure units belonging to the specified documents within scope.
    ///
    /// Figure selection logic (matching anchors, captions, deduplication, gating)
    /// lives in the engine (`kb-engine`), not in the adapter. This method provides
    /// the port capability to retrieve all candidate figure units for the
    /// requested documents within the tenant's visible scope.
    async fn figures(
        &self,
        scope: &DocumentScope,
        document_ids: &[DocumentId],
    ) -> Result<Vec<Candidate>, PortError>;

    /// Read the text units that sit on the given zero-based pages, within scope.
    ///
    /// This is the tree arm's second read (DEC-4): the outline names pages, and
    /// the arm needs the chunks on them. A page with no chunk simply contributes
    /// nothing. `per_page` bounds how many units one page contributes, so a
    /// 30-page section cannot flood the candidate pool from one heading.
    ///
    /// The default is an error rather than an empty list: an adapter that has
    /// not implemented the read would otherwise make the tree arm look like it
    /// found nothing, which is exactly the silent degradation AGENTS rule 6
    /// forbids. The engine turns the error into `tree_arm_failed` and returns
    /// the other arms' results.
    async fn fetch_pages(
        &self,
        _scope: &DocumentScope,
        _pages: &[(DocumentId, u32)],
        _per_page: usize,
    ) -> Result<Vec<Candidate>, PortError> {
        Err(PortError::Unavailable(
            "this index does not support reading units by page".to_string(),
        ))
    }

    /// Retrieve all stored units of a single document, ordered by `chunk_index` ascending.
    /// Used by document inspection routes (`GET /v1/documents/:id`).
    async fn ordered_chunks(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<Vec<Candidate>, PortError>;

    /// Read at most `max_units` stored units of one document, ordered by
    /// `chunk_index` ascending, for the document-structure read.
    ///
    /// The default is the honest smallest implementation — [`Self::ordered_chunks`]
    /// followed by a truncation — which is what a fake or a small adapter needs.
    /// A real adapter overrides it and scrolls in windows, so a 2,000-page
    /// document is never one unbounded store round trip. `max_units` is the
    /// caller's memory cap as well as the read cap: the adapter must not read
    /// past it, and a caller that gets exactly `max_units` units cannot tell
    /// whether more exist (the route reports `truncated` when the cap is hit).
    ///
    /// Document visibility is the caller's job, exactly as for
    /// [`Self::ordered_chunks`]: pass a document id only after the tenant and
    /// knowledge-base check.
    async fn document_units(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        max_units: usize,
    ) -> Result<Vec<Candidate>, PortError> {
        let mut units = self.ordered_chunks(tenant, document_id).await?;
        units.truncate(max_units);
        Ok(units)
    }

    /// List one document's chunk metadata, ordered by `chunk_index`, **without
    /// vectors or scores**. The outline v2 (`RET-13`) and the content route
    /// (`RET-14`) use this instead of a search-shaped read.
    ///
    /// Scope is part of the contract: a document outside `scope` is refused by
    /// returning an empty list. The default derives the answer from
    /// [`Self::document_units`] for fakes and small adapters; a real adapter
    /// overrides it with a scroll that asks the store for payloads only.
    async fn list_chunk_meta(
        &self,
        scope: &DocumentScope,
        document: &DocumentId,
    ) -> Result<Vec<ChunkMeta>, PortError> {
        if !scope.allows_document(document) {
            return Ok(Vec::new());
        }
        let units = self
            .document_units(scope.tenant(), document, usize::MAX)
            .await?;
        let mut out: Vec<ChunkMeta> = units.iter().map(chunk_meta_from_candidate).collect();
        out.sort_by_key(|meta| meta.chunk_index);
        Ok(out)
    }

    /// List a bounded slice of one document's chunk metadata, ordered by
    /// `chunk_index` and **without vectors or scores** (`EVAL-9`).
    ///
    /// The reader uses this instead of scrolling the whole document on every
    /// `/content` call. `limit` caps how many chunks are returned, so a caller
    /// can stop at its character budget by advancing the chunk lower bound.
    /// Both halves of `range` are inclusive and ANDed. The scope check is the
    /// same one [`Self::list_chunk_meta`] performs: a document outside `scope`
    /// returns an empty list, and a real adapter must repeat it in its override.
    ///
    /// The default derives the slice from [`Self::list_chunk_meta`] for fakes
    /// and small adapters; an adapter with integer indexes overrides it with a
    /// filtered scroll so it never reads the rest of the document.
    async fn list_chunk_meta_range(
        &self,
        scope: &DocumentScope,
        document: &DocumentId,
        range: ChunkRange,
        limit: usize,
    ) -> Result<Vec<ChunkMeta>, PortError> {
        if !scope.allows_document(document) {
            return Ok(Vec::new());
        }
        let mut out: Vec<ChunkMeta> = self
            .list_chunk_meta(scope, document)
            .await?
            .into_iter()
            .filter(|meta| range.allows(meta))
            .collect();
        if limit > 0 {
            out.truncate(limit);
        }
        Ok(out)
    }

    /// Probe whether the index backend is healthy and responding.
    async fn health_check(&self) -> Result<(), PortError> {
        Ok(())
    }

    /// What this adapter supports. Static, cheap, and safe to call per request.
    fn capabilities(&self) -> IndexCapabilities;
}

/// Order candidates by fused score descending, breaking ties on properties of
/// the *corpus* rather than properties of the ingest.
///
/// Deliberately **not** "the dense arm wins ties". That was the old behaviour:
/// vector and BM25 rank-1 hits both scored 1/61, the vector hit owned position
/// 1, and with a noisy dense arm that pinned Hit@1 at 0 (`docs/eval-baseline.md`
/// §4). A deterministic total order is enough; which arm *wins* is a tuning
/// question the eval should answer, not a property of the port.
///
/// The tie-break is the chunk's text, then its page and index. It used to be
/// `document_id` and `unit_id`, but both contain the document id, which is a
/// fresh UUID on every ingest. Two chunks with the same fused score were
/// therefore ordered by whichever document happened to be inserted first:
/// identical corpora could answer differently after a re-ingest, and the tiny
/// stub gate — whose 8-dimensional embeddings make exact score ties common —
/// flipped one document in and out of rank 1 between runs. Text, page and index
/// are fixed by the corpus, so the same chunks always come out in the same
/// order. Chunks with no text at all fall through to their index, which is the
/// only remaining corpus-stable property.
pub fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by(|a, b| {
        b.scores
            .fused
            .partial_cmp(&a.scores.fused)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.content.cmp(&b.content))
            .then_with(|| a.page.cmp(&b.page))
            .then_with(|| a.chunk_index.cmp(&b.chunk_index))
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
        candidate_with(doc, index, fused, "")
    }

    fn candidate_with(doc: &str, index: u32, fused: f32, content: &str) -> Candidate {
        Candidate {
            unit_id: ChunkId::new_unchecked(format!("{doc}_{index}")),
            document_id: DocumentId::new_unchecked(doc),
            tenant: TenantId::new_unchecked("t"),
            chunk_index: index,
            page: None,
            chunk_type: ChunkType::Text,
            content: content.to_string(),
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

    /// Equal fused scores are ordered by the chunk's text, never by the
    /// document id: that id is a new UUID on every ingest, so a tie-break on it
    /// makes a fresh ingest of the same corpus answer differently.
    #[test]
    fn ties_break_on_content_not_on_the_per_ingest_document_id() {
        // Same three chunks, two different random document ids (and a different
        // input order, because the two arms can hand them back either way).
        let mut first = vec![
            candidate_with("9f2c-uuid", 0, 1.0, "zebra"),
            candidate_with("1a7b-uuid", 7, 1.0, "alpha"),
            candidate_with("4c8d-uuid", 3, 1.0, "mango"),
        ];
        sort_candidates(&mut first);
        let contents: Vec<&str> = first.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(contents, vec!["alpha", "mango", "zebra"]);

        // A re-ingest assigns new ids; the order must not move.
        let mut second = vec![
            candidate_with("0000-uuid", 3, 1.0, "mango"),
            candidate_with("ffff-uuid", 0, 1.0, "zebra"),
            candidate_with("5555-uuid", 7, 1.0, "alpha"),
        ];
        sort_candidates(&mut second);
        let contents: Vec<&str> = second.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(contents, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn identical_text_falls_back_to_chunk_index() {
        let mut v = vec![
            candidate_with("doc_b", 2, 1.0, "same"),
            candidate_with("doc_a", 0, 1.0, "same"),
            candidate_with("doc_c", 1, 1.0, "same"),
        ];
        sort_candidates(&mut v);
        let indices: Vec<u32> = v.iter().map(|c| c.chunk_index).collect();
        assert_eq!(indices, vec![0, 1, 2]);
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
        assert_eq!(caps.dense_dims, Some(1024));
        assert_eq!(caps.sparse, None);
        assert!(!caps.server_side_fusion);
    }

    #[test]
    fn default_fusion_is_rrf_at_sixty() {
        assert_eq!(Fusion::default(), Fusion::Rrf { k: 60 });
    }

    #[test]
    fn a_keyword_query_carries_both_shapes_of_the_same_tokens() {
        let q = KeywordQuery::from_text("Siapakah yang menulis Candy Candy?");
        // Folded, deduplicated, question words present — an adapter with its own
        // analyzer needs the text, not the hashes, and that text must be the
        // tokenizer's output or the two arms disagree about the vocabulary.
        assert_eq!(q.terms, vec!["siapakah", "yang", "menulis", "candy"]);
        assert_eq!(q.vector.len(), 4);
        assert!(q.vector.values.iter().all(|v| *v == 1.0));
        // The hashes must be the ids of exactly those terms, or an adapter using
        // `vector` would be searching for something else entirely.
        for term in &q.terms {
            assert!(
                q.vector.indices.contains(&kb_core::sparse::term_id(term)),
                "{term}"
            );
        }
        assert!(!q.is_empty());
    }

    #[test]
    fn a_query_with_no_word_characters_is_empty_in_both_shapes() {
        let q = KeywordQuery::from_text("?! …");
        assert!(q.is_empty(), "{q:?}");
        assert!(q.vector.is_empty());
        assert!(q.terms.is_empty());
    }

    #[test]
    fn hybrid_capability_constructor_declares_or_semantics() {
        let caps = IndexCapabilities::hybrid(1024, false);
        assert_eq!(caps.sparse, Some(KeywordSemantics::AnyTerm));
        assert!(caps.server_side_fusion);
        assert!(!caps.approximate_dense);
    }
}
