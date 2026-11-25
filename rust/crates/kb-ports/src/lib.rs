//! Shared domain types and ports for the KB engine.
//!
//! Pure types and traits only. No I/O, no environment reads, no async runtime
//! in scope. Adapters depend on this; the engine depends on this; nothing in
//! here depends on anything else.

#![allow(clippy::module_name_repetitions)]

mod blob;
pub mod community;
mod error;
pub mod graph;
#[cfg(feature = "contract")]
pub mod graph_contract;
mod ids;
pub mod ocr;
mod pages;
mod retrieval;
mod scope;
pub mod tables;
mod traits;
mod types;
pub mod visuals;

pub use blob::{
    BlobStore, asset_path, document_path, page_image_path, sanitize_filename, table_grid_path,
};
pub use community::{CommunityRecord, CommunityStore, NewCommunity};
/// The section outline a document's extraction carries (`DEC-4`), re-exported so
/// adapters that implement [`Extractor::outline`] need not depend on kb-core.
pub use kb_core::tree::Outline;
#[cfg(feature = "contract")]
pub mod contract;
pub use error::{EngineError, PortError, RetrievalDegrade};
pub use graph::{EntityMentions, EntityUpsert, GraphStore, RelationUpsert, TenantGraph};
pub use ids::{ChunkId, DocumentId, KnowledgeBaseId, TenantId};
pub use ocr::{LayoutBox, LayoutDetector, OcrPageCache, OcrReport, PageLayout};
pub use pages::{PageDescriber, PageEmbedder, PageImage, PageRenderer, PageVisionScorer};
pub use retrieval::{
    ArmRanks, ArmScores, Candidate, ChunkMeta, ChunkRange, Fusion, HybridQuery, IndexCapabilities,
    IndexUnit, KeywordQuery, KeywordSemantics, RetrievalIndex, chunk_meta_from_candidate,
    sort_candidates,
};
#[cfg(feature = "scope-adapter")]
pub use scope::adapter;
pub use scope::{
    DEFAULT_SCOPE_ID_LIMIT, DocumentScope, PredicateSpec, UnitRange, VerifySpec,
    predicate_spec_for, scope_id_limit_from_env,
};
pub use tables::{TableId, TableQueryFailure, TableRecord, TableStore};
pub use traits::{
    ChunkStore, DocumentFilter, DocumentStore, Embedder, EntityExtractor, EntityStore, Extractor,
    FigureGate, JobStore, MetadataFilter, NeighbourAnchor, ProgressSink, QueryEmbedding, Reranker,
    ScopeRequest,
};
pub use types::{
    AuthContext, CategoryRecord, Chunk, ChunkMetadata, ChunkToStore, ChunkType, CreateCategory,
    CreateJobInput, CreateKnowledgeBase, DocumentDownloadMeta, DocumentIntelligenceData,
    DocumentMeta, DocumentPage, Entity, EntityView, ExtractedFigure, ExtractionOptions,
    ExtractionResult, FigureGateCandidate, JobRecord, JobView, KnowledgeBaseRecord,
    KnowledgeBaseSummary, NewDocument, PageBlock, PageBlockKind, PageText, PatchCategory,
    PatchDocument, PatchKnowledgeBase, ProvenanceArm, ReapableJob, Relation, RelationView,
    RerankCandidate, RerankResult, RerankScore, RetrievalProvenance, RetryJobOutcome, SearchHit,
    SeatRelease, StoredChunk,
};
pub use visuals::{
    ImageDescriber, ImageReading, NormalizedImage, Visual, VisualCollector, VisualKind,
    VisualOptions, VisualOrigin, VisualScan, VisualScanSkips,
};
