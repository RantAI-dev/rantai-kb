//! Shared domain types and ports for the KB engine.
//!
//! Pure types and traits only. No I/O, no environment reads, no async runtime
//! in scope. Adapters depend on this; the engine depends on this; nothing in
//! here depends on anything else.

#![allow(clippy::module_name_repetitions)]

mod error;
mod ids;
mod scope;
mod traits;
mod types;

pub use error::{EngineError, PortError, RetrievalDegrade};
pub use ids::{ChunkId, DocumentId, KnowledgeBaseId, TenantId};
pub use scope::DocumentScope;
pub use traits::{ChunkStore, DocumentStore, Embedder, Reranker};
pub use types::{
    AuthContext, Chunk, ChunkMetadata, ChunkType, DocumentMeta, KnowledgeBaseSummary,
    RerankCandidate, RerankResult, RerankScore, SearchHit,
};