//! Shared domain types and ports for the KB engine.
//!
//! Pure types and traits only. No I/O, no environment reads, no async runtime
//! in scope. Adapters depend on this; the engine depends on this; nothing in
//! here depends on anything else.

#![allow(clippy::module_name_repetitions)]

mod blob;
mod error;
mod ids;
mod scope;
mod traits;
mod types;

pub use blob::{BlobStore, asset_path, document_path, sanitize_filename};
pub use error::{EngineError, PortError, RetrievalDegrade};
pub use ids::{ChunkId, DocumentId, KnowledgeBaseId, TenantId};
pub use scope::DocumentScope;
#[cfg(feature = "scope-adapter")]
pub use scope::adapter;
pub use traits::{
    ChunkStore, DocumentFilter, DocumentStore, Embedder, EntityExtractor, EntityStore, Extractor,
    FigureGate, JobStore, NeighbourAnchor, ProgressSink, QueryEmbedding, Reranker, ScopeRequest,
};
pub use types::{
    AuthContext, CategoryRecord, Chunk, ChunkMetadata, ChunkToStore, ChunkType, CreateCategory,
    CreateJobInput, CreateKnowledgeBase, DocumentDownloadMeta, DocumentIntelligenceData,
    DocumentMeta, Entity, EntityView, ExtractedFigure, ExtractionOptions, ExtractionResult,
    FigureGateCandidate, JobRecord, JobView, KnowledgeBaseRecord, KnowledgeBaseSummary,
    NewDocument, PageBlock, PageBlockKind, PageText, PatchCategory, PatchDocument,
    PatchKnowledgeBase, ReapableJob, Relation, RelationView, RerankCandidate, RerankResult,
    RerankScore, RetryJobOutcome, SearchHit, StoredChunk,
};
