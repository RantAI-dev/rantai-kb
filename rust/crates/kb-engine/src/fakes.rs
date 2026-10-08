#![allow(clippy::unwrap_used, clippy::expect_used)]

//! In-memory fakes for all ports used by `kb-engine` and `kb-server`.
//!
//! Suitable for unit tests and in-process HTTP tests without network dependencies.

use std::collections::HashMap;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use kb_ports::{
    AuthContext, Chunk, ChunkId, ChunkMetadata, ChunkStore, ChunkType, DocumentId, DocumentMeta,
    DocumentScope, DocumentStore, Embedder, KnowledgeBaseId, KnowledgeBaseSummary, NeighbourAnchor,
    PortError, RerankCandidate, RerankResult, Reranker, ScopeRequest, SearchHit, TenantId,
    adapter::ScopeToken,
};

/// Helper to create a synthetic SearchHit.
#[must_use]
pub fn make_fake_hit(
    id: &str,
    doc_id: &str,
    title: &str,
    content: &str,
    chunk_index: u32,
    similarity: f32,
) -> SearchHit {
    SearchHit {
        chunk: Chunk {
            id: ChunkId(id.to_string()),
            document_id: DocumentId(doc_id.to_string()),
            document_title: title.to_string(),
            content: content.to_string(),
            chunk_type: ChunkType::Text,
            metadata: ChunkMetadata {
                chunk_index: Some(chunk_index),
                ..Default::default()
            },
        },
        similarity,
        rank: 0,
    }
}

/// Helper to create a fake `DocumentScope`.
#[must_use]
pub fn make_fake_scope(tenant: TenantId, doc_ids: Vec<DocumentId>) -> DocumentScope {
    let token = ScopeToken::new_for_adapter();
    DocumentScope::new(&token, tenant, doc_ids)
}

/// Fake in-memory document store.
#[derive(Clone, Default)]
pub struct FakeDocumentStore {
    pub docs: Arc<RwLock<HashMap<TenantId, Vec<DocumentMeta>>>>,
    pub api_keys: Arc<RwLock<HashMap<String, AuthContext>>>,
    pub kbs: Arc<RwLock<HashMap<TenantId, Vec<KnowledgeBaseSummary>>>>,
    pub fail_scope: Arc<AtomicBool>,
    /// Soft-deleted documents, kept so tests can assert on them.
    pub deleted: Arc<RwLock<Vec<DocumentMeta>>>,
}

impl FakeDocumentStore {
    pub fn insert_doc(&self, tenant: TenantId, meta: DocumentMeta) {
        let mut map = self.docs.write().unwrap();
        map.entry(tenant).or_default().push(meta);
    }

    pub fn insert_api_key(&self, key: &str, tenant: TenantId) {
        let auth = AuthContext {
            tenant_id: tenant,
            key_id: format!("key-{key}"),
            scopes: vec!["kb:read".to_string(), "kb:write".to_string()],
            knowledge_base_ids: vec![],
        };
        self.api_keys.write().unwrap().insert(key.to_string(), auth);
    }

    pub fn insert_api_key_with_scopes(
        &self,
        key: &str,
        tenant: TenantId,
        scopes: Vec<String>,
        kb_ids: Vec<KnowledgeBaseId>,
    ) {
        let auth = AuthContext {
            tenant_id: tenant,
            key_id: format!("key-{key}"),
            scopes,
            knowledge_base_ids: kb_ids,
        };
        self.api_keys.write().unwrap().insert(key.to_string(), auth);
    }
}

#[async_trait]
impl DocumentStore for FakeDocumentStore {
    async fn build_scope(&self, req: &ScopeRequest) -> Result<DocumentScope, PortError> {
        if self.fail_scope.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced scope failure".to_string()));
        }
        let map = self.docs.read().unwrap();
        let list = map.get(&req.tenant).cloned().unwrap_or_default();
        let doc_ids = list.into_iter().map(|d| d.id).collect();
        let token = ScopeToken::new_for_adapter();
        Ok(DocumentScope::new(&token, req.tenant.clone(), doc_ids))
    }

    async fn find_by_id(
        &self,
        _scope: &DocumentScope,
        id: &DocumentId,
    ) -> Result<Option<DocumentMeta>, PortError> {
        let map = self.docs.read().unwrap();
        for list in map.values() {
            if let Some(doc) = list.iter().find(|d| &d.id == id) {
                return Ok(Some(doc.clone()));
            }
        }
        Ok(None)
    }

    async fn soft_delete(&self, tenant: &TenantId, id: &DocumentId) -> Result<bool, PortError> {
        // Tenant-scoped: another tenant's document is simply not found. The fake
        // has no deleted_at column, so a soft-deleted doc is moved to `deleted`.
        let mut map = self.docs.write().unwrap();
        let Some(list) = map.get_mut(tenant) else {
            return Ok(false);
        };
        let Some(pos) = list.iter().position(|d| &d.id == id) else {
            return Ok(false);
        };
        let doc = list.remove(pos);
        self.deleted.write().unwrap().push(doc);
        Ok(true)
    }

    async fn hard_delete(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
    ) -> Result<Option<Option<String>>, PortError> {
        let mut map = self.docs.write().unwrap();
        let Some(list) = map.get_mut(tenant) else {
            return Ok(None);
        };
        let Some(pos) = list.iter().position(|d| &d.id == id) else {
            return Ok(None);
        };
        list.remove(pos);
        // The fake keeps no blob keys, so none is reported.
        Ok(Some(None))
    }

    async fn find_alive_meta_by_ids(
        &self,
        tenant: &TenantId,
        ids: &[DocumentId],
    ) -> Result<Vec<DocumentMeta>, PortError> {
        let map = self.docs.read().unwrap();
        let list = map.get(tenant).cloned().unwrap_or_default();
        let id_set: std::collections::HashSet<_> = ids.iter().collect();
        Ok(list
            .into_iter()
            .filter(|d| id_set.contains(&d.id))
            .collect())
    }

    async fn list(
        &self,
        tenant: &TenantId,
        limit: u32,
        _knowledge_base_ids: &[KnowledgeBaseId],
    ) -> Result<Vec<DocumentMeta>, PortError> {
        let map = self.docs.read().unwrap();
        let list = map.get(tenant).cloned().unwrap_or_default();
        Ok(list.into_iter().take(limit as usize).collect())
    }

    async fn list_knowledge_bases(
        &self,
        tenant: &TenantId,
    ) -> Result<Vec<KnowledgeBaseSummary>, PortError> {
        let map = self.kbs.read().unwrap();
        Ok(map.get(tenant).cloned().unwrap_or_default())
    }

    async fn resolve_api_key(&self, key: &str) -> Result<Option<AuthContext>, PortError> {
        let map = self.api_keys.read().unwrap();
        Ok(map.get(key).cloned())
    }
}

/// Fake in-memory chunk store.
#[derive(Clone, Default)]
pub struct FakeChunkStore {
    pub vector_hits: Arc<RwLock<Vec<SearchHit>>>,
    pub bm25_hits: Arc<RwLock<Vec<SearchHit>>>,
    pub neighbour_hits: Arc<RwLock<Vec<SearchHit>>>,
    pub figure_hits: Arc<RwLock<Vec<SearchHit>>>,
    pub fail_vector: Arc<AtomicBool>,
    pub fail_bm25: Arc<AtomicBool>,
    pub fail_neighbours: Arc<AtomicBool>,
    pub fail_figures: Arc<AtomicBool>,
}

#[async_trait]
impl ChunkStore for FakeChunkStore {
    async fn delete_by_document(&self, _document_id: &DocumentId) -> Result<u64, PortError> {
        Ok(0)
    }

    async fn chunk_counts(
        &self,
        _document_ids: &[DocumentId],
    ) -> Result<Vec<(DocumentId, u32)>, PortError> {
        Ok(Vec::new())
    }

    async fn ordered_chunks(
        &self,
        _document_id: &DocumentId,
    ) -> Result<Vec<(u32, String, ChunkMetadata)>, PortError> {
        Ok(Vec::new())
    }

    async fn vector_search(
        &self,
        scope: &DocumentScope,
        _embedding: &[f32],
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        if self.fail_vector.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced vector failure".to_string()));
        }
        let list = self.vector_hits.read().unwrap();
        let allowed = scope.document_ids();
        let filtered: Vec<SearchHit> = list
            .iter()
            .filter(|h| allowed.contains(&h.chunk.document_id))
            .take(limit as usize)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn bm25_search(
        &self,
        scope: &DocumentScope,
        _query: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        if self.fail_bm25.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced bm25 failure".to_string()));
        }
        let list = self.bm25_hits.read().unwrap();
        let allowed = scope.document_ids();
        let filtered: Vec<SearchHit> = list
            .iter()
            .filter(|h| allowed.contains(&h.chunk.document_id))
            .take(limit as usize)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        _anchors: &[NeighbourAnchor],
        _window: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        if self.fail_neighbours.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable(
                "forced neighbours failure".to_string(),
            ));
        }
        let list = self.neighbour_hits.read().unwrap();
        let allowed = scope.document_ids();
        let filtered: Vec<SearchHit> = list
            .iter()
            .filter(|h| allowed.contains(&h.chunk.document_id))
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        _query: &str,
        _retrieved_text: &str,
        _doc_ids: &[DocumentId],
        _anchored_chunks: &[(DocumentId, u32)],
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        if self.fail_figures.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced figures failure".to_string()));
        }
        let list = self.figure_hits.read().unwrap();
        let allowed = scope.document_ids();
        let filtered: Vec<SearchHit> = list
            .iter()
            .filter(|h| allowed.contains(&h.chunk.document_id))
            .take(limit as usize)
            .cloned()
            .collect();
        Ok(filtered)
    }
}

/// Fake in-memory embedder.
#[derive(Clone, Default)]
pub struct FakeEmbedder {
    pub fail: Arc<AtomicBool>,
}

#[async_trait]
impl Embedder for FakeEmbedder {
    async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, PortError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced embed failure".to_string()));
        }
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced embed failure".to_string()));
        }
        Ok(texts.iter().map(|_| vec![0.1, 0.2, 0.3, 0.4]).collect())
    }

    fn dimension(&self) -> u32 {
        4
    }
}

/// Fake in-memory reranker.
#[derive(Clone, Default)]
pub struct FakeReranker {
    pub fail: Arc<AtomicBool>,
}

#[async_trait]
impl Reranker for FakeReranker {
    async fn rerank(
        &self,
        _query: &str,
        candidates: Vec<RerankCandidate>,
        top_k: u32,
    ) -> Result<Vec<RerankResult>, PortError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced rerank failure".to_string()));
        }
        let mut results = Vec::new();
        for (i, c) in candidates.into_iter().take(top_k as usize).enumerate() {
            results.push(RerankResult {
                id: c.id,
                final_rank: i as u32,
                score: 1.0 - (i as f32 * 0.05),
            });
        }
        Ok(results)
    }
}
