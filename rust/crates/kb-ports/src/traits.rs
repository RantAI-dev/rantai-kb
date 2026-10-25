//! Port definitions (driven traits). The engine and CLI interact with external
//! systems solely through these interfaces.
//!
//! Every external dependency — Postgres, Qdrant, object storage, embedding,
//! reranking, text extraction — sits behind a trait here.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::PortError;
use crate::ids::{ChunkId, DocumentId, KnowledgeBaseId, TenantId};
use crate::scope::DocumentScope;
use crate::types::{
    AuthContext, CategoryRecord, ChunkToStore, CreateCategory, CreateJobInput, CreateKnowledgeBase,
    DocumentDownloadMeta, DocumentIntelligenceData, DocumentMeta, Entity, ExtractionOptions,
    ExtractionResult, FigureGateCandidate, JobRecord, JobView, KnowledgeBaseRecord,
    KnowledgeBaseSummary, NewDocument, PatchCategory, PatchDocument, PatchKnowledgeBase,
    ReapableJob, Relation, RerankCandidate, RerankResult, RetryJobOutcome, SearchHit, StoredChunk,
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

    /// Look up one document by id for a known tenant.
    ///
    /// This is the `findFirst({ id, tenantId })` the HTTP routes run before
    /// every read, write or blob access, with `deletedAt: null` added unless
    /// `include_deleted` is set: reads exclude soft-deleted rows, while
    /// `DELETE /v1/documents/:id` must still find one so a soft delete can be
    /// followed by a hard delete. It is not the scoped
    /// [`DocumentStore::find_by_id`], because building a scope resolves every
    /// document id the tenant owns and the route needs exactly one row.
    async fn find_meta_by_id(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
        include_deleted: bool,
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

    /// Merge metadata fields into a document's JSON metadata column.
    ///
    /// Mirrors `kb("documents").updateMetadata(id, patch)` in TypeScript.
    async fn update_metadata(
        &self,
        id: &DocumentId,
        patch: &serde_json::Value,
    ) -> Result<(), PortError>;

    /// Set a document's ingest status (`"ready"`, `"failed"`, `"processing"`).
    ///
    /// Failure to update is logged and swallowed by the worker, as in the TS
    /// adapter, so a missing row does not mask the job's own outcome.
    async fn set_status(&self, id: &DocumentId, status: &str) -> Result<(), PortError>;

    /// Create a new document and link it to knowledge bases.
    async fn create_document(
        &self,
        doc: &NewDocument,
        group_kb_ids: &[KnowledgeBaseId],
    ) -> Result<(), PortError>;

    /// Patch an existing document's metadata or knowledge base links.
    ///
    /// Must resolve the document within the tenant scope first.
    /// Returns `Ok(None)` if no document was found.
    async fn patch_document(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
        patch: &PatchDocument,
    ) -> Result<Option<DocumentMeta>, PortError>;

    /// Find document download metadata by id (tenant-filtered, not deleted).
    async fn find_for_download(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
    ) -> Result<Option<DocumentDownloadMeta>, PortError>;

    /// Filter requested knowledge base IDs to those actually owned by the tenant.
    async fn filter_owned_kb_ids(
        &self,
        tenant: &TenantId,
        kb_ids: &[KnowledgeBaseId],
    ) -> Result<Vec<KnowledgeBaseId>, PortError>;

    /// Create a new knowledge base.
    async fn create_knowledge_base(
        &self,
        tenant: &TenantId,
        input: &CreateKnowledgeBase,
    ) -> Result<KnowledgeBaseRecord, PortError>;

    /// Patch a knowledge base by ID (tenant-scoped).
    async fn patch_knowledge_base(
        &self,
        tenant: &TenantId,
        id: &KnowledgeBaseId,
        input: &PatchKnowledgeBase,
    ) -> Result<Option<KnowledgeBaseRecord>, PortError>;

    /// Delete a knowledge base and its document links (tenant-scoped).
    async fn delete_knowledge_base(
        &self,
        tenant: &TenantId,
        id: &KnowledgeBaseId,
    ) -> Result<bool, PortError>;

    /// List categories for a tenant.
    async fn list_categories(&self, tenant: &TenantId) -> Result<Vec<CategoryRecord>, PortError>;

    /// Create a category for a tenant.
    async fn create_category(
        &self,
        tenant: &TenantId,
        input: &CreateCategory,
    ) -> Result<CategoryRecord, PortError>;

    /// Patch a category by ID (tenant-scoped).
    async fn patch_category(
        &self,
        tenant: &TenantId,
        id: &str,
        input: &PatchCategory,
    ) -> Result<Option<CategoryRecord>, PortError>;

    /// Delete a category by ID (tenant-scoped).
    async fn delete_category(&self, tenant: &TenantId, id: &str) -> Result<bool, PortError>;

    /// Liveness probe for the backing database. `GET /health` reports this
    /// rather than assuming the store is reachable.
    async fn health_check(&self) -> Result<(), PortError> {
        Ok(())
    }
}

/// Legacy chunk-store port: vector + BM25 + neighbours + figure co-retrieval.
/// Every method takes a scope so a tenant leak is a type error.
///
/// Superseded by [`crate::RetrievalIndex`], which is what server and engine use.
/// It remains only because the in-process test fake implements it; no production
/// adapter does.
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
    /// when present.
    async fn store_chunks(
        &self,
        document_id: &DocumentId,
        chunks: &[ChunkToStore],
        embedding_model: Option<&str>,
    ) -> Result<(), PortError>;

    /// Chunk counts per document. Missing ids are absent from the result and
    /// count as 0 at the call site.
    async fn chunk_counts(
        &self,
        document_ids: &[DocumentId],
    ) -> Result<Vec<(DocumentId, u32)>, PortError>;

    /// Ordered chunks for one document, for `GET /v1/documents/:id`. Document
    /// visibility is the caller's job: pass the id only after `find_by_id`.
    async fn ordered_chunks(&self, document_id: &DocumentId)
    -> Result<Vec<StoredChunk>, PortError>;

    /// Liveness probe for the vector store. `GET /health` reports this rather
    /// than assuming the store is reachable.
    async fn health_check(&self) -> Result<(), PortError> {
        Ok(())
    }

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

/// Read side of the document intelligence API: the entities and relations one
/// document contributed to the Postgres entity graph (`KB_GRAPH_STORE=pg`).
///
/// Writes go through [`GraphStore`], the one entity store Rust has; there is no
/// second, chunk-store-shaped entity destination. This port stays separate
/// because the intelligence route wants a document's own view, while ingest and
/// retrieval speak in entity keys and scoped graphs.
#[async_trait]
pub trait EntityStore: Send + Sync {
    /// Retrieve extracted entities and relations for one document.
    ///
    /// Tenant-scoped: a document id from another tenant returns empty rather
    /// than its rows.
    async fn get_document_intelligence(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<DocumentIntelligenceData, PortError>;
}

/// Document intelligence extractor port.
#[async_trait]
pub trait EntityExtractor: Send + Sync {
    /// Extract entities from text.
    async fn extract_entities(
        &self,
        text: &str,
        document_id: Option<&str>,
        chunk_id: Option<&str>,
    ) -> Result<Vec<Entity>, PortError>;

    /// Extract entities and relations in a single pass.
    async fn extract_entities_and_relations(
        &self,
        text: &str,
        document_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<(Vec<Entity>, Vec<Relation>), PortError>;
}

/// Figure veto gate port for multimodal retrieval.
#[async_trait]
pub trait FigureGate: Send + Sync {
    /// Keep only candidates the vision model vouches for. Returns survivor IDs.
    async fn gate_figures(
        &self,
        query: &str,
        candidates: &[FigureGateCandidate],
    ) -> Result<Vec<String>, PortError>;
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

    /// Create a new pending ingest job. Returns the created job ID.
    async fn create_job(&self, input: &CreateJobInput) -> Result<String, PortError>;

    /// Get a job by ID (tenant-scoped).
    async fn get_job(&self, tenant: &TenantId, id: &str) -> Result<Option<JobView>, PortError>;

    /// Retry a failed job (tenant-scoped).
    async fn retry_job(&self, tenant: &TenantId, id: &str) -> Result<RetryJobOutcome, PortError>;

    /// List failed jobs older than `max_age_days` with a non-null `s3Key`, so
    /// their stored file can be reaped.
    async fn list_reapable(
        &self,
        max_age_days: u32,
        limit: u32,
    ) -> Result<Vec<ReapableJob>, PortError>;

    /// Clear the s3_key of a reaped job.
    async fn clear_s3_key(&self, job_id: &str) -> Result<(), PortError>;
}

/// Progress sink for streaming job updates and SSE event frames.
#[async_trait]
pub trait ProgressSink: Send + Sync {
    /// Emit an event with a JSON payload to a tenant's subscribers.
    async fn emit(
        &self,
        tenant_id: &TenantId,
        event: &str,
        payload: serde_json::Value,
    ) -> Result<(), PortError>;
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
    ) -> Result<ExtractionResult, PortError> {
        self.extract_with_options(
            filename,
            mime_type,
            file_bytes,
            ExtractionOptions::default(),
        )
        .await
    }

    /// Extract text (and figures when configured) with explicit extraction options.
    async fn extract_with_options(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
        _options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        self.extract(filename, mime_type, file_bytes).await
    }
}

#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a single query string for retrieval.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError>;

    /// Embed a single query string and say where the vector came from.
    ///
    /// The same call as [`Embedder::embed_query`] with one extra fact
    /// attached: whether a cache answered it or a provider was called. Query
    /// embedding is the round trip that dominates search latency on a local
    /// profile, and "it was 900 ms" means something very different for a
    /// repeat of the same question than for a first-time question, so the
    /// latency report needs to separate the two instead of averaging them.
    ///
    /// The default says `cached: false` — correct for an embedder that does not
    /// cache, and the honest default for one that does and does not report it.
    async fn embed_query_detailed(&self, text: &str) -> Result<QueryEmbedding, PortError> {
        self.embed_query(text).await.map(|vector| QueryEmbedding {
            vector,
            cached: false,
        })
    }

    /// Embed a batch of document chunks during ingest.
    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError>;

    /// Expected dimensionality of vectors produced by this embedder.
    fn dimension(&self) -> u32;
}

/// A query embedding plus its provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryEmbedding {
    pub vector: Vec<f32>,
    /// True when a cache answered this query rather than the provider.
    pub cached: bool,
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
