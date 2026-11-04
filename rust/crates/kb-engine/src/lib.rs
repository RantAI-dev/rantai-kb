//! Retrieval and ingest orchestration.
//!
//! Owns the retrieval pipeline:
//! 1. Parallel vector & BM25 search.
//! 2. Reciprocal Rank Fusion.
//! 3. Drop vector-only hits below `min_similarity`.
//! 4. Rerank (failures degrade to fused order).
//! 5. Cut to `max_chunks`.
//! 6. Weave neighbour chunks beside anchors.
//! 7. Append figures.
//! 8. Enrich document titles via `find_alive_meta_by_ids`.
//! 9. Build context string and numbered sources with `kb-core`.
//!
//! Also owns the ingest worker (`ingest`), which drives one claimed job from a
//! stored file through extraction, chunking, embedding and storage. Both go
//! through `kb-ports`; neither names an adapter.

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{
    DocumentScope, DocumentStore, Embedder, EngineError, FigureGate, PortError, Reranker,
    RetrievalIndex,
};

#[cfg(any(test, feature = "test-utils"))]
pub mod fakes;
pub mod global_arm;
pub mod graph_arm;
pub mod ingest;
pub mod page_arm;
pub mod retrieval;
pub mod timing;

/// The reference `RetrievalIndex`. Under `test-utils` so an adapter's contract
/// test can depend on it as a control; it is also the implementation the engine
/// will use once the Qdrant swap lands.
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_index;

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory_index::InMemoryIndex;

/// The reference `GraphStore`. Same shape as [`InMemoryIndex`]: under
/// `test-utils` so an adapter's contract test can use it as a control, and
/// available as the implementation the engine will use once the Postgres graph
/// tables are switched on.
#[cfg(any(test, feature = "test-utils"))]
pub mod in_memory_graph_store;

#[cfg(any(test, feature = "test-utils"))]
pub use in_memory_graph_store::InMemoryGraphStore;

#[cfg(test)]
mod boost_tests;
#[cfg(test)]
mod global_arm_tests;
#[cfg(test)]
mod graph_arm_tests;
#[cfg(all(test, feature = "test-utils"))]
mod in_memory_graph_store_tests;
#[cfg(all(test, feature = "test-utils"))]
mod in_memory_index_tests;
#[cfg(test)]
mod ingest_tests;
#[cfg(test)]
mod page_arm_tests;
#[cfg(test)]
mod tests;

pub use global_arm::{GlobalArm, GlobalArmConfig, GlobalRanking, fuse_global};
pub use graph_arm::{GraphArm, GraphArmConfig, GraphCache, GraphRanking};
pub use ingest::{
    CancelToken, INGEST_CHUNKING, IngestSettings, IngestWorker, JobOutcome, reap_failed_job_uploads,
};
pub use page_arm::{
    MergeOutcome, PageArm, PageArmConfig, PageCaption, PageCaptionConfig, PageIngest, PageOutcome,
    PageRerankConfig, PageRerankContext, PageTextSemantics, PageVision, PageVisionContext,
    PageVisionRerankConfig, RankedPage,
};
pub use retrieval::{
    AdaptiveStats, BoostStats, FloorScoreKind, FloorStats, RetrievalOptions, RetrievalOutcome,
    apply_boosts, apply_boosts_now, enrich_titles, execute_pipeline,
};
pub use timing::{Clock, ManualClock, StageTimings, Stopwatch, SystemClock};

/// Tunable engine knobs.
#[derive(Debug, Clone)]
pub struct EngineSettings {
    /// Candidates fetched per arm before fusion (`KB_RERANK_INITIAL_K`).
    pub candidate_pool_size: u32,
    /// Maximum candidates passed to the reranker (`KB_RERANK_CANDIDATE_LIMIT`); 0 = all.
    pub rerank_candidate_limit: u32,
    /// Character budget per candidate (`KB_RERANK_CHAR_BUDGET`); 0 = full text.
    pub rerank_char_budget: usize,
    /// Per-request reranker timeout in ms (`KB_RERANK_TIMEOUT_MS`).
    pub rerank_timeout_ms: u64,
    /// Chunks kept by the reranker (`KB_RERANK_FINAL_K`).
    pub rerank_final_k: u32,
    /// Default chunk cap when the request has none (`KB_DEFAULT_MAX_CHUNKS`).
    pub max_chunks: u32,
    /// Minimum similarity score for vector-only hits (`KB_MIN_SIMILARITY`).
    pub min_similarity: f32,
    /// Default relevance floor (`KB_MIN_SCORE`). `None` means no floor, which
    /// is rank-identical to every release before RET-2. A request's `minScore`
    /// overrides this for that request.
    pub min_score: Option<f32>,
    /// Chunks on either side of an anchor (`KB_NEIGHBOUR_WINDOW`).
    pub neighbour_window: u32,
    /// Figures kept per answer (`KB_FIGURE_MAX_PER_ANSWER`).
    pub figure_limit: u32,
    /// Query expansion on/off (`KB_QUERY_EXPANSION_ENABLED`).
    pub expansion_enabled: bool,
    /// Paraphrases requested (`KB_QUERY_EXPANSION_PARAPHRASES`).
    pub expansion_paraphrases: u32,
    /// BM25 arm on/off (`KB_HYBRID_BM25_ENABLED`). When off the engine runs
    /// the vector arm alone and does not record a degrade.
    pub hybrid_bm25_enabled: bool,
    /// Whether reranker is enabled (`KB_RERANK_ENABLED`). Default false.
    pub rerank_enabled: bool,
    /// The RRF constant that fuses the vector and keyword arms (`KB_RRF_K`).
    /// 60 won live with the local embedder (commit 4c01a7d); 2 won offline
    /// with e5-large on MIRACL-id, so it is a per-deployment choice.
    pub rrf_k: u32,
    /// Adaptive arm choice (`KB_ADAPTIVE_ARMS`, RET-5). Off by default: every
    /// search runs the same arms it always has. A request's `adaptiveArms`
    /// overrides this for that request.
    pub adaptive_arms: bool,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            candidate_pool_size: 20,
            rrf_k: 60,
            rerank_candidate_limit: 0,
            rerank_char_budget: 0,
            rerank_timeout_ms: 20_000,
            rerank_final_k: 5,
            max_chunks: 8,
            min_similarity: 0.30,
            min_score: None,
            neighbour_window: 1,
            figure_limit: 3,
            expansion_enabled: false,
            expansion_paraphrases: 3,
            hybrid_bm25_enabled: true,
            rerank_enabled: false,
            adaptive_arms: false,
        }
    }
}

/// Query expander port. The engine calls it only when expansion is enabled.
/// Its implementation lives in `kb-inference`.
#[async_trait]
pub trait QueryExpander: Send + Sync {
    async fn expand(&self, query: &str) -> Result<Vec<String>, PortError>;
}

/// No-op expander (returns the original query unchanged).
pub struct NoopExpander;

#[async_trait]
impl QueryExpander for NoopExpander {
    async fn expand(&self, query: &str) -> Result<Vec<String>, PortError> {
        Ok(vec![query.trim().to_string()])
    }
}

#[derive(Clone)]
pub struct EngineInputs {
    pub embedder: Arc<dyn Embedder>,
    pub index: Arc<dyn RetrievalIndex>,
    pub reranker: Arc<dyn Reranker>,
    pub document_store: Arc<dyn DocumentStore>,
    pub query_expander: Arc<dyn QueryExpander>,
    pub figure_gate: Option<Arc<dyn FigureGate>>,
    /// The entity-graph retrieval arm, or `None` when it is off. Owns its own
    /// scoped-graph cache, so the cache is per-process state rather than engine
    /// settings.
    pub graph_arm: Option<Arc<GraphArm>>,
    /// The global (graph community) arm, or `None` when `KB_GLOBAL_ARM` is off.
    /// `None` leaves the pipeline byte-identical to the one before `DEC-5`.
    pub global_arm: Option<Arc<GlobalArm>>,
    /// The page-image arm, or `None` when it is off.
    pub page_arm: Option<Arc<PageArm>>,
}

/// Engine handle — cheap to clone (all state behind `Arc`s).
#[derive(Clone)]
pub struct Engine {
    pub settings: EngineSettings,
    pub inputs: EngineInputs,
    /// What the pipeline times itself against. Behind an `Arc` because it is
    /// shared by every in-flight request and is the one piece of engine state
    /// a test may want to replace.
    pub clock: Arc<dyn Clock>,
}

impl Engine {
    #[must_use]
    pub fn new(settings: EngineSettings, inputs: EngineInputs) -> Self {
        Self {
            settings,
            inputs,
            clock: Arc::new(SystemClock),
        }
    }

    /// Build an engine whose stages are measured against `clock``.
    ///
    /// Production passes the real clock; a test passes a [`ManualClock`] and
    /// asserts the stage accounting without sleeping through it.
    #[must_use]
    pub fn with_clock(
        settings: EngineSettings,
        inputs: EngineInputs,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            settings,
            inputs,
            clock,
        }
    }

    /// Build an engine with the default no-op expander.
    #[must_use]
    pub fn with_settings_and_inputs(
        settings: EngineSettings,
        embedder: Arc<dyn Embedder>,
        index: Arc<dyn RetrievalIndex>,
        reranker: Arc<dyn Reranker>,
        document_store: Arc<dyn DocumentStore>,
    ) -> Self {
        let inputs = EngineInputs {
            embedder,
            index,
            reranker,
            document_store,
            query_expander: Arc::new(NoopExpander),
            figure_gate: None,
            graph_arm: None,
            global_arm: None,
            page_arm: None,
        };
        Self::new(settings, inputs)
    }

    /// Retrieve sources for `query`. The caller passes the resolved
    /// `DocumentScope` (built by a `DocumentStore`).
    pub async fn retrieve(
        &self,
        query: &str,
        scope: &DocumentScope,
        request_max_chunks: Option<u32>,
    ) -> Result<RetrievalOutcome, EngineError> {
        self.retrieve_with_floor(query, scope, request_max_chunks, None)
            .await
    }

    /// [`Self::retrieve`] with optional recency/authority boosts (`RET-4`).
    ///
    /// `request_boost` is off when `None` or when both of its parts are `None`;
    /// either way the pipeline never computes a factor or re-sorts, so the
    /// answer is rank-identical to a request that never sent the field.
    pub async fn retrieve_with_boost(
        &self,
        query: &str,
        scope: &DocumentScope,
        request_max_chunks: Option<u32>,
        request_min_score: Option<f32>,
        request_boost: Option<kb_core::BoostConfig>,
    ) -> Result<RetrievalOutcome, EngineError> {
        self.retrieve_inner(
            query,
            scope,
            request_max_chunks,
            request_min_score,
            request_boost,
            None,
        )
        .await
    }

    /// [`Self::retrieve`] with a per-request relevance floor.
    ///
    /// `request_min_score` wins over the server default (`KB_MIN_SCORE`,
    /// [`EngineSettings::min_score`]); `None` means "use the default", which is
    /// itself `None` unless an operator set one.
    pub async fn retrieve_with_floor(
        &self,
        query: &str,
        scope: &DocumentScope,
        request_max_chunks: Option<u32>,
        request_min_score: Option<f32>,
    ) -> Result<RetrievalOutcome, EngineError> {
        self.retrieve_inner(
            query,
            scope,
            request_max_chunks,
            request_min_score,
            None,
            None,
        )
        .await
    }

    /// [`Self::retrieve_with_floor`] with a per-request adaptive-arm override.
    ///
    /// `request_adaptive_arms` wins over the server default
    /// (`KB_ADAPTIVE_ARMS`, [`EngineSettings::adaptive_arms`]). `None` means
    /// "use the default", so a caller that has never heard of the field keeps
    /// the operator's choice.
    pub async fn retrieve_with_request(
        &self,
        query: &str,
        scope: &DocumentScope,
        request_max_chunks: Option<u32>,
        request_min_score: Option<f32>,
        request_adaptive_arms: Option<bool>,
    ) -> Result<RetrievalOutcome, EngineError> {
        self.retrieve_inner(
            query,
            scope,
            request_max_chunks,
            request_min_score,
            None,
            request_adaptive_arms,
        )
        .await
    }

    /// Every per-request option at once: floor (`RET-2`), boosts (`RET-4`) and
    /// the adaptive-arm override (`RET-5`). The narrower `retrieve_*` methods
    /// are this with the other options left at their defaults.
    pub async fn retrieve_inner(
        &self,
        query: &str,
        scope: &DocumentScope,
        request_max_chunks: Option<u32>,
        request_min_score: Option<f32>,
        request_boost: Option<kb_core::BoostConfig>,
        request_adaptive_arms: Option<bool>,
    ) -> Result<RetrievalOutcome, EngineError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(RetrievalOutcome {
                chunks: Vec::new(),
                sources: Vec::new(),
                context: String::new(),
                degrades: Vec::new(),
                note: None,
                floor: None,
                boost: None,
                adaptive: None,
                global_arm: None,
                timings: StageTimings::default(),
            });
        }

        let max_chunks = request_max_chunks
            .map(|mc| mc.clamp(1, 50))
            .unwrap_or(self.settings.max_chunks);

        let opts = RetrievalOptions {
            max_chunks,
            min_similarity: self.settings.min_similarity,
            min_score: request_min_score.or(self.settings.min_score),
            candidate_pool_size: self.settings.candidate_pool_size,
            rrf_k: self.settings.rrf_k,
            rerank_candidate_limit: self.settings.rerank_candidate_limit,
            rerank_char_budget: self.settings.rerank_char_budget,
            rerank_timeout_ms: self.settings.rerank_timeout_ms,
            rerank_final_k: self.settings.rerank_final_k,
            neighbour_window: self.settings.neighbour_window,
            figure_limit: self.settings.figure_limit,
            expansion_enabled: self.settings.expansion_enabled,
            expansion_paraphrases: self.settings.expansion_paraphrases,
            hybrid_bm25_enabled: self.settings.hybrid_bm25_enabled,
            figure_gate: self.inputs.figure_gate.clone(),
            rerank_enabled: self.settings.rerank_enabled,
            graph_arm: self.inputs.graph_arm.clone(),
            global_arm: self.inputs.global_arm.clone(),
            page_arm: self.inputs.page_arm.clone(),
            boost: request_boost,
            boost_now: None,
            adaptive_arms: request_adaptive_arms.unwrap_or(self.settings.adaptive_arms),
        };

        execute_pipeline(
            &self.inputs.document_store,
            &self.inputs.index,
            &self.inputs.embedder,
            &self.inputs.reranker,
            &self.inputs.query_expander,
            scope,
            trimmed,
            &opts,
            self.clock.as_ref(),
        )
        .await
    }
}
