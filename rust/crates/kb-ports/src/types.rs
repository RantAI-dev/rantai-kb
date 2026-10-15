//! Domain types shared across the engine and its adapters.

use serde::{Deserialize, Serialize};

pub use kb_core::intelligence::{Entity, Relation};

use crate::ids::{ChunkId, DocumentId, KnowledgeBaseId, TenantId};

/// Chunk type as understood by retrieval, rendering and the policy in
/// `kb-core`. Stored in SurrealDB as a string field (`metadata.chunkType`)
/// and carried on the wire as an enum so the boundary is explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkType {
    Text,
    Table,
    List,
    Code,
    Heading,
    Figure,
}

impl ChunkType {
    /// Parse the string written by the ingest path. Unknown values fall back to
    /// `Text` so a future enum variant doesn't silently drop chunks.
    #[must_use]
    pub fn from_storage(s: Option<&str>) -> Self {
        match s {
            Some("table") => Self::Table,
            Some("list") => Self::List,
            Some("code") => Self::Code,
            Some("heading") => Self::Heading,
            Some("figure") => Self::Figure,
            _ => Self::Text,
        }
    }

    /// Wire / storage form. Stable across versions.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Table => "table",
            Self::List => "list",
            Self::Code => "code",
            Self::Heading => "heading",
            Self::Figure => "figure",
        }
    }

    /// Alias for `as_str`.
    #[must_use]
    pub fn as_storage(&self) -> &'static str {
        self.as_str()
    }
}

/// Metadata carried alongside a chunk at search time. Mirrors what the
/// ingestion path writes into `metadata.{section,page,chunkType,...}` in
/// SurrealDB.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChunkMetadata {
    pub section: Option<String>,
    pub page: Option<u32>,
    pub chunk_index: Option<u32>,
    pub asset_key: Option<String>,
    pub anchor_chunk_index: Option<u32>,
    pub contextual_prefix: Option<String>,
}

/// A chunk returned by the chunk store. The store is responsible for filling
/// `document_title` from a join against the document store; the engine never
/// sees a chunk whose parent doc is invisible to the tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: ChunkId,
    pub document_id: DocumentId,
    pub document_title: String,
    pub content: String,
    pub chunk_type: ChunkType,
    pub metadata: ChunkMetadata,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub subcategory: Option<String>,
}

/// A chunk on its way into the vector store.
///
/// `metadata` is the object stored verbatim in SurrealDB's `metadata` column,
/// so it carries the field names the Bun chunker writes (`documentTitle`,
/// `category`, `section`, `chunkIndex`, `chunkType`, …) rather than the
/// read-shape of [`ChunkMetadata`]. Keeping it opaque is what lets a Rust
/// worker write chunks the Bun retrieval path reads back unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkToStore {
    pub content: String,
    pub chunk_index: u32,
    pub embedding: Vec<f32>,
    pub metadata: serde_json::Value,
    /// Written to the `contextual_prefix` column when present. SurrealDB
    /// rejects an explicit NULL for `option<string>`, so the adapter omits the
    /// assignment instead.
    pub contextual_prefix: Option<String>,
}

/// One row of the Postgres `IngestJob` queue, as handed to a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub tenant_id: TenantId,
    /// `externalRef` in the schema: the user who uploaded the file, if known.
    pub user_id: Option<String>,
    pub document_id: Option<DocumentId>,
    pub s3_key: Option<String>,
    pub filename: String,
    pub mime_type: Option<String>,
    pub attempt: i32,
    /// Job parameters written at enqueue time (title, categories, figure mode…).
    pub params: Option<serde_json::Value>,
}

/// Options passed to [`Extractor::extract_with_options`](crate::Extractor::extract_with_options).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionOptions {
    /// Request layout extractors to extract and return cropped figures.
    pub figures: bool,
    /// Force running layout extraction even if the PDF has a readable text layer.
    pub force_layout: bool,
    /// Specific document type hint (e.g. "handwritten", "table", "form", "printed_text").
    #[serde(default)]
    pub document_type: Option<String>,
}

/// Result of running an [`Extractor`](crate::Extractor) over a stored file.
///
/// Mirrors the TypeScript `ExtractionResult` (`src/lib/ingest/extract.ts:26`),
/// including its failure shape: a parser that cannot read the file returns a
/// result carrying `error` rather than empty content, and the caller aborts the
/// ingest. The layout-only fields are empty for every local parser; they exist
/// so the shape does not change when a layout extractor lands.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtractionResult {
    /// Extracted text; empty when `error` is set.
    pub content: String,
    /// Format of `content` after extraction (`"markdown"`, `"pdf"`, `"image"`),
    /// matching the TypeScript union.
    pub file_type: String,
    /// Whether an OCR or layout extractor produced the text.
    pub used_ocr: bool,
    /// Figures the layout parser found. Empty for text-only formats.
    pub figures: Vec<ExtractedFigure>,
    /// Per-page reading-order blocks, with figures inline at their own
    /// position, so a figure can be anchored to the text chunk it belongs to
    /// instead of to whatever caption matched.
    pub pages_blocks: Option<Vec<Vec<PageBlock>>>,
    /// `(page, text)` pairs in document order, so each chunk can carry the
    /// page it came from.
    pub page_map: Option<Vec<PageText>>,
    /// Set when extraction failed; the caller must abort rather than index the
    /// empty content.
    pub error: Option<String>,
}

impl ExtractionResult {
    /// Plain text with no figures and no page information — what every local
    /// parser returns.
    #[must_use]
    pub fn text(content: impl Into<String>, file_type: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            file_type: file_type.into(),
            ..Self::default()
        }
    }

    /// The TypeScript failure shape (`extract.ts:192`): no content, plus the
    /// reason. A value rather than only an error type so the reason survives to
    /// whoever reads the result.
    #[must_use]
    pub fn failed(file_type: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            file_type: file_type.into(),
            error: Some(error.into()),
            ..Self::default()
        }
    }
}

/// One entry of a layout parser's page map (an element of the TypeScript
/// `pageMap` array).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PageText {
    /// Zero-based page index.
    pub page: u32,
    pub text: String,
}

/// One block of a page in reading order (the TypeScript `PageBlocks`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PageBlock {
    pub kind: PageBlockKind,
    /// Set when `kind` is [`PageBlockKind::Figure`]; matches the figure's id.
    pub id: Option<String>,
    /// Set for text and caption blocks.
    pub text: Option<String>,
}

/// Which kind of block a [`PageBlock`] is.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PageBlockKind {
    /// Body text.
    Text,
    /// A figure's caption.
    Caption,
    /// A figure, inline at the position it sits at in the page's reading order.
    Figure,
}

/// One figure crop lifted out of a document by a layout extractor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFigure {
    pub kind: String,
    pub page: u32,
    /// `[x0, y0, x1, y1]` in page points.
    pub bbox: [f32; 4],
    pub caption: Option<String>,
    /// Crop bytes. Stored as an asset by the caller; kept raw here so the port
    /// does not force an image encoding choice on every extractor.
    pub image_bytes: Vec<u8>,
    /// Identifier matching `id` in `PageBlock` for reading order anchoring.
    #[serde(default)]
    pub id: Option<String>,
}

/// Candidate figure presented to the figure gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FigureGateCandidate {
    /// Chunk identifier
    pub id: String,
    /// Storage key of the crop image
    pub asset_key: String,
    /// Figure caption or title
    pub caption: String,
}

/// A single hit from one retrieval arm. Multiple arms are fused upstream into a
/// ranked list; this is the raw material that goes in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub chunk: Chunk,
    /// Raw cosine similarity (vector arm) or BM25 score. Stored verbatim —
    /// the fusion code uses ranks, but the engine records the score for the
    /// caller to inspect via debug fields.
    pub similarity: f32,
    /// Zero-based rank within the source arm. Set by the chunk store before
    /// the engine sees it.
    pub rank: u32,
    /// Whether this hit originated from the BM25 arm.
    #[serde(default)]
    pub is_bm25: bool,
}

impl SearchHit {
    /// Stable id used by RRF. Mirrors the `RrfItem::id` contract.
    #[must_use]
    pub fn rrf_id(&self) -> &str {
        &self.chunk.id.0
    }
}

/// Lightweight metadata about a document — what the engine needs to render
/// sources and reason about visibility. Heavy fields (file bytes, status) stay
/// in the document store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMeta {
    pub id: DocumentId,
    pub title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    pub status: String,
    pub chunk_count: u32,
    pub file_type: Option<String>,
    pub file_size: Option<i32>,
    pub mime_type: Option<String>,
    pub s3_key: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub knowledge_base_ids: Vec<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Summary of a knowledge base, as returned by the document store list path.
/// Mirrors the shape the Bun service emits ({ id, name, description, color,
/// documentCount }).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBaseSummary {
    pub id: KnowledgeBaseId,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    pub document_count: u32,
}

/// Authentication context for a single request. Populated by the auth layer
/// (API key lookup) and threaded through the engine so every call can be
/// attributed and scoped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthContext {
    pub tenant_id: TenantId,
    pub key_id: String,
    pub scopes: Vec<String>,
    /// Empty = the whole tenant. Non-empty restricts to those knowledge bases.
    pub knowledge_base_ids: Vec<KnowledgeBaseId>,
}

/// A candidate handed to a reranker. The reranker ranks the inputs by query
/// relevance and returns [RerankResult]s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankCandidate {
    pub id: ChunkId,
    pub text: String,
    pub original_rank: u32,
    pub original_score: f32,
}

/// Reranker output. final_rank is zero-based and contiguous.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankResult {
    pub id: ChunkId,
    pub final_rank: u32,
    pub score: f32,
}

/// A single (query, id) score from the embedder. Same id may appear across
/// paraphrase expansions; the engine picks the max.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankScore {
    pub chunk_id: ChunkId,
    pub score: f32,
}

/// Detailed view of an ingest job, as returned by `GET /v1/jobs/:id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: String,
    pub status: String,
    pub step: Option<String>,
    pub progress: u32,
    pub step_current: Option<u32>,
    pub step_total: Option<u32>,
    pub eta_seconds: Option<i32>,
    pub attempt: i32,
    pub error: Option<String>,
    pub document_id: Option<String>,
}

/// Parameters to create a new pending ingest job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateJobInput {
    pub tenant_id: TenantId,
    pub user_id: Option<String>,
    pub filename: String,
    pub file_size: Option<i32>,
    pub mime_type: Option<String>,
    pub s3_key: Option<String>,
    pub document_id: Option<DocumentId>,
    pub params: serde_json::Value,
}

/// A new document row before storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewDocument {
    pub id: DocumentId,
    pub tenant_id: TenantId,
    pub title: String,
    pub content: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    pub s3_key: Option<String>,
    pub file_size: Option<i32>,
    pub mime_type: Option<String>,
    pub status: String,
    pub external_ref: Option<String>,
}

/// Metadata update patch for a document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchDocument {
    pub title: Option<String>,
    pub categories: Option<Vec<String>>,
    pub subcategory: Option<Option<String>>,
    pub knowledge_base_ids: Option<Vec<KnowledgeBaseId>>,
}

/// One stored chunk as the document-detail route returns it: the SurrealDB
/// record id, the position in the document, and the read-shape metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredChunk {
    /// SurrealDB record id, rendered as a string (the Bun client does the same).
    pub id: String,
    pub chunk_index: u32,
    pub content: String,
    /// `metadata.chunkType`, absent when the chunk carries none.
    pub chunk_type: Option<String>,
    pub metadata: ChunkMetadata,
}

/// Minimal document metadata needed to stream raw bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentDownloadMeta {
    pub id: DocumentId,
    pub s3_key: Option<String>,
    pub mime_type: Option<String>,
    pub title: String,
}

/// A failed job eligible for S3 blob reaping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReapableJob {
    pub id: String,
    pub s3_key: String,
}

/// Full record of a knowledge base in Postgres.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeBaseRecord {
    pub id: KnowledgeBaseId,
    pub tenant_id: TenantId,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Input to create a knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKnowledgeBase {
    pub id: KnowledgeBaseId,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
}

/// Input to patch a knowledge base.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchKnowledgeBase {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub color: Option<Option<String>>,
}

/// Category record as stored in Postgres.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryRecord {
    pub id: String,
    pub name: String,
    pub label: String,
    pub color: Option<String>,
}

/// Input to create a category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCategory {
    pub id: String,
    pub name: String,
    pub label: String,
    pub color: Option<String>,
}

/// Input to patch a category.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchCategory {
    pub name: Option<String>,
    pub label: Option<String>,
    pub color: Option<Option<String>>,
}

/// Entity item in `GET /v1/documents/:id/intelligence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityView {
    pub id: String,
    pub name: String,
    pub r#type: String,
    pub confidence: f32,
    pub metadata: serde_json::Value,
}

/// Relation item in `GET /v1/documents/:id/intelligence`. `in`/`out` are the
/// record ids of the edge's endpoints, and the relation type is the dynamic
/// table the edge lives in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationView {
    pub id: String,
    #[serde(rename = "in")]
    pub from_id: String,
    pub out: String,
    pub relation_type: String,
    pub confidence: f32,
    pub metadata: serde_json::Value,
}

/// Combined intelligence data for a document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentIntelligenceData {
    pub entities: Vec<EntityView>,
    pub relations: Vec<RelationView>,
}

/// Outcome of attempting to retry an ingest job.
///
/// The document status flip is deliberately left to the caller: the TypeScript
/// retry route performs it as a separate best-effort update that is allowed to
/// fail without failing the retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryJobOutcome {
    /// The job was reset to `pending`, carrying the document it indexes.
    Success {
        /// The retried job id.
        job_id: String,
        /// The document the job indexes, when the row carried one.
        document_id: Option<String>,
    },
    /// The job exists but is not in `failed` state (409 in the API).
    NotFailed,
    /// No job with that id is visible to the tenant (404 in the API).
    NotFound,
}
