//! Inference adapters — embedder, query expander, rerankers, entity extraction, figure gate.
//!
//! Selected by the engine at boot from `KB_*` env vars; the engine only sees
//! the trait objects from `kb-ports`.

#![allow(clippy::module_name_repetitions)]

pub mod embedder;
pub mod heuristic_extractor;
pub mod llm_extractor;
pub mod llm_reranker;
pub mod page_describer;
pub mod page_embedder;
pub mod query_cache;
pub mod query_expansion;
pub mod rerankers;
pub mod vision_scorer;
pub mod vlm_gate;

pub use embedder::{EmbedderImpl, EmbedderSettings};
pub use heuristic_extractor::{HeuristicExtractor, HeuristicExtractorSettings};
pub use llm_extractor::{ExtractorSettings, LlmCombinedExtractor};
/// Type alias for ExtractorSettings.
pub type ExtractorConfig = ExtractorSettings;
pub use llm_reranker::LlmReranker;
pub use page_describer::{
    DEFAULT_PAGE_CAPTION_BASE_URL, HttpPageDescriber, PAGE_DESCRIBE_PROMPT,
    PAGE_DESCRIBE_PROMPT_VERSION, PageDescriberSettings,
};
pub use page_embedder::{HttpPageEmbedder, PageEmbedFormat, PageEmbedderSettings};
pub use query_expansion::{ExpansionSettings, QueryExpander};
pub use rerankers::{CohereReranker, TeiReranker, VllmReranker};
pub use vision_scorer::{
    DEFAULT_VISION_BASE_URL, HttpPageVisionScorer, VISION_PROMPT_VERSION, VisionScorerSettings,
};
pub use vlm_gate::VlmFigureGate;

use async_trait::async_trait;
use kb_ports::{PortError, RerankCandidate, RerankResult, Reranker};

/// No-op reranker that preserves candidate order. Used when rerank is disabled
/// or all providers failed and we want a stable, predictable fallback (engine
/// surfaces degrade via `RetrievalDegrade`).
pub struct NoopReranker;

#[async_trait]
impl Reranker for NoopReranker {
    async fn rerank(
        &self,
        _query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError> {
        Ok(candidates
            .into_iter()
            .take(final_k as usize)
            .enumerate()
            .map(|(i, c)| RerankResult {
                id: c.id,
                final_rank: i as u32,
                score: c.original_score,
            })
            .collect())
    }
}
