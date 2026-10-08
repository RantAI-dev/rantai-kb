//! Read-path engine.
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

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{
    ChunkStore, DocumentScope, DocumentStore, Embedder, EngineError, PortError, Reranker,
};

#[cfg(any(test, feature = "test-utils"))]
pub mod fakes;
pub mod retrieval;

#[cfg(test)]
mod tests;

pub use retrieval::{RetrievalOptions, RetrievalOutcome, enrich_titles, execute_pipeline};

/// Tunable engine knobs.
#[derive(Debug, Clone)]
pub struct EngineSettings {
    pub candidate_pool_size: u32,
    pub rerank_final_k: u32,
    pub max_chunks: u32,
    pub min_similarity: f32,
    pub neighbour_window: u32,
    pub figure_limit: u32,
    pub expansion_enabled: bool,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            candidate_pool_size: 40,
            rerank_final_k: 20,
            max_chunks: 10,
            min_similarity: 0.30,
            neighbour_window: 2,
            figure_limit: 4,
            expansion_enabled: false,
        }
    }
}

/// A query expander the server can plug in without forcing `kb-engine` to
/// depend on `kb-inference`.
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
        };

        execute_pipeline(
            &self.inputs.document_store,
            &self.inputs.chunk_store,
            &self.inputs.embedder,
            &self.inputs.reranker,
            scope,
            trimmed,
            &opts,
        )
        .await
    }
}
