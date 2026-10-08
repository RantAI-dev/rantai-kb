//! Port definitions (driven traits). The engine and CLI interact with external
//! systems solely through these interfaces.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::PortError;
use crate::ids::{ChunkId, DocumentId, KnowledgeBaseId, TenantId};
use crate::scope::DocumentScope;
use crate::types::{
    AuthContext, ChunkMetadata, DocumentMeta, KnowledgeBaseSummary, RerankCandidate, RerankResult,
    SearchHit,
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

/// Text embedder (dense vectors).
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
