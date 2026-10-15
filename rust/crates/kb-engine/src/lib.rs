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
    ChunkStore, DocumentScope, DocumentStore, Embedder, EngineError, FigureGate, PortError,
    Reranker,
};

#[cfg(any(test, feature = "test-utils"))]
pub mod fakes;
pub mod ingest;
pub mod retrieval;

#[cfg(test)]
mod ingest_tests;
#[cfg(test)]
mod tests;

pub use ingest::{CancelToken, IngestSettings, IngestWorker, JobOutcome, reap_failed_job_uploads};
pub use retrieval::{RetrievalOptions, RetrievalOutcome, enrich_titles, execute_pipeline};

/// Tunable engine knobs.
#[derive(Debug, Clone)]
pub struct EngineSettings {
    /// Candidates fetched per arm before fusion (`KB_RERANK_INITIAL_K`).
    pub candidate_pool_size: u32,
    /// Chunks kept by the reranker (`KB_RERANK_FINAL_K`).
    pub rerank_final_k: u32,
    /// Default chunk cap when the request has none (`KB_DEFAULT_MAX_CHUNKS`).
    pub max_chunks: u32,
    /// Minimum similarity score for vector-only hits (`KB_MIN_SIMILARITY`).
    pub min_similarity: f32,
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
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            candidate_pool_size: 20,
            rerank_final_k: 5,
            max_chunks: 8,
            min_similarity: 0.30,
            neighbour_window: 1,
            figure_limit: 3,
            expansion_enabled: false,
            expansion_paraphrases: 3,
            hybrid_bm25_enabled: true,
            rerank_enabled: false,
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
    pub chunk_store: Arc<dyn ChunkStore>,
    pub reranker: Arc<dyn Reranker>,
    pub document_store: Arc<dyn DocumentStore>,
    pub query_expander: Arc<dyn QueryExpander>,
    pub figure_gate: Option<Arc<dyn FigureGate>>,
}

/// Engine handle — cheap to clone (all state behind `Arc`s).
#[derive(Clone)]
pub struct Engine {
    pub settings: EngineSettings,
    pub inputs: EngineInputs,
}

impl Engine {
    #[must_use]
    pub fn new(settings: EngineSettings, inputs: EngineInputs) -> Self {
        Self { settings, inputs }
    }

    /// Build an engine with the default no-op expander.
    #[must_use]
    pub fn with_settings_and_inputs(
        settings: EngineSettings,
        embedder: Arc<dyn Embedder>,
        chunk_store: Arc<dyn ChunkStore>,
        reranker: Arc<dyn Reranker>,
        document_store: Arc<dyn DocumentStore>,
    ) -> Self {
        let inputs = EngineInputs {
            embedder,
            chunk_store,
            reranker,
            document_store,
            query_expander: Arc::new(NoopExpander),
            figure_gate: None,
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
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(RetrievalOutcome {
                chunks: Vec::new(),
                sources: Vec::new(),
                context: String::new(),
                degrades: Vec::new(),
            });
        }

        let max_chunks = request_max_chunks
            .map(|mc| mc.clamp(1, 50))
            .unwrap_or(self.settings.max_chunks);

        let opts = RetrievalOptions {
            max_chunks,
            min_similarity: self.settings.min_similarity,
            candidate_pool_size: self.settings.candidate_pool_size,
            rerank_final_k: self.settings.rerank_final_k,
            neighbour_window: self.settings.neighbour_window,
            figure_limit: self.settings.figure_limit,
            expansion_enabled: self.settings.expansion_enabled,
            expansion_paraphrases: self.settings.expansion_paraphrases,
            hybrid_bm25_enabled: self.settings.hybrid_bm25_enabled,
            figure_gate: self.inputs.figure_gate.clone(),
            rerank_enabled: self.settings.rerank_enabled,
        };

        execute_pipeline(
            &self.inputs.document_store,
            &self.inputs.chunk_store,
            &self.inputs.embedder,
            &self.inputs.reranker,
            &self.inputs.query_expander,
            scope,
            trimmed,
            &opts,
        )
        .await
    }
}
