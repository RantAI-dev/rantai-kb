//! Ports: store and inference traits.
//!
//! Every trait method takes structured arguments; no raw query strings cross a
//! trait boundary. Implementations are responsible for translating them into
//! SQL or SurrealQL and for binding every value (no string interpolation of
//! caller-supplied data).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::PortError;
use crate::ids::{DocumentId, KnowledgeBaseId, TenantId};
use crate::scope::DocumentScope;
use crate::types::{
    ChunkMetadata, DocumentMeta, KnowledgeBaseSummary, RerankCandidate, RerankResult, SearchHit,
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

/// Document metadata lookup. Lives behind the Postgres adapter. The engine
/// depends on this; it never touches `DocumentStore` for raw query strings.
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

    /// List documents visible to the tenant (optionally filtered). Used by
    /// `GET /v1/documents`.
    async fn list(
        &self,
        tenant: &TenantId,
        limit: u32,
        knowledge_base_ids: &[KnowledgeBaseId],
    ) -> Result<Vec<DocumentMeta>, PortError>;

    /// Fetch ordered chunks for a document. Used by `GET /v1/documents/:id`.
    /// Document visibility is the caller's responsibility — pass the id only
    /// after `find_by_id` succeeded.
    async fn ordered_chunks(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<(u32, String, ChunkMetadata)>, PortError>;

    /// List knowledge bases visible to the tenant.
    async fn list_knowledge_bases(
        &self,
        tenant: &TenantId,
    ) -> Result<Vec<KnowledgeBaseSummary>, PortError>;

    /// Look up the tenant for a presented API key. Returns `None` if the key
    /// is unknown, revoked, or hashed differently in the response.
    async fn resolve_api_key(&self, plaintext_key: &str) -> Result<Option<TenantId>, PortError>;
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

    /// BM25 search by raw query string. Hits are returned in BM25 score order.
    async fn bm25_search(
        &self,
        scope: &DocumentScope,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError>;

    /// Fetch up to `window` adjacent chunks per anchor (same document, by
    /// `chunk_index`). Returns zero-score hits so the engine can keep the
    /// anchor's rank priority while adding context.
    async fn neighbours(
        &self,
        anchors: &[NeighbourAnchor],
        window: u32,
    ) -> Result<Vec<SearchHit>, PortError>;

    /// Figure co-retrieval: pull in figures from `scope`'s documents whose
    /// caption matches the query or already-retrieved text. The vision gate
    /// is NOT applied at this layer (R1 scope).
    async fn figures(
        &self,
        scope: &DocumentScope,
        query: &str,
        retrieved_text: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError>;
}

/// Anchor for a neighbour lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeighbourAnchor {
    pub chunk_id: crate::ids::ChunkId,
    pub document_id: DocumentId,
    pub chunk_index: u32,
}

/// Embedder: turns text into vectors. The engine never embeds inline — it
/// calls the port, hits a cache upstream, and hands the vector to the chunk
/// store. R1 has no LRU; the embedding read path is best-effort cached inside
/// `kb-inference` for repeated queries.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a single query. Concatenates any configured query prefix.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError>;

    /// Embed a batch of passages (storage). Concatenates any configured passage
    /// prefix. Implementations may chunk internally and respect provider caps.
    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError>;

    /// Vector dimensionality of the configured model. The engine asserts on
    /// this when storing chunks (R3) but reads it here to confirm the chunk
    /// store's expectation.
    fn dimension(&self) -> u32;
}

/// Reranker: takes ranked candidates and returns a reordered list.
#[async_trait]
pub trait Reranker: Send + Sync {
    /// Rank the candidates against `query` and return the top `final_k`.
    /// Returns them in final order. Missing entries from the backend are
    /// back-filled with zero-score leftovers so the caller can still map by id.
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError>;
}