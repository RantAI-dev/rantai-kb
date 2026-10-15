//! Domain types shared across the engine and its adapters.

use serde::{Deserialize, Serialize};

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

/// Result of running an [`Extractor`](crate::Extractor) over a stored file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtractionResult {
    pub content: String,
    /// Format of `content` after extraction (`"markdown"`, `"text"`, …).
    pub file_type: String,
    /// Figures the layout parser found. Empty for text-only formats.
    pub figures: Vec<ExtractedFigure>,
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
}

// `RrfItem` lives in `kb-core` because the fusion algorithm lives there, but
// we add the impl here so the engine can fuse `SearchHit` lists without the
// orphan rule biting.
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
    /// Object-store key of the stored original file, when the document has one.
    /// Hard delete removes this object.
    pub s3_key: Option<String>,
    /// RFC 3339 timestamps, UTC.
    pub created_at: String,
    pub updated_at: String,
    /// Knowledge bases this document belongs to (from DocumentGroup).
    pub knowledge_base_ids: Vec<String>,
    /// Raw `Document.metadata` JSON; figures are read from it by the server.
    pub metadata: Option<serde_json::Value>,
}

/// Summary of a knowledge base, as returned by the document store list path.
/// Mirrors the shape the Bun service emits (`{ id, name, description, color,
/// documentCount }`).
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
/// relevance and returns [`ReRerankResult`]s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankCandidate {
    pub id: ChunkId,
    pub text: String,
    pub original_rank: u32,
    pub original_score: f32,
}

/// Reranker output. `final_rank` is zero-based and contiguous.
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
