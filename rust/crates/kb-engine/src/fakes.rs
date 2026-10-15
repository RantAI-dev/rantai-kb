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
    AuthContext, BlobStore, Chunk, ChunkId, ChunkMetadata, ChunkStore, ChunkToStore, ChunkType,
    DocumentId, DocumentMeta, DocumentScope, DocumentStore, Embedder, ExtractionResult, Extractor,
    JobRecord, JobStore, KnowledgeBaseId, KnowledgeBaseSummary, NeighbourAnchor, PortError,
    RerankCandidate, RerankResult, Reranker, ScopeRequest, SearchHit, TenantId,
    adapter::ScopeToken,
};
use std::time::Duration;

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
        scope: &DocumentScope,
        id: &DocumentId,
    ) -> Result<Option<DocumentMeta>, PortError> {
        // Mirror PgDocumentStore: a scope is the visibility rule, so a lookup
        // outside it must miss even when the id exists for another tenant.
        if !scope.document_ids().contains(id) {
            return Ok(None);
        }
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

    async fn update_extracted_content(
        &self,
        id: &DocumentId,
        _content: &str,
        file_type: Option<&str>,
        file_size: Option<i32>,
    ) -> Result<(), PortError> {
        let mut map = self.docs.write().unwrap();
        for list in map.values_mut() {
            if let Some(doc) = list.iter_mut().find(|d| &d.id == id) {
                if let Some(v) = file_type {
                    doc.file_type = Some(v.to_string());
                }
                if let Some(v) = file_size {
                    doc.file_size = Some(v);
                }
                doc.updated_at = "2026-01-01T00:00:00Z".to_string();
                return Ok(());
            }
        }
        Ok(())
    }

    async fn set_status(&self, id: &DocumentId, status: &str) -> Result<(), PortError> {
        let mut map = self.docs.write().unwrap();
        for list in map.values_mut() {
            if let Some(doc) = list.iter_mut().find(|d| &d.id == id) {
                doc.status = status.to_string();
                return Ok(());
            }
        }
        Ok(())
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
    /// Chunks written through `store_chunks`, keyed by document id. Also
    /// backs `chunk_counts`, so tests see the same numbers a real store would.
    pub stored_chunks: Arc<RwLock<HashMap<DocumentId, Vec<ChunkToStore>>>>,
    pub fail_store: Arc<AtomicBool>,
    pub fail_vector: Arc<AtomicBool>,
    pub fail_bm25: Arc<AtomicBool>,
    pub fail_neighbours: Arc<AtomicBool>,
    pub fail_figures: Arc<AtomicBool>,
}

#[async_trait]
impl ChunkStore for FakeChunkStore {
    async fn delete_by_document(&self, document_id: &DocumentId) -> Result<u64, PortError> {
        let removed = self
            .stored_chunks
            .write()
            .unwrap()
            .remove(document_id)
            .map(|v| v.len() as u64)
            .unwrap_or(0);
        Ok(removed)
    }

    async fn store_chunks(
        &self,
        document_id: &DocumentId,
        chunks: &[ChunkToStore],
        _embedding_model: Option<&str>,
    ) -> Result<(), PortError> {
        if self.fail_store.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced store failure".to_string()));
        }
        self.stored_chunks
            .write()
            .unwrap()
            .insert(document_id.clone(), chunks.to_vec());
        Ok(())
    }

    async fn chunk_counts(
        &self,
        document_ids: &[DocumentId],
    ) -> Result<Vec<(DocumentId, u32)>, PortError> {
        let stored = self.stored_chunks.read().unwrap();
        let mut out = Vec::new();
        for id in document_ids {
            let n = stored.get(id).map_or(0, |c| c.len() as u32);
            if n > 0 {
                out.push((id.clone(), n));
            }
        }
        Ok(out)
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

/// In-memory object store. Keys are exact; a missing key is an error, the
/// same way a real `GET` on an absent object is.
#[derive(Clone, Default)]
pub struct FakeBlobStore {
    pub objects: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

#[async_trait]
impl BlobStore for FakeBlobStore {
    async fn upload(
        &self,
        key: &str,
        body: Vec<u8>,
        _content_type: &str,
    ) -> Result<u64, PortError> {
        let len = body.len() as u64;
        self.objects.write().unwrap().insert(key.to_string(), body);
        Ok(len)
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, PortError> {
        self.objects
            .read()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| PortError::Unavailable(format!("fake blob not found: {key}")))
    }

    async fn delete(&self, key: &str) -> Result<(), PortError> {
        self.objects.write().unwrap().remove(key);
        Ok(())
    }
}

/// One terminal write recorded by [`FakeJobStore`].
#[derive(Debug, Clone)]
pub struct FinishedJob {
    pub job_id: String,
    pub success: bool,
    pub detail: String,
}

/// In-memory ingest queue. Claiming pops the oldest pending row under a single
/// write lock, so two concurrent workers can never take the same job — the
/// same guarantee `FOR UPDATE SKIP LOCKED` gives in Postgres.
#[derive(Clone, Default)]
pub struct FakeJobStore {
    pending: Arc<RwLock<std::collections::VecDeque<JobRecord>>>,
    /// `(job_id, step, progress)` in call order.
    pub progress: Arc<RwLock<Vec<(String, String, u32)>>>,
    pub finished: Arc<RwLock<Vec<FinishedJob>>>,
    pub reclaimed: Arc<RwLock<u64>>,
    /// Job ids passed to `touch`, in call order: the heartbeat's trail.
    pub touches: Arc<RwLock<Vec<String>>>,
}

impl FakeJobStore {
    /// Queue a job for the next claim.
    pub fn push(&self, job: JobRecord) {
        self.pending.write().unwrap().push_back(job);
    }

    pub fn pending_len(&self) -> usize {
        self.pending.read().unwrap().len()
    }
}

#[async_trait]
impl JobStore for FakeJobStore {
    async fn claim_next_pending(&self) -> Result<Option<JobRecord>, PortError> {
        Ok(self.pending.write().unwrap().pop_front())
    }

    async fn update_progress(
        &self,
        job_id: &str,
        step: &str,
        progress: u32,
        _step_current: Option<u32>,
        _step_total: Option<u32>,
        _eta_seconds: Option<i32>,
    ) -> Result<(), PortError> {
        self.progress
            .write()
            .unwrap()
            .push((job_id.to_string(), step.to_string(), progress));
        Ok(())
    }

    async fn finish_success(
        &self,
        job_id: &str,
        document_id: &DocumentId,
    ) -> Result<(), PortError> {
        self.finished.write().unwrap().push(FinishedJob {
            job_id: job_id.to_string(),
            success: true,
            detail: document_id.0.clone(),
        });
        Ok(())
    }

    async fn finish_failure(&self, job_id: &str, error: &str) -> Result<(), PortError> {
        self.finished.write().unwrap().push(FinishedJob {
            job_id: job_id.to_string(),
            success: false,
            detail: error.to_string(),
        });
        Ok(())
    }

    async fn touch(&self, job_id: &str) -> Result<(), PortError> {
        self.touches.write().unwrap().push(job_id.to_string());
        Ok(())
    }

    async fn reclaim_stale(
        &self,
        _stale_after: Duration,
        _max_attempts: i32,
    ) -> Result<u64, PortError> {
        Ok(*self.reclaimed.read().unwrap())
    }
}

/// Extractor that returns fixed text, or fails on demand.
#[derive(Clone, Default)]
pub struct FakeExtractor {
    pub content: Arc<RwLock<String>>,
    pub file_type: Arc<RwLock<String>>,
    pub fail: Arc<AtomicBool>,
    pub calls: Arc<AtomicBool>,
}

impl FakeExtractor {
    /// Build an extractor that returns `content` as `file_type`.
    pub fn returning(content: &str, file_type: &str) -> Self {
        Self {
            content: Arc::new(RwLock::new(content.to_string())),
            file_type: Arc::new(RwLock::new(file_type.to_string())),
            fail: Arc::new(AtomicBool::new(false)),
            calls: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl Extractor for FakeExtractor {
    async fn extract(
        &self,
        _filename: &str,
        _mime_type: &str,
        _file_bytes: &[u8],
    ) -> Result<ExtractionResult, PortError> {
        self.calls.store(true, Ordering::Relaxed);
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced extract failure".to_string()));
        }
        Ok(ExtractionResult {
            content: self.content.read().unwrap().clone(),
            file_type: self.file_type.read().unwrap().clone(),
            ..ExtractionResult::default()
        })
    }
}
