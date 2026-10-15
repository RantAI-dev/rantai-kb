//! Port definitions (driven traits). The engine and CLI interact with external
//! systems solely through these interfaces.
//!
//! Every external dependency — Postgres, SurrealDB, object storage, embedding,
//! reranking, text extraction — sits behind a trait here.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::PortError;
use crate::ids::{ChunkId, DocumentId, KnowledgeBaseId, TenantId};
use crate::scope::DocumentScope;
use crate::types::{
    AuthContext, ChunkMetadata, ChunkToStore, DocumentMeta, ExtractionResult, JobRecord,
    KnowledgeBaseSummary, RerankCandidate, RerankResult, SearchHit,
};

/// What the chunk store is allowed to look at. Currently the empty case is
/// allowed (it short-circuits to zero results).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentFilter {
    /// Restrict to documents in any of these knowledge bases. Empty = no
    /// knowledge-base filter; the tenant scope alone applies.
    pub knowledge_base_ids: Vec<KnowledgeBaseId>,
    /// Restrict to documents whose `categories` array contains this value.
    /// `None` = no category filter.
    pub category: Option<String>,
}

/// What the document store can build a [`DocumentScope`] from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeRequest {
    pub tenant: TenantId,
    pub filter: DocumentFilter,
}

/// Anchor for a neighbour lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeighbourAnchor {
    pub chunk_id: ChunkId,
    pub document_id: DocumentId,
    pub chunk_index: u32,
}

/// Document-metadata and tenancy store (Postgres). Every read is tenant-
/// filtered; every write preserves soft-delete (`deletedAt IS NULL`).
#[async_trait]
pub trait DocumentStore: Send + Sync {
    /// Build a [`DocumentScope`] for the given tenant + filter. This is the
    /// ONLY way to build one; the visibility rule is enforced at the type level.
    async fn build_scope(&self, req: &ScopeRequest) -> Result<DocumentScope, PortError>;

    /// Look up a single document by id within a scope.
    async fn find_by_id(
        &self,
        scope: &DocumentScope,
        id: &DocumentId,
    ) -> Result<Option<DocumentMeta>, PortError>;

    /// Soft delete: sets `deletedAt`. Returns `false` when no live document matched.
    async fn soft_delete(&self, tenant: &TenantId, id: &DocumentId) -> Result<bool, PortError>;

    /// Hard delete: removes the Postgres row (and its group links). Returns the
    /// stored blob key when there was one, so the caller can remove the object.
    /// Returns `Ok(None)` when no row matched.
    async fn hard_delete(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
    ) -> Result<Option<Option<String>>, PortError>;

    /// Look up metadata for multiple document ids (tenant-filtered, not soft-deleted).
    async fn find_alive_meta_by_ids(
        &self,
        tenant: &TenantId,
        document_ids: &[DocumentId],
    ) -> Result<Vec<DocumentMeta>, PortError>;

    /// List documents visible to the tenant (optionally filtered). Used by
    /// `GET /v1/documents`.
    async fn list(
        &self,
        tenant: &TenantId,
        limit: u32,
        knowledge_base_ids: &[KnowledgeBaseId],
    ) -> Result<Vec<DocumentMeta>, PortError>;

    /// List knowledge bases visible to the tenant.
    async fn list_knowledge_bases(
        &self,
        tenant: &TenantId,
    ) -> Result<Vec<KnowledgeBaseSummary>, PortError>;

    /// Look up the tenant and auth context for a presented API key. Returns `None`
    /// if the key is unknown, revoked, or hashed differently.
    async fn resolve_api_key(&self, plaintext_key: &str) -> Result<Option<AuthContext>, PortError>;

    /// Write the extracted text and file metadata of a document.
    ///
    /// Mirrors the `content` / `fileType` / `fileSize` update the ingest worker
    /// performs after extraction, before chunking.
    async fn update_extracted_content(
        &self,
        id: &DocumentId,
        content: &str,
        file_type: Option<&str>,
        file_size: Option<i32>,
    ) -> Result<(), PortError>;

    /// Set a document's ingest status (`"ready"`, `"failed"`, `"processing"`).
    ///
    /// Failure to update is logged and swallowed by the worker, as in the TS
    /// adapter, so a missing row does not mask the job's own outcome.
    async fn set_status(&self, id: &DocumentId, status: &str) -> Result<(), PortError>;
}

/// Chunk store: vector + BM25 + neighbours + figure co-retrieval over the
/// SurrealDB `document_chunk` table. Every method takes a scope so a tenant
/// leak is a type error.
#[async_trait]
pub trait ChunkStore: Send + Sync {
    /// Cosine search by precomputed query embedding. Returns up to `limit`
    /// hits, ordered by similarity descending.
    async fn vector_search(
        &self,
        scope: &DocumentScope,
        embedding: &[f32],
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError>;

    /// BM25 full-text search over `content`.
    async fn bm25_search(
        &self,
        scope: &DocumentScope,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError>;

    /// Look up adjacent chunks within `window` offsets of each anchor chunk.
    /// Results must be within the provided `scope`.
    async fn neighbours(
        &self,
        scope: &DocumentScope,
        anchors: &[NeighbourAnchor],
        window: u32,
    ) -> Result<Vec<SearchHit>, PortError>;

    /// Delete every chunk belonging to a document (hard delete). Returns the
    /// number removed. Visibility is the caller's job.
    async fn delete_by_document(&self, document_id: &DocumentId) -> Result<u64, PortError>;

    /// Write a batch of chunks for one document.
    ///
    /// Record ids are `${document_id}_${i}` by index, so a re-run of the same
    /// batch replaces rather than duplicates. `embedding_model` is stored only
    /// when present, because SurrealDB rejects a literal NULL for `option<T>`.
    async fn store_chunks(
        &self,
        document_id: &DocumentId,
        chunks: &[ChunkToStore],
        embedding_model: Option<&str>,
    ) -> Result<(), PortError>;

    /// Chunk counts per document, from SurrealDB `document_chunk`. Missing ids
    /// are absent from the result and count as 0 at the call site.
    async fn chunk_counts(
        &self,
        document_ids: &[DocumentId],
    ) -> Result<Vec<(DocumentId, u32)>, PortError>;

    /// Ordered chunks for one document, for `GET /v1/documents/:id`. Document
    /// visibility is the caller's job: pass the id only after `find_by_id`.
    async fn ordered_chunks(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<(u32, String, ChunkMetadata)>, PortError>;

    /// Co-retrieve figures belonging to retrieved documents whose anchor or caption matches.
    async fn figures(
        &self,
        scope: &DocumentScope,
        query: &str,
        retrieved_text: &str,
        retrieved_doc_ids: &[DocumentId],
        anchored_chunks: &[(DocumentId, u32)],
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError>;
}

/// Ingest job queue (Postgres `IngestJob`).
///
/// The queue is shared with the Bun workers: a Rust worker and a Bun worker
/// polling the same table must never take the same row, so `claim_next_pending`
/// uses `FOR UPDATE SKIP LOCKED` and the column names match the TS adapter.
#[async_trait]
pub trait JobStore: Send + Sync {
    /// Atomically flip the oldest `pending` job to `processing` and return it.
    /// Returns `None` when there is nothing to do.
    async fn claim_next_pending(&self) -> Result<Option<JobRecord>, PortError>;

    /// Persist step and overall progress for a claimed job. Best-effort: a
    /// failure here must not fail the ingest.
    async fn update_progress(
        &self,
        job_id: &str,
        step: &str,
        progress: u32,
        step_current: Option<u32>,
        step_total: Option<u32>,
        eta_seconds: Option<i32>,
    ) -> Result<(), PortError>;

    /// Terminal success: status `success`, document linked, progress 100.
    async fn finish_success(&self, job_id: &str, document_id: &DocumentId)
    -> Result<(), PortError>;

    /// Terminal failure: status `failed`, error truncated to 1000 characters.
    /// The stored file is kept so the job can be replayed without re-upload.
    async fn finish_failure(&self, job_id: &str, error: &str) -> Result<(), PortError>;

    /// Touch `updatedAt` so the stale reclaim does not eat a live job during a
    /// long emit-less step.
    async fn touch(&self, job_id: &str) -> Result<(), PortError>;

    /// Crash recovery. Jobs left `processing` with no update for `stale_after`
    /// go back to `pending` with the attempt counter incremented, or to `failed`
    /// once `max_attempts` is reached. Returns how many were reclaimed.
    async fn reclaim_stale(
        &self,
        stale_after: Duration,
        max_attempts: i32,
    ) -> Result<u64, PortError>;
}

/// Turns a stored file into text. Implementations wrap MinerU, a VLM endpoint,
/// or one of the local parsers; the port only cares about the result shape.
#[async_trait]
pub trait Extractor: Send + Sync {
    /// Extract text (and, when the format has them, figures) from `file_bytes`.
    async fn extract(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
    ) -> Result<ExtractionResult, PortError>;
}
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a single query string for retrieval.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError>;

    /// Embed a batch of document chunks during ingest.
    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError>;

    /// Expected dimensionality of vectors produced by this embedder.
    fn dimension(&self) -> u32;
}

/// Cross-encoder reranker. Scores (query, candidate) pairs.
#[async_trait]
pub trait Reranker: Send + Sync {
    /// Rerank candidates against query, returning top `final_k` results.
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError>;
}
