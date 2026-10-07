//! Inference adapters — embedder, query expander, rerankers.
//!
//! Selected by the engine at boot from `KB_*` env vars; the engine only sees
//! the trait objects from `kb-ports`.

#![allow(clippy::module_name_repetitions)]

pub mod embedder;
pub mod query_expansion;
pub mod rerankers;

pub use embedder::{EmbedderImpl, EmbedderSettings};
pub use query_expansion::{ExpansionSettings, QueryExpander};
pub use rerankers::{CohereReranker, TeiReranker, VllmReranker};

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
                score: (final_k as f32) - (i as f32),
            })
            .collect())
    }
}