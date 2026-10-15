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

/// Timestamp the fakes stamp on every row they create.
const FAKE_TIMESTAMP: &str = "2026-01-01T00:00:00.000Z";

/// Monotonic id source for rows the fakes create themselves. A counter keeps
/// `kb-engine` free of a uuid dependency and makes ids readable in failures.
fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

use kb_ports::{
    AuthContext, BlobStore, CategoryRecord, Chunk, ChunkId, ChunkMetadata, ChunkStore,
    ChunkToStore, ChunkType, CreateCategory, CreateJobInput, CreateKnowledgeBase,
    DocumentDownloadMeta, DocumentId, DocumentIntelligenceData, DocumentMeta, DocumentScope,
    DocumentStore, Embedder, Entity, EntityExtractor, EntityStore, EntityView, ExtractionOptions,
    ExtractionResult, Extractor, FigureGate, FigureGateCandidate, JobRecord, JobStore, JobView,
    KnowledgeBaseId, KnowledgeBaseRecord, KnowledgeBaseSummary, NeighbourAnchor, NewDocument,
    PatchCategory, PatchDocument, PatchKnowledgeBase, PortError, ProgressSink, ReapableJob,
    Relation, RelationView, RerankCandidate, RerankResult, Reranker, RetryJobOutcome, ScopeRequest,
    SearchHit, StoredChunk, TenantId, adapter::ScopeToken,
};
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// Metadata as read back from a stored chunk's JSON metadata object.
fn stored_chunk_metadata(value: &serde_json::Value) -> ChunkMetadata {
    let get_u32 = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as u32)
    };
    ChunkMetadata {
        section: value
            .get("section")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        page: get_u32("page"),
        chunk_index: get_u32("chunkIndex"),
        asset_key: value
            .get("assetKey")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        anchor_chunk_index: get_u32("anchorChunkIndex"),
        contextual_prefix: value
            .get("contextualPrefix")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    }
}

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
    pub kb_records: Arc<RwLock<HashMap<TenantId, Vec<KnowledgeBaseRecord>>>>,
    pub categories: Arc<RwLock<HashMap<TenantId, Vec<CategoryRecord>>>>,
    pub download_metas: Arc<RwLock<HashMap<(TenantId, DocumentId), DocumentDownloadMeta>>>,
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

    async fn find_meta_by_id(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
        include_deleted: bool,
    ) -> Result<Option<DocumentMeta>, PortError> {
        let map = self.docs.read().unwrap();
        if let Some(doc) = map
            .get(tenant)
            .and_then(|list| list.iter().find(|d| &d.id == id))
        {
            return Ok(Some(doc.clone()));
        }
        if include_deleted {
            // The fake keeps soft-deleted rows in their own list, so the hard
            // delete that follows a soft delete still finds the document.
            return Ok(self
                .deleted
                .read()
                .unwrap()
                .iter()
                .find(|d| &d.id == id)
                .cloned());
        }
        Ok(None)
    }

    async fn soft_delete(&self, tenant: &TenantId, id: &DocumentId) -> Result<bool, PortError> {
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
        if let Some(list) = map.get_mut(tenant) {
            if let Some(pos) = list.iter().position(|d| &d.id == id) {
                list.remove(pos);
                // The fake keeps no blob keys, so none is reported.
                return Ok(Some(None));
            }
        }
        // A soft delete moved the row aside; a hard delete still removes it,
        // which is what the delete route is allowed to do.
        let mut deleted = self.deleted.write().unwrap();
        let Some(pos) = deleted.iter().position(|d| &d.id == id) else {
            return Ok(None);
        };
        deleted.remove(pos);
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
        knowledge_base_ids: &[KnowledgeBaseId],
    ) -> Result<Vec<DocumentMeta>, PortError> {
        let map = self.docs.read().unwrap();
        let list = map.get(tenant).cloned().unwrap_or_default();
        // An empty filter is no filter, matching `SELECT_DOCS_WITH_KB` only
        // being used when a knowledge base was actually asked for.
        let filtered: Vec<DocumentMeta> = list
            .into_iter()
            .filter(|d| {
                knowledge_base_ids.is_empty()
                    || d.knowledge_base_ids
                        .iter()
                        .any(|id| knowledge_base_ids.iter().any(|k| k.0 == *id))
            })
            .collect();
        Ok(filtered.into_iter().take(limit as usize).collect())
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

    async fn update_metadata(
        &self,
        id: &DocumentId,
        patch: &serde_json::Value,
    ) -> Result<(), PortError> {
        let mut map = self.docs.write().unwrap();
        for list in map.values_mut() {
            if let Some(doc) = list.iter_mut().find(|d| &d.id == id) {
                let mut current = doc
                    .metadata
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({}));
                if let (Some(cur_obj), Some(patch_obj)) =
                    (current.as_object_mut(), patch.as_object())
                {
                    for (k, v) in patch_obj {
                        cur_obj.insert(k.clone(), v.clone());
                    }
                } else {
                    current = patch.clone();
                }
                doc.metadata = Some(current);
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

    async fn create_document(
        &self,
        doc: &NewDocument,
        group_kb_ids: &[KnowledgeBaseId],
    ) -> Result<(), PortError> {
        let meta = DocumentMeta {
            id: doc.id.clone(),
            title: doc.title.clone(),
            categories: doc.categories.clone(),
            subcategory: doc.subcategory.clone(),
            status: doc.status.clone(),
            chunk_count: 0,
            // The upload route stores the original's MIME type; `fileType` is
            // only filled in by the worker after extraction.
            file_type: None,
            file_size: doc.file_size,
            mime_type: doc.mime_type.clone(),
            s3_key: doc.s3_key.clone(),
            created_at: FAKE_TIMESTAMP.to_string(),
            updated_at: FAKE_TIMESTAMP.to_string(),
            knowledge_base_ids: group_kb_ids.iter().map(|k| k.0.clone()).collect(),
            metadata: None,
        };
        self.insert_doc(doc.tenant_id.clone(), meta);
        self.download_metas.write().unwrap().insert(
            (doc.tenant_id.clone(), doc.id.clone()),
            DocumentDownloadMeta {
                id: doc.id.clone(),
                s3_key: doc.s3_key.clone(),
                mime_type: doc.mime_type.clone(),
                title: doc.title.clone(),
            },
        );
        Ok(())
    }

    async fn patch_document(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
        patch: &PatchDocument,
    ) -> Result<Option<DocumentMeta>, PortError> {
        let mut map = self.docs.write().unwrap();
        let Some(list) = map.get_mut(tenant) else {
            return Ok(None);
        };
        let Some(doc) = list.iter_mut().find(|d| &d.id == id) else {
            return Ok(None);
        };
        if let Some(t) = &patch.title {
            doc.title = t.clone();
        }
        if let Some(c) = &patch.categories {
            doc.categories = c.clone();
        }
        if let Some(s) = &patch.subcategory {
            doc.subcategory = s.clone();
        }
        if let Some(k) = &patch.knowledge_base_ids {
            doc.knowledge_base_ids = k.iter().map(|kb| kb.0.clone()).collect();
        }
        doc.updated_at = FAKE_TIMESTAMP.to_string();
        Ok(Some(doc.clone()))
    }

    async fn find_for_download(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
    ) -> Result<Option<DocumentDownloadMeta>, PortError> {
        if let Some(meta) = self
            .download_metas
            .read()
            .unwrap()
            .get(&(tenant.clone(), id.clone()))
        {
            return Ok(Some(meta.clone()));
        }
        let docs = self.docs.read().unwrap();
        let Some(doc) = docs
            .get(tenant)
            .and_then(|list| list.iter().find(|m| &m.id == id))
        else {
            return Ok(None);
        };
        Ok(Some(DocumentDownloadMeta {
            id: doc.id.clone(),
            s3_key: doc.s3_key.clone(),
            mime_type: doc.mime_type.clone(),
            title: doc.title.clone(),
        }))
    }

    async fn filter_owned_kb_ids(
        &self,
        tenant: &TenantId,
        kb_ids: &[KnowledgeBaseId],
    ) -> Result<Vec<KnowledgeBaseId>, PortError> {
        let map = self.kb_records.read().unwrap();
        let owned = map.get(tenant).cloned().unwrap_or_default();
        let owned: std::collections::HashSet<&str> =
            owned.iter().map(|k| k.id.0.as_str()).collect();
        Ok(kb_ids
            .iter()
            .filter(|k| owned.contains(k.0.as_str()))
            .cloned()
            .collect())
    }

    async fn create_knowledge_base(
        &self,
        tenant: &TenantId,
        input: &CreateKnowledgeBase,
    ) -> Result<KnowledgeBaseRecord, PortError> {
        let record = KnowledgeBaseRecord {
            id: input.id.clone(),
            tenant_id: tenant.clone(),
            name: input.name.clone(),
            description: input.description.clone(),
            color: input.color.clone(),
            created_at: FAKE_TIMESTAMP.to_string(),
            updated_at: FAKE_TIMESTAMP.to_string(),
        };
        self.kb_records
            .write()
            .unwrap()
            .entry(tenant.clone())
            .or_default()
            .push(record.clone());
        self.kbs
            .write()
            .unwrap()
            .entry(tenant.clone())
            .or_default()
            .push(KnowledgeBaseSummary {
                id: input.id.clone(),
                name: input.name.clone(),
                description: input.description.clone(),
                color: input.color.clone(),
                document_count: 0,
            });
        Ok(record)
    }

    async fn patch_knowledge_base(
        &self,
        tenant: &TenantId,
        id: &KnowledgeBaseId,
        patch: &PatchKnowledgeBase,
    ) -> Result<Option<KnowledgeBaseRecord>, PortError> {
        let mut map = self.kb_records.write().unwrap();
        let Some(list) = map.get_mut(tenant) else {
            return Ok(None);
        };
        let Some(rec) = list.iter_mut().find(|k| &k.id == id) else {
            return Ok(None);
        };
        if let Some(n) = &patch.name {
            rec.name = n.clone();
        }
        if let Some(d) = &patch.description {
            rec.description = d.clone();
        }
        if let Some(c) = &patch.color {
            rec.color = c.clone();
        }
        rec.updated_at = FAKE_TIMESTAMP.to_string();
        Ok(Some(rec.clone()))
    }

    async fn delete_knowledge_base(
        &self,
        tenant: &TenantId,
        id: &KnowledgeBaseId,
    ) -> Result<bool, PortError> {
        let mut map = self.kb_records.write().unwrap();
        let Some(list) = map.get_mut(tenant) else {
            return Ok(false);
        };
        let Some(pos) = list.iter().position(|k| k.id.0 == id.0) else {
            return Ok(false);
        };
        list.remove(pos);
        let mut kbs = self.kbs.write().unwrap();
        if let Some(kblist) = kbs.get_mut(tenant) {
            kblist.retain(|k| k.id.0 != id.0);
        }
        Ok(true)
    }

    async fn list_categories(&self, tenant: &TenantId) -> Result<Vec<CategoryRecord>, PortError> {
        let map = self.categories.read().unwrap();
        Ok(map.get(tenant).cloned().unwrap_or_default())
    }

    async fn create_category(
        &self,
        tenant: &TenantId,
        input: &CreateCategory,
    ) -> Result<CategoryRecord, PortError> {
        let record = CategoryRecord {
            id: input.id.clone(),
            name: input.name.clone(),
            label: input.label.clone(),
            color: input.color.clone(),
        };
        self.categories
            .write()
            .unwrap()
            .entry(tenant.clone())
            .or_default()
            .push(record.clone());
        Ok(record)
    }

    async fn patch_category(
        &self,
        tenant: &TenantId,
        id: &str,
        patch: &PatchCategory,
    ) -> Result<Option<CategoryRecord>, PortError> {
        let mut map = self.categories.write().unwrap();
        let Some(list) = map.get_mut(tenant) else {
            return Ok(None);
        };
        let Some(rec) = list.iter_mut().find(|c| c.id == id) else {
            return Ok(None);
        };
        if let Some(n) = &patch.name {
            rec.name = n.clone();
        }
        if let Some(l) = &patch.label {
            rec.label = l.clone();
        }
        if let Some(c) = &patch.color {
            rec.color = c.clone();
        }
        Ok(Some(rec.clone()))
    }

    async fn delete_category(&self, tenant: &TenantId, id: &str) -> Result<bool, PortError> {
        let mut map = self.categories.write().unwrap();
        let Some(list) = map.get_mut(tenant) else {
            return Ok(false);
        };
        let Some(pos) = list.iter().position(|c| c.id == id) else {
            return Ok(false);
        };
        list.remove(pos);
        Ok(true)
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
        document_id: &DocumentId,
    ) -> Result<Vec<StoredChunk>, PortError> {
        let stored = self.stored_chunks.read().unwrap();
        let Some(chunks) = stored.get(document_id) else {
            return Ok(Vec::new());
        };
        // `ChunkToStore.metadata` is the SurrealDB metadata object verbatim, so
        // the read shape is derived from it the same way the Surreal adapter
        // derives it, and the record id follows the store's own naming.
        let mut out: Vec<StoredChunk> = chunks
            .iter()
            .map(|c| {
                let meta = stored_chunk_metadata(&c.metadata);
                StoredChunk {
                    id: format!("{document_id}_{}", meta.chunk_index.unwrap_or(0)),
                    chunk_index: meta.chunk_index.unwrap_or(0),
                    content: c.content.clone(),
                    chunk_type: c
                        .metadata
                        .get("chunkType")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    metadata: meta,
                }
            })
            .collect();
        out.sort_by_key(|c| c.chunk_index);
        Ok(out)
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
    pub pending: Arc<RwLock<std::collections::VecDeque<JobRecord>>>,
    /// `(job_id, step, progress)` in call order.
    pub progress: Arc<RwLock<Vec<(String, String, u32)>>>,
    /// Detailed step updates with sub-counters.
    pub detailed_progress: Arc<RwLock<Vec<(String, String, u32, Option<u32>, Option<u32>)>>>,
    pub finished: Arc<RwLock<Vec<FinishedJob>>>,
    pub reclaimed: Arc<RwLock<u64>>,
    /// Job ids passed to `touch`, in call order: the heartbeat's trail.
    pub touches: Arc<RwLock<Vec<String>>>,
    pub job_views: Arc<RwLock<HashMap<(TenantId, String), JobView>>>,
    pub reapables: Arc<RwLock<Vec<ReapableJob>>>,
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
        step_current: Option<u32>,
        step_total: Option<u32>,
        eta_seconds: Option<i32>,
    ) -> Result<(), PortError> {
        self.progress
            .write()
            .unwrap()
            .push((job_id.to_string(), step.to_string(), progress));
        self.detailed_progress.write().unwrap().push((
            job_id.to_string(),
            step.to_string(),
            progress,
            step_current,
            step_total,
        ));
        let mut views = self.job_views.write().unwrap();
        for ((_, jid), view) in views.iter_mut() {
            if jid == job_id {
                view.step = Some(step.to_string());
                view.progress = progress;
                view.step_current = step_current;
                view.step_total = step_total;
                view.eta_seconds = eta_seconds;
            }
        }
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
        let mut views = self.job_views.write().unwrap();
        for ((_, jid), view) in views.iter_mut() {
            if jid == job_id {
                view.status = "success".to_string();
                view.step = Some("done".to_string());
                view.progress = 100;
                view.document_id = Some(document_id.0.clone());
            }
        }
        Ok(())
    }

    async fn finish_failure(&self, job_id: &str, error: &str) -> Result<(), PortError> {
        self.finished.write().unwrap().push(FinishedJob {
            job_id: job_id.to_string(),
            success: false,
            detail: error.to_string(),
        });
        let mut views = self.job_views.write().unwrap();
        for ((_, jid), view) in views.iter_mut() {
            if jid == job_id {
                view.status = "failed".to_string();
                view.error = Some(error.to_string());
            }
        }
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

    async fn create_job(&self, input: &CreateJobInput) -> Result<String, PortError> {
        let job_id = format!("job-{}", next_id());
        let view = JobView {
            id: job_id.clone(),
            status: "pending".to_string(),
            step: Some("queued".to_string()),
            progress: 0,
            step_current: None,
            step_total: None,
            eta_seconds: None,
            // Postgres defaults `attempt` to 1.
            attempt: 1,
            error: None,
            document_id: input.document_id.as_ref().map(|d| d.0.clone()),
        };
        self.job_views
            .write()
            .unwrap()
            .insert((input.tenant_id.clone(), job_id.clone()), view);
        self.pending.write().unwrap().push_back(JobRecord {
            id: job_id.clone(),
            tenant_id: input.tenant_id.clone(),
            user_id: input.user_id.clone(),
            document_id: input.document_id.clone(),
            s3_key: input.s3_key.clone(),
            filename: input.filename.clone(),
            mime_type: input.mime_type.clone(),
            attempt: 1,
            params: Some(input.params.clone()),
        });
        Ok(job_id)
    }

    async fn get_job(&self, tenant: &TenantId, job_id: &str) -> Result<Option<JobView>, PortError> {
        let map = self.job_views.read().unwrap();
        Ok(map.get(&(tenant.clone(), job_id.to_string())).cloned())
    }

    async fn retry_job(&self, tenant: &TenantId, id: &str) -> Result<RetryJobOutcome, PortError> {
        let mut map = self.job_views.write().unwrap();
        let Some(view) = map.get_mut(&(tenant.clone(), id.to_string())) else {
            return Ok(RetryJobOutcome::NotFound);
        };
        if view.status != "failed" {
            return Ok(RetryJobOutcome::NotFailed);
        }
        view.status = "pending".to_string();
        view.attempt += 1;
        view.error = None;
        view.step = Some("queued".to_string());
        view.progress = 0;
        view.step_current = None;
        view.step_total = None;
        view.eta_seconds = None;
        Ok(RetryJobOutcome::Success {
            job_id: view.id.clone(),
            document_id: view.document_id.clone(),
        })
    }

    async fn list_reapable(
        &self,
        _max_age_days: u32,
        _limit: u32,
    ) -> Result<Vec<ReapableJob>, PortError> {
        Ok(self.reapables.read().unwrap().clone())
    }

    async fn clear_s3_key(&self, job_id: &str) -> Result<(), PortError> {
        let mut list = self.reapables.write().unwrap();
        list.retain(|j| j.id != job_id);
        Ok(())
    }
}

/// Extractor that returns fixed text, or fails on demand.
#[derive(Clone, Default)]
pub struct FakeExtractor {
    pub result: Arc<RwLock<Option<ExtractionResult>>>,
    pub content: Arc<RwLock<String>>,
    pub file_type: Arc<RwLock<String>>,
    pub fail: Arc<AtomicBool>,
    pub calls: Arc<AtomicBool>,
}

impl FakeExtractor {
    /// Build an extractor that returns `content` as `file_type`.
    pub fn returning(content: &str, file_type: &str) -> Self {
        Self {
            result: Arc::new(RwLock::new(None)),
            content: Arc::new(RwLock::new(content.to_string())),
            file_type: Arc::new(RwLock::new(file_type.to_string())),
            fail: Arc::new(AtomicBool::new(false)),
            calls: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Build an extractor that returns a custom `ExtractionResult`.
    pub fn returning_result(result: ExtractionResult) -> Self {
        Self {
            content: Arc::new(RwLock::new(result.content.clone())),
            file_type: Arc::new(RwLock::new(result.file_type.clone())),
            result: Arc::new(RwLock::new(Some(result))),
            fail: Arc::new(AtomicBool::new(false)),
            calls: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl Extractor for FakeExtractor {
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

    async fn extract_with_options(
        &self,
        _filename: &str,
        _mime_type: &str,
        _file_bytes: &[u8],
        _options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        self.calls.store(true, Ordering::Relaxed);
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced extract failure".to_string()));
        }
        if let Some(res) = self.result.read().unwrap().as_ref() {
            return Ok(res.clone());
        }
        Ok(ExtractionResult {
            content: self.content.read().unwrap().clone(),
            file_type: self.file_type.read().unwrap().clone(),
            ..ExtractionResult::default()
        })
    }
}

/// Fake in-memory entity store.
#[derive(Clone, Default)]
pub struct FakeEntityStore {
    #[allow(clippy::type_complexity)]
    pub upserted_entities: Arc<
        RwLock<
            Vec<(
                DocumentId,
                String,
                String,
                String,
                f32,
                Option<serde_json::Value>,
            )>,
        >,
    >,
    pub relations: Arc<RwLock<Vec<(String, String, String, f32, DocumentId, Option<String>)>>>,
    pub intelligences: Arc<RwLock<HashMap<DocumentId, DocumentIntelligenceData>>>,
    pub fail_upsert: Arc<AtomicBool>,
}

#[async_trait]
impl EntityStore for FakeEntityStore {
    async fn upsert_entity(
        &self,
        document_id: &DocumentId,
        sanitized_name: &str,
        name: &str,
        entity_type: &str,
        confidence: f32,
        metadata: Option<&serde_json::Value>,
    ) -> Result<(), PortError> {
        if self.fail_upsert.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable(
                "forced entity upsert failure".into(),
            ));
        }
        self.upserted_entities.write().unwrap().push((
            document_id.clone(),
            sanitized_name.to_string(),
            name.to_string(),
            entity_type.to_string(),
            confidence,
            metadata.cloned(),
        ));
        Ok(())
    }

    async fn relate_entities(
        &self,
        source_id: &str,
        rel_type: &str,
        target_id: &str,
        confidence: f32,
        document_id: &DocumentId,
        context: Option<&str>,
    ) -> Result<(), PortError> {
        self.relations.write().unwrap().push((
            source_id.to_string(),
            rel_type.to_string(),
            target_id.to_string(),
            confidence,
            document_id.clone(),
            context.map(|s| s.to_string()),
        ));
        Ok(())
    }

    async fn get_document_intelligence(
        &self,
        document_id: &DocumentId,
    ) -> Result<DocumentIntelligenceData, PortError> {
        if let Some(data) = self.intelligences.read().unwrap().get(document_id) {
            return Ok(data.clone());
        }
        let entities: Vec<EntityView> = self
            .upserted_entities
            .read()
            .unwrap()
            .iter()
            .filter(|(doc, ..)| doc == document_id)
            .map(|(_, id, name, etype, conf, meta)| EntityView {
                id: id.clone(),
                name: name.clone(),
                r#type: etype.clone(),
                confidence: *conf,
                metadata: meta.clone().unwrap_or_else(|| serde_json::json!({})),
            })
            .collect();
        let relations: Vec<RelationView> = self
            .relations
            .read()
            .unwrap()
            .iter()
            .filter(|(_, _, _, _, doc, _)| doc == document_id)
            .map(|(from, rel, to, conf, _, ctx)| RelationView {
                id: format!("{from}-{rel}-{to}"),
                from_id: from.clone(),
                out: to.clone(),
                relation_type: rel.clone(),
                confidence: *conf,
                metadata: serde_json::json!({ "context": ctx }),
            })
            .collect();

        Ok(DocumentIntelligenceData {
            entities,
            relations,
        })
    }
}

/// Fake progress sink recording emitted events.
#[derive(Clone, Default)]
pub struct FakeProgressSink {
    pub events: Arc<RwLock<Vec<(TenantId, String, serde_json::Value)>>>,
}

#[async_trait]
impl ProgressSink for FakeProgressSink {
    async fn emit(
        &self,
        tenant: &TenantId,
        event: &str,
        payload: serde_json::Value,
    ) -> Result<(), PortError> {
        self.events
            .write()
            .unwrap()
            .push((tenant.clone(), event.to_string(), payload));
        Ok(())
    }
}

/// Fake entity extractor.
#[derive(Clone, Default)]
pub struct FakeEntityExtractor {
    pub entities: Arc<RwLock<Vec<Entity>>>,
    pub relations: Arc<RwLock<Vec<Relation>>>,
    pub fail: Arc<AtomicBool>,
}

#[async_trait]
impl EntityExtractor for FakeEntityExtractor {
    async fn extract_entities(
        &self,
        _text: &str,
        _document_id: Option<&str>,
        _chunk_id: Option<&str>,
    ) -> Result<Vec<Entity>, PortError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced extractor failure".into()));
        }
        Ok(self.entities.read().unwrap().clone())
    }

    async fn extract_entities_and_relations(
        &self,
        _text: &str,
        _document_id: Option<&str>,
        _user_id: Option<&str>,
    ) -> Result<(Vec<Entity>, Vec<Relation>), PortError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced extractor failure".into()));
        }
        Ok((
            self.entities.read().unwrap().clone(),
            self.relations.read().unwrap().clone(),
        ))
    }
}

/// Fake figure gate veto implementation.
#[derive(Clone, Default)]
pub struct FakeFigureGate {
    pub kept_ids: Arc<RwLock<Vec<String>>>,
    pub fail: Arc<AtomicBool>,
}

#[async_trait]
impl FigureGate for FakeFigureGate {
    async fn gate_figures(
        &self,
        _query: &str,
        candidates: &[FigureGateCandidate],
    ) -> Result<Vec<String>, PortError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced figure gate failure".into()));
        }
        let kept = self.kept_ids.read().unwrap().clone();
        if kept.is_empty() {
            Ok(candidates.iter().map(|c| c.id.clone()).collect())
        } else {
            Ok(kept)
        }
    }
}
