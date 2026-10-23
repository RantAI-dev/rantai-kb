//! Content-addressed disk caches for the expensive model calls.
//!
//! AGENTS.md rule 7: every embedding, OCR and caption result is cached on disk,
//! keyed by model and content hash, so no input is paid for twice. The wrappers
//! here are decorators over the ports: the engine never knows a cache exists,
//! and the composition root (`kb-server`) decides whether to wrap.

mod embedder;
mod extractor;
mod store;

pub use embedder::CachedEmbedder;
pub use extractor::CachedExtractor;
pub use store::{CacheKey, CacheStats, DiskCache};

#[cfg(test)]
mod tests;
