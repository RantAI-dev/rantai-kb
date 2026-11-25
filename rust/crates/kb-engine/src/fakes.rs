#![allow(clippy::unwrap_used, clippy::expect_used)]

//! In-memory fakes for all ports used by `kb-engine` and `kb-server`.
//!
//! Suitable for unit tests and in-process HTTP tests without network dependencies.

use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;

/// `(job_id, step, progress)` recorded by [`FakeJobStore::update_progress`].
pub type ProgressCall = (String, String, u32);
/// `(job_id, step, progress, step_current, step_total)` recorded by
/// [`FakeJobStore::update_detailed`].
pub type DetailedProgressCall = (String, String, u32, Option<u32>, Option<u32>);
/// `(document, kb ids)` passed to the reconciler's payload repair (SEC-2b).
pub type KbPayloadWrite = (DocumentId, Vec<String>);

/// Timestamp the fakes stamp on every row they create.
const FAKE_TIMESTAMP: &str = "2026-01-01T00:00:00.000Z";

/// Monotonic id source for rows the fakes create themselves. A counter keeps
/// `kb-engine` free of a uuid dependency and makes ids readable in failures.
fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

use kb_ports::{
    ArmRanks, ArmScores, AuthContext, BlobStore, Candidate, CategoryRecord, Chunk, ChunkId,
    ChunkMeta, ChunkMetadata, ChunkStore, ChunkToStore, ChunkType, CreateCategory, CreateJobInput,
    CreateKnowledgeBase, DocumentDownloadMeta, DocumentFilter, DocumentId,
    DocumentIntelligenceData, DocumentMeta, DocumentPage, DocumentScope, DocumentStore, Embedder,
    Entity, EntityExtractor, EntityStore, ExtractionOptions, ExtractionResult, Extractor,
    FigureGate, FigureGateCandidate, Fusion, HybridQuery, IndexCapabilities, IndexUnit, JobRecord,
    JobStore, JobView, KnowledgeBaseId, KnowledgeBaseRecord, KnowledgeBaseSummary, NeighbourAnchor,
    NewDocument, OutboxItem, OutboxLag, OutboxOp, PatchCategory, PatchDocument, PatchKnowledgeBase,
    PortError, PredicateSpec, ProgressSink, QueryEmbedding, ReapableJob, Relation, RerankCandidate,
    RerankResult, Reranker, RetrievalIndex, RetryJobOutcome, ScopeOutbox, ScopeRequest, SearchHit,
    StoredChunk, TableId, TableQueryFailure, TableRecord, TableStore, TenantId,
    adapter::ScopeToken, predicate_spec_for, scope_id_limit_from_env, sort_candidates,
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
        table_id: value
            .get("tableId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        // Version fields are filled from the document record at query time,
        // not from the chunk payload, exactly as `document_title` is.
        doc_key: None,
        version: None,
        effective_from: None,
        effective_to: None,
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
            external_ref: None,
            content: content.to_string(),
            chunk_type: ChunkType::Text,
            metadata: ChunkMetadata {
                chunk_index: Some(chunk_index),
                ..Default::default()
            },
            categories: Vec::new(),
            subcategory: None,
        },
        similarity,
        rank: 0,
        is_bm25: false,
        retrieval: None,
        dense_score: None,
        rerank_score: None,
    }
}

/// Helper to create a fake `DocumentScope`.
#[must_use]
pub fn make_fake_scope(tenant: TenantId, doc_ids: Vec<DocumentId>) -> DocumentScope {
    let token = ScopeToken::new_for_adapter();
    DocumentScope::new(&token, tenant, doc_ids)
}

/// Helper to create a fake predicate scope (SEC-2a).
#[must_use]
pub fn make_fake_predicate_scope(tenant: TenantId, spec: PredicateSpec) -> DocumentScope {
    let token = ScopeToken::new_for_adapter();
    DocumentScope::new_predicate(&token, tenant, spec)
}

/// A JSON scalar as Postgres `->>` renders it: strings unquoted, numbers and
/// booleans as their text, everything else unmatchable.
fn json_scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Whether `metadata[key]` equals one of `values`, or (when the document value
/// is an array) contains one of them. Mirrors `scope_sql` in `kb-store-pg`.
fn metadata_values_match(
    metadata: Option<&serde_json::Value>,
    key: &str,
    values: &[String],
) -> bool {
    let Some(value) = metadata
        .and_then(serde_json::Value::as_object)
        .and_then(|object| object.get(key))
    else {
        return false;
    };
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(json_scalar_text)
            .any(|item| values.contains(&item)),
        scalar => json_scalar_text(scalar).is_some_and(|item| values.contains(&item)),
    }
}

fn metadata_key_matches(metadata: Option<&serde_json::Value>, key: &str, value: &str) -> bool {
    metadata_values_match(metadata, key, &[value.to_string()])
}

/// The fake's copy of the Postgres scope predicate. `build_scope` and
/// `verify_visible` both call it, so the verify cannot disagree with the scope
/// it is checking — the same property the SQL adapter gets from `scope_sql`.
fn meta_matches_filter(meta: &DocumentMeta, filter: &DocumentFilter) -> bool {
    if !filter.knowledge_base_ids.is_empty()
        && !meta
            .knowledge_base_ids
            .iter()
            .any(|kb| filter.knowledge_base_ids.iter().any(|k| k.0 == *kb))
    {
        return false;
    }
    if let Some(cat) = &filter.category
        && !meta.categories.contains(cat)
    {
        return false;
    }
    if let Some(refs) = &filter.external_refs
        && !meta
            .external_ref
            .as_ref()
            .is_some_and(|r| refs.iter().any(|allowed| allowed == r))
    {
        return false;
    }
    if let Some(language) = &filter.language
        && !metadata_key_matches(meta.metadata.as_ref(), "language", language)
    {
        return false;
    }
    // RET-16 version scope. KEEP IN SYNC with `scope_sql_inner` in
    // `kb-store-pg/src/store.rs`: the same three modes, after every other
    // clause, so a version filter can only narrow the caller's scope. Being in
    // this shared predicate, it also governs the SEC-2a verify.
    if let Some(as_of) = &filter.as_of {
        // `asOf` wins over `includeHistory` and ignores isLatest.
        if meta
            .effective_from
            .as_ref()
            .is_some_and(|from| from.as_str() > as_of.as_str())
        {
            return false;
        }
        if meta
            .effective_to
            .as_ref()
            .is_some_and(|to| to.as_str() <= as_of.as_str())
        {
            return false;
        }
    } else if !filter.include_history && meta.doc_key.is_some() && !meta.is_latest {
        return false;
    }
    filter.metadata.iter().all(|predicate| {
        metadata_values_match(meta.metadata.as_ref(), &predicate.key, &predicate.values)
    })
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
    /// Fail the SEC-2a verify without failing `build_scope`, so a test can
    /// prove that a search fails closed when Postgres cannot verify.
    pub fail_verify: Arc<AtomicBool>,
    /// Test-only override for `KB_SCOPE_ID_LIMIT`. `None` reads the environment
    /// exactly as the Postgres adapter does.
    pub scope_id_limit: Option<usize>,
    /// Section outlines by document, for the tree arm (DEC-4).
    pub outlines: Arc<RwLock<HashMap<DocumentId, kb_core::tree::Outline>>>,
    /// Force [`DocumentStore::outlines`] to fail, so a test can assert the
    /// tree arm degrades instead of emptying the search.
    pub fail_outlines: Arc<AtomicBool>,
    /// Return every outline regardless of scope, the way the old unscoped
    /// `bm25_search_global` returned every tenant's hits. Only the red-team
    /// matrix sets this, to prove the scoped page read contains the hole.
    pub outline_scope_hole: Arc<AtomicBool>,
    /// Soft-deleted documents, kept so tests can assert on them.
    pub deleted: Arc<RwLock<Vec<DocumentMeta>>>,
}

impl FakeDocumentStore {
    pub fn insert_doc(&self, tenant: TenantId, meta: DocumentMeta) {
        let mut map = self.docs.write().unwrap();
        map.entry(tenant).or_default().push(meta);
    }

    /// Store a document's outline for the tree arm.
    pub fn insert_outline(&self, id: DocumentId, outline: kb_core::tree::Outline) {
        self.outlines.write().unwrap().insert(id, outline);
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
        let filtered: Vec<DocumentId> = list
            .iter()
            .filter(|d| meta_matches_filter(d, &req.filter))
            .map(|d| d.id.clone())
            .collect();
        let token = ScopeToken::new_for_adapter();
        let limit = self.scope_id_limit.unwrap_or_else(scope_id_limit_from_env);
        if let Some(spec) = predicate_spec_for(req, filtered.len(), limit, false) {
            return Ok(DocumentScope::new_predicate(
                &token,
                req.tenant.clone(),
                spec,
            ));
        }
        Ok(DocumentScope::new(&token, req.tenant.clone(), filtered))
    }

    async fn verify_visible(
        &self,
        req: &ScopeRequest,
        document_ids: &[DocumentId],
    ) -> Result<HashSet<DocumentId>, PortError> {
        if document_ids.is_empty() {
            return Ok(HashSet::new());
        }
        if self.fail_verify.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced verify failure".to_string()));
        }
        let map = self.docs.read().unwrap();
        let list = map.get(&req.tenant).cloned().unwrap_or_default();
        let wanted: HashSet<&DocumentId> = document_ids.iter().collect();
        Ok(list
            .into_iter()
            .filter(|d| wanted.contains(&d.id) && meta_matches_filter(d, &req.filter))
            .map(|d| d.id)
            .collect())
    }

    async fn find_by_id(
        &self,
        scope: &DocumentScope,
        id: &DocumentId,
    ) -> Result<Option<DocumentMeta>, PortError> {
        if !scope.allows_document(id) {
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

    async fn outlines(
        &self,
        scope: &DocumentScope,
    ) -> Result<Vec<(DocumentId, kb_core::tree::Outline)>, PortError> {
        if self.fail_outlines.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced outline failure".to_string()));
        }
        // The fake checks the scope exactly as the Postgres adapter does: a
        // document outside the caller's visible ids is never returned, even
        // when it has a matching outline.
        let outlines = self.outlines.read().unwrap();
        if self.outline_scope_hole.load(Ordering::Relaxed) {
            return Ok(outlines
                .iter()
                .map(|(id, outline)| (id.clone(), outline.clone()))
                .collect());
        }
        if let Some(verify) = scope.verify_spec() {
            // Predicate mode has no id list to enumerate: use the same document
            // rows and the same filter the verify uses, so a document moved out
            // of the KB in Postgres loses its outline even with a stale payload.
            let docs = self.docs.read().unwrap();
            let list = docs.get(&verify.tenant).cloned().unwrap_or_default();
            return Ok(list
                .into_iter()
                .filter(|doc| scope.allows_document(&doc.id))
                .filter(|doc| meta_matches_filter(doc, &verify.filter))
                .filter_map(|doc| {
                    outlines
                        .get(&doc.id)
                        .map(|outline| (doc.id.clone(), outline.clone()))
                })
                .collect());
        }
        Ok(scope
            .document_ids()
            .iter()
            .filter_map(|id| {
                outlines
                    .get(id)
                    .map(|outline| (id.clone(), outline.clone()))
            })
            .collect())
    }

    async fn find_duplicate_by_sha256(
        &self,
        tenant: &TenantId,
        sha256: &str,
        knowledge_base_ids: &[KnowledgeBaseId],
    ) -> Result<Option<DocumentId>, PortError> {
        let map = self.docs.read().unwrap();
        let list = map.get(tenant).cloned().unwrap_or_default();
        // The same scope the Postgres statement applies: the tenant (the map
        // is keyed by it), the hash, the knowledge bases, and neither a
        // deleted nor a failed document.
        let duplicate = list
            .into_iter()
            .filter(|d| d.sha256.as_deref() == Some(sha256))
            .filter(|d| d.status != "failed")
            .filter(|d| {
                knowledge_base_ids.is_empty()
                    || d.knowledge_base_ids
                        .iter()
                        .any(|id| knowledge_base_ids.iter().any(|k| k.0 == *id))
            })
            .max_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.0.cmp(&b.id.0))
            });
        Ok(duplicate.map(|d| d.id))
    }

    async fn list_page(
        &self,
        tenant: &TenantId,
        limit: u32,
        knowledge_base_ids: &[KnowledgeBaseId],
        external_ref: Option<&str>,
        cursor: Option<&kb_core::pagination::Cursor>,
    ) -> Result<DocumentPage, PortError> {
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
            .filter(|d| external_ref.is_none_or(|r| d.external_ref.as_deref() == Some(r)))
            .collect();
        // The same total order the Postgres adapter uses: newest first, the id
        // breaking the tie. Without the tie-break two rows created in the same
        // millisecond could swap places between pages and be listed twice.
        let mut ordered = filtered;
        ordered.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.0.cmp(&a.id.0))
        });
        let total = ordered.len() as u64;
        // The cursor continues the walk: everything strictly before its
        // position in this order is what the previous page already showed.
        let remaining: Vec<DocumentMeta> = match cursor {
            Some(c) => ordered
                .into_iter()
                .filter(|d| {
                    (d.created_at.as_str(), d.id.0.as_str())
                        < (c.created_at.as_str(), c.id.as_str())
                })
                .collect(),
            None => ordered,
        };
        let has_more = remaining.len() > limit as usize;
        let page: Vec<DocumentMeta> = remaining.into_iter().take(limit as usize).collect();
        let next_cursor = if has_more {
            page.last().map(|last| {
                kb_core::pagination::encode(&kb_core::pagination::Cursor::new(
                    last.created_at.clone(),
                    last.id.0.clone(),
                ))
            })
        } else {
            None
        };
        Ok(DocumentPage {
            documents: page,
            total,
            next_cursor,
        })
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
        let mut meta = DocumentMeta {
            id: doc.id.clone(),
            title: doc.title.clone(),
            categories: doc.categories.clone(),
            subcategory: doc.subcategory.clone(),
            status: doc.status.clone(),
            external_ref: doc.external_ref.clone(),
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
            metadata: doc.metadata.clone(),
            sha256: doc.sha256.clone(),
            doc_key: doc.doc_key.clone(),
            version: doc.version.clone(),
            supersedes_document_id: None,
            effective_from: doc.effective_from.clone(),
            effective_to: doc.effective_to.clone(),
            is_latest: true,
        };
        // Mirror `PgDocumentStore::create_document`: the new version becomes
        // latest and the previous one is retired in the same call. The HTTP
        // fake store uses the same rule so the in-process tests exercise the
        // semantics the SQL implements.
        if let Some(doc_key) = &doc.doc_key {
            let mut map = self.docs.write().unwrap();
            if let Some(list) = map.get_mut(&doc.tenant_id) {
                if let Some(previous) = list
                    .iter_mut()
                    .find(|d| d.doc_key.as_deref() == Some(doc_key.as_str()) && d.is_latest)
                {
                    meta.supersedes_document_id = Some(previous.id.0.clone());
                    if previous.effective_to.is_none() {
                        previous.effective_to = doc
                            .effective_from
                            .clone()
                            .or_else(|| Some(FAKE_TIMESTAMP.to_string()));
                    }
                    previous.is_latest = false;
                }
            }
        }
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
        if let Some(k) = &patch.doc_key {
            doc.doc_key = Some(k.clone());
        }
        if let Some(v) = &patch.version {
            doc.version = Some(v.clone());
        }
        if let Some(v) = &patch.effective_from {
            doc.effective_from = Some(v.clone());
        }
        if let Some(v) = &patch.effective_to {
            doc.effective_to = Some(v.clone());
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

/// The coarse payload a fake chunk carries. Deliberately independent of the
/// document store: a test can move a document between KBs, or give a chunk a
/// wrong tenant, without touching the index payload (SEC-2a stale payload).
#[derive(Clone, Debug, Default)]
pub struct FakeChunkPayload {
    pub tenant: Option<String>,
    pub knowledge_base_ids: Vec<String>,
    pub category: Option<String>,
}

/// Fake in-memory chunk store.
#[derive(Clone, Default)]
pub struct FakeChunkStore {
    pub vector_hits: Arc<RwLock<Vec<SearchHit>>>,
    pub bm25_hits: Arc<RwLock<Vec<SearchHit>>>,
    pub neighbour_hits: Arc<RwLock<Vec<SearchHit>>>,
    pub figure_hits: Arc<RwLock<Vec<SearchHit>>>,
    /// Payload per document, keyed exactly like Qdrant's keyword fields.
    /// A document with no entry has an unknown payload, so predicate mode lets
    /// the candidate through and leaves the decision to the Postgres verify.
    pub payloads: Arc<RwLock<HashMap<DocumentId, FakeChunkPayload>>>,
    /// Chunks written through `store_chunks`, keyed by document id. Also
    /// backs `chunk_counts`, so tests see the same numbers a real store would.
    pub stored_chunks: Arc<RwLock<HashMap<DocumentId, Vec<ChunkToStore>>>>,
    pub fail_store: Arc<AtomicBool>,
    pub fail_vector: Arc<AtomicBool>,
    pub fail_bm25: Arc<AtomicBool>,
    pub fail_neighbours: Arc<AtomicBool>,
    pub fail_figures: Arc<AtomicBool>,
    /// SEC-2b: a repair write fails while this is set, so a test can prove the
    /// outbox row is retried instead of completed.
    pub fail_payload: Arc<AtomicBool>,
    /// SEC-2b: documents whose points were removed, in call order.
    pub deleted_documents: Arc<RwLock<Vec<DocumentId>>>,
    /// SEC-2b: `(document, kb ids)` pairs handed to `set_document_kb_payload`.
    pub kb_payload_writes: Arc<RwLock<Vec<KbPayloadWrite>>>,
}

impl FakeChunkStore {
    /// Set the payload the index holds for a document (SEC-2a test hook).
    pub fn set_payload(&self, document: &DocumentId, payload: FakeChunkPayload) {
        self.payloads
            .write()
            .unwrap()
            .insert(document.clone(), payload);
    }

    /// Would the index's coarse predicate filter keep this document's chunks?
    /// Ids mode has no payload conditions: the id list is the whole filter.
    fn payload_allows(&self, scope: &DocumentScope, document: &DocumentId) -> bool {
        if !scope.is_predicate() {
            return true;
        }
        let Some(predicate) = scope.predicate() else {
            return true;
        };
        let map = self.payloads.read().unwrap();
        let Some(payload) = map.get(document) else {
            return true;
        };
        if payload
            .tenant
            .as_ref()
            .is_some_and(|tenant| tenant != scope.tenant().as_str())
        {
            return false;
        }
        if predicate.kb_ids.as_ref().is_some_and(|wanted| {
            !wanted
                .iter()
                .any(|kb| payload.knowledge_base_ids.iter().any(|id| id == &kb.0))
        }) {
            return false;
        }
        predicate
            .category
            .as_ref()
            .is_none_or(|cat| payload.category.as_ref() == Some(cat))
    }

    /// Both the scope's id rule and the index's coarse payload rule.
    fn scope_allows(&self, scope: &DocumentScope, document: &DocumentId) -> bool {
        if self.deleted_documents.read().unwrap().contains(document) {
            return false;
        }
        scope.allows_document(document) && self.payload_allows(scope, document)
    }
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
        // `ChunkToStore.metadata` is the stored metadata object verbatim, so
        // the read shape is derived from it the same way an adapter derives
        // it, and the id follows the store's own naming.
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
        let filtered: Vec<SearchHit> = list
            .iter()
            .filter(|h| self.scope_allows(scope, &h.chunk.document_id))
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
        let filtered: Vec<SearchHit> = list
            .iter()
            .filter(|h| self.scope_allows(scope, &h.chunk.document_id))
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
        let filtered: Vec<SearchHit> = list
            .iter()
            .filter(|h| self.scope_allows(scope, &h.chunk.document_id))
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
        let filtered: Vec<SearchHit> = list
            .iter()
            .filter(|h| self.scope_allows(scope, &h.chunk.document_id))
            .take(limit as usize)
            .cloned()
            .collect();
        Ok(filtered)
    }
}

/// Merge one candidate list from both arms into the fused answer `search` must
/// return: one entry per unit, both arm scores kept, RRF when asked for it.
///
/// A test fake that concatenates instead of fusing is not a smaller version of
/// the real thing, it is a different thing: the engine and every downstream
/// consumer assume `search` yields one candidate per unit.
pub fn fuse_candidates(candidates: Vec<Candidate>, fusion: Fusion, k: usize) -> Vec<Candidate> {
    let mut merged: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        match merged
            .iter()
            .position(|existing| existing.unit_id == candidate.unit_id)
        {
            Some(position) => {
                let previous = merged[position].clone();
                let entry = &mut merged[position];
                match fusion {
                    Fusion::Rrf { k: rrf_k } => {
                        entry.scores.fused +=
                            1.0 / (rrf_k.max(1) as f32 + previous.ranks.fused as f32 + 1.0);
                        entry.scores.fused +=
                            1.0 / (rrf_k.max(1) as f32 + candidate.ranks.fused as f32 + 1.0);
                    }
                    Fusion::None => {
                        if candidate.scores.dense.is_some() {
                            entry.scores.fused = candidate.scores.fused;
                        }
                    }
                }
                entry.scores.dense = candidate.scores.dense.or(entry.scores.dense);
                entry.scores.sparse = candidate.scores.sparse.or(entry.scores.sparse);
                entry.ranks.dense = candidate.ranks.dense.or(entry.ranks.dense);
                entry.ranks.sparse = candidate.ranks.sparse.or(entry.ranks.sparse);
            }
            None => merged.push(candidate),
        }
    }
    sort_candidates(&mut merged);
    merged.truncate(k);
    merged
}

#[async_trait]
impl RetrievalIndex for FakeChunkStore {
    async fn upsert(&self, units: Vec<IndexUnit>) -> Result<(), PortError> {
        if self.fail_store.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced store failure".to_string()));
        }
        let mut stored = self.stored_chunks.write().unwrap();
        for unit in units {
            let chunks = stored.entry(unit.document_id.clone()).or_default();
            chunks.push(ChunkToStore {
                content: unit.content,
                chunk_index: unit.chunk_index,
                embedding: unit.dense,
                metadata: unit.metadata,
                contextual_prefix: unit.contextual_prefix,
            });
        }
        Ok(())
    }

    async fn delete_document(
        &self,
        _tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<u64, PortError> {
        let removed = self
            .stored_chunks
            .write()
            .unwrap()
            .remove(document_id)
            .map(|v| v.len() as u64)
            .unwrap_or(0);
        self.deleted_documents
            .write()
            .unwrap()
            .push(document_id.clone());
        // A delete is idempotent in the reconciler: the second call (the route
        // already deleted the points) records the intent but returns 0.
        Ok(removed)
    }

    async fn set_document_kb_payload(
        &self,
        _tenant: &TenantId,
        document_id: &DocumentId,
        kb_ids: &[KnowledgeBaseId],
    ) -> Result<(), PortError> {
        if self.fail_payload.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced payload failure".to_string()));
        }
        let values: Vec<String> = kb_ids.iter().map(|kb| kb.0.clone()).collect();
        {
            let mut map = self.payloads.write().unwrap();
            let entry = map.entry(document_id.clone()).or_default();
            entry.knowledge_base_ids = values.clone();
        }
        self.kb_payload_writes
            .write()
            .unwrap()
            .push((document_id.clone(), values));
        Ok(())
    }

    async fn search(
        &self,
        query: &HybridQuery,
        scope: &DocumentScope,
    ) -> Result<Vec<Candidate>, PortError> {
        let v_failed = self.fail_vector.load(Ordering::Relaxed);
        let b_failed = self.fail_bm25.load(Ordering::Relaxed);

        if v_failed && b_failed {
            return Err(PortError::Unavailable(
                "both search arms failed".to_string(),
            ));
        }

        if query.dense.is_some() && v_failed {
            return Err(PortError::Unavailable("forced vector failure".to_string()));
        }

        if query.sparse.is_some() && b_failed {
            return Err(PortError::Unavailable("forced bm25 failure".to_string()));
        }

        let range = scope.unit_range();
        let in_range = |h: &SearchHit| {
            range.is_none_or(|r| {
                r.allows(
                    h.chunk.metadata.page,
                    h.chunk.metadata.chunk_index.unwrap_or(0),
                )
            })
        };
        // Fused by unit id, as both real adapters do: a unit both arms rank is
        // one candidate with two arm scores, not two candidates. The fake used
        // to concatenate, which hid the difference between "the keyword arm
        // found this" and "both arms found this" — the distinction the `retrieval`
        // provenance field exists to report.
        let mut out: Vec<Candidate> = Vec::new();

        if query.dense.is_some() && !v_failed {
            let v_list = self.vector_hits.read().unwrap();
            for h in v_list
                .iter()
                .filter(|h| self.scope_allows(scope, &h.chunk.document_id) && in_range(h))
            {
                out.push(Candidate {
                    unit_id: h.chunk.id.clone(),
                    document_id: h.chunk.document_id.clone(),
                    tenant: scope.tenant().clone(),
                    chunk_index: h.chunk.metadata.chunk_index.unwrap_or(0),
                    page: h.chunk.metadata.page,
                    chunk_type: h.chunk.chunk_type,
                    content: h.chunk.content.clone(),
                    contextual_prefix: h.chunk.metadata.contextual_prefix.clone(),
                    metadata: serde_json::json!({
                        "section": h.chunk.metadata.section,
                        "assetKey": h.chunk.metadata.asset_key,
                        "anchorChunkIndex": h.chunk.metadata.anchor_chunk_index,
                        "chunkIndex": h.chunk.metadata.chunk_index,
                        "page": h.chunk.metadata.page,
                        "tableId": h.chunk.metadata.table_id,
                    }),
                    scores: ArmScores {
                        dense: Some(h.similarity),
                        sparse: None,
                        fused: h.similarity,
                    },
                    ranks: ArmRanks {
                        dense: Some(h.rank),
                        sparse: None,
                        fused: h.rank,
                    },
                });
            }
        }

        if query.sparse.is_some() && !b_failed {
            let b_list = self.bm25_hits.read().unwrap();
            for h in b_list
                .iter()
                .filter(|h| self.scope_allows(scope, &h.chunk.document_id) && in_range(h))
            {
                out.push(Candidate {
                    unit_id: h.chunk.id.clone(),
                    document_id: h.chunk.document_id.clone(),
                    tenant: scope.tenant().clone(),
                    chunk_index: h.chunk.metadata.chunk_index.unwrap_or(0),
                    page: h.chunk.metadata.page,
                    chunk_type: h.chunk.chunk_type,
                    content: h.chunk.content.clone(),
                    contextual_prefix: h.chunk.metadata.contextual_prefix.clone(),
                    metadata: serde_json::json!({
                        "section": h.chunk.metadata.section,
                        "assetKey": h.chunk.metadata.asset_key,
                        "anchorChunkIndex": h.chunk.metadata.anchor_chunk_index,
                        "chunkIndex": h.chunk.metadata.chunk_index,
                        "page": h.chunk.metadata.page,
                        "tableId": h.chunk.metadata.table_id,
                    }),
                    scores: ArmScores {
                        dense: None,
                        sparse: Some(h.similarity),
                        fused: h.similarity,
                    },
                    ranks: ArmRanks {
                        dense: None,
                        sparse: Some(h.rank),
                        fused: h.rank,
                    },
                });
            }
        }

        Ok(fuse_candidates(out, query.fusion, query.k))
    }

    /// Serves ids out of [`Self::stored_chunks`] and **filters by scope**.
    ///
    /// The scope filter is the whole point. The graph arm ranks chunks by id and
    /// then asks for them by id, so this method is the only thing standing between
    /// a walk that over-reached and a document the caller cannot see. A fake that
    /// returned everything would make the arm's scope tests pass vacuously.
    async fn fetch(
        &self,
        scope: &DocumentScope,
        ids: &[ChunkId],
    ) -> Result<Vec<Candidate>, PortError> {
        if ids.is_empty() || scope.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: std::collections::HashSet<&str> = ids.iter().map(|id| id.0.as_str()).collect();
        let stored = self.stored_chunks.read().unwrap();
        let mut out: Vec<Candidate> = Vec::new();
        // Predicate mode has no materialised id list: walk the stored documents
        // and let the scope's id rule plus the index payload rule decide.
        for (document_id, chunks) in stored.iter() {
            if !self.scope_allows(scope, document_id) {
                continue;
            }
            for chunk in chunks {
                let unit_id = format!("{}_{}", document_id.0, chunk.chunk_index);
                if !wanted.contains(unit_id.as_str()) {
                    continue;
                }
                let meta = stored_chunk_metadata(&chunk.metadata);
                if !scope
                    .unit_range()
                    .is_none_or(|r| r.allows(meta.page, chunk.chunk_index))
                {
                    continue;
                }
                out.push(Candidate {
                    unit_id: ChunkId(unit_id),
                    document_id: document_id.clone(),
                    tenant: scope.tenant().clone(),
                    chunk_index: chunk.chunk_index,
                    page: None,
                    chunk_type: ChunkType::Text,
                    content: chunk.content.clone(),
                    contextual_prefix: chunk.contextual_prefix.clone(),
                    metadata: chunk.metadata.clone(),
                    scores: ArmScores::default(),
                    ranks: ArmRanks::default(),
                });
            }
        }
        Ok(out)
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        document_id: &DocumentId,
        _chunk_indices: &[u32],
    ) -> Result<Vec<Candidate>, PortError> {
        if self.fail_neighbours.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable(
                "forced neighbours failure".to_string(),
            ));
        }
        let list = self.neighbour_hits.read().unwrap();
        if !self.scope_allows(scope, document_id) {
            return Ok(Vec::new());
        }
        let range = scope.unit_range();
        let mut out = Vec::new();
        for h in list.iter() {
            if &h.chunk.document_id == document_id {
                let idx = h.chunk.metadata.chunk_index.unwrap_or(0);
                if !range.is_none_or(|r| r.allows(h.chunk.metadata.page, idx)) {
                    continue;
                }
                out.push(Candidate {
                    unit_id: h.chunk.id.clone(),
                    document_id: h.chunk.document_id.clone(),
                    tenant: scope.tenant().clone(),
                    chunk_index: idx,
                    page: h.chunk.metadata.page,
                    chunk_type: h.chunk.chunk_type,
                    content: h.chunk.content.clone(),
                    contextual_prefix: h.chunk.metadata.contextual_prefix.clone(),
                    metadata: serde_json::json!({
                        "section": h.chunk.metadata.section,
                        "assetKey": h.chunk.metadata.asset_key,
                        "anchorChunkIndex": h.chunk.metadata.anchor_chunk_index,
                        "chunkIndex": h.chunk.metadata.chunk_index,
                        "page": h.chunk.metadata.page,
                        "tableId": h.chunk.metadata.table_id,
                    }),
                    scores: ArmScores::default(),
                    ranks: ArmRanks::default(),
                });
            }
        }
        Ok(out)
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        document_ids: &[DocumentId],
    ) -> Result<Vec<Candidate>, PortError> {
        if self.fail_figures.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced figures failure".to_string()));
        }
        let list = self.figure_hits.read().unwrap();
        let range = scope.unit_range();
        let mut out = Vec::new();
        for h in list.iter() {
            if self.scope_allows(scope, &h.chunk.document_id)
                && document_ids.contains(&h.chunk.document_id)
                && range.is_none_or(|r| {
                    r.allows(
                        h.chunk.metadata.page,
                        h.chunk.metadata.chunk_index.unwrap_or(0),
                    )
                })
            {
                out.push(Candidate {
                    unit_id: h.chunk.id.clone(),
                    document_id: h.chunk.document_id.clone(),
                    tenant: scope.tenant().clone(),
                    chunk_index: h.chunk.metadata.chunk_index.unwrap_or(0),
                    page: h.chunk.metadata.page,
                    chunk_type: h.chunk.chunk_type,
                    content: h.chunk.content.clone(),
                    contextual_prefix: h.chunk.metadata.contextual_prefix.clone(),
                    metadata: serde_json::json!({
                        "section": h.chunk.metadata.section,
                        "assetKey": h.chunk.metadata.asset_key,
                        "anchorChunkIndex": h.chunk.metadata.anchor_chunk_index,
                        "chunkIndex": h.chunk.metadata.chunk_index,
                        "page": h.chunk.metadata.page,
                        "tableId": h.chunk.metadata.table_id,
                    }),
                    scores: ArmScores::default(),
                    ranks: ArmRanks::default(),
                });
            }
        }
        Ok(out)
    }

    async fn fetch_pages(
        &self,
        scope: &DocumentScope,
        pages: &[(DocumentId, u32)],
        per_page: usize,
    ) -> Result<Vec<Candidate>, PortError> {
        if pages.is_empty() || per_page == 0 || scope.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: std::collections::HashSet<(DocumentId, u32)> = pages
            .iter()
            .filter(|(document, _)| self.scope_allows(scope, document))
            .cloned()
            .collect();
        let stored = self.stored_chunks.read().unwrap();
        let mut out: Vec<Candidate> = Vec::new();
        let mut counts: std::collections::HashMap<(DocumentId, u32), usize> =
            std::collections::HashMap::new();
        for (document, chunks) in stored.iter() {
            if !self.scope_allows(scope, document) {
                continue;
            }
            for c in chunks {
                let meta = stored_chunk_metadata(&c.metadata);
                let Some(page) = meta.page else {
                    continue;
                };
                if !scope
                    .unit_range()
                    .is_none_or(|r| r.allows(meta.page, meta.chunk_index.unwrap_or(0)))
                {
                    continue;
                }
                let key = (document.clone(), page);
                if !wanted.contains(&key) {
                    continue;
                }
                let count = counts.entry(key).or_insert(0);
                if *count >= per_page {
                    continue;
                }
                *count += 1;
                out.push(Candidate {
                    unit_id: ChunkId(format!("{document}_{}", meta.chunk_index.unwrap_or(0))),
                    document_id: document.clone(),
                    tenant: scope.tenant().clone(),
                    chunk_index: meta.chunk_index.unwrap_or(0),
                    page: meta.page,
                    chunk_type: ChunkType::from_storage(
                        c.metadata
                            .get("chunkType")
                            .and_then(serde_json::Value::as_str),
                    ),
                    content: c.content.clone(),
                    contextual_prefix: c.contextual_prefix.clone(),
                    metadata: c.metadata.clone(),
                    scores: ArmScores::default(),
                    ranks: ArmRanks::default(),
                });
            }
        }
        out.sort_by(|a, b| {
            a.document_id
                .as_str()
                .cmp(b.document_id.as_str())
                .then_with(|| a.page.cmp(&b.page))
                .then_with(|| a.chunk_index.cmp(&b.chunk_index))
        });
        Ok(out)
    }

    async fn ordered_chunks(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<Vec<Candidate>, PortError> {
        let stored = self.stored_chunks.read().unwrap();
        let Some(chunks) = stored.get(document_id) else {
            return Ok(Vec::new());
        };
        let mut out: Vec<Candidate> = chunks
            .iter()
            .map(|c| {
                let meta = stored_chunk_metadata(&c.metadata);
                Candidate {
                    unit_id: ChunkId(format!("{document_id}_{}", meta.chunk_index.unwrap_or(0))),
                    document_id: document_id.clone(),
                    tenant: tenant.clone(),
                    chunk_index: meta.chunk_index.unwrap_or(0),
                    page: meta.page,
                    chunk_type: ChunkType::from_storage(
                        c.metadata
                            .get("chunkType")
                            .and_then(serde_json::Value::as_str),
                    ),
                    content: c.content.clone(),
                    contextual_prefix: c.contextual_prefix.clone(),
                    metadata: c.metadata.clone(),
                    scores: ArmScores::default(),
                    ranks: ArmRanks::default(),
                }
            })
            .collect();
        out.sort_by_key(|c| c.chunk_index);
        Ok(out)
    }

    async fn list_chunk_meta(
        &self,
        scope: &DocumentScope,
        document: &DocumentId,
    ) -> Result<Vec<ChunkMeta>, PortError> {
        if !self.scope_allows(scope, document) {
            return Ok(Vec::new());
        }
        let stored = self.stored_chunks.read().unwrap();
        let mut out: Vec<ChunkMeta> = stored
            .get(document)
            .map(|chunks| {
                chunks
                    .iter()
                    .map(|chunk| {
                        let span = |key: &str| {
                            chunk
                                .metadata
                                .get(key)
                                .and_then(serde_json::Value::as_u64)
                                .and_then(|value| u32::try_from(value).ok())
                        };
                        let meta = stored_chunk_metadata(&chunk.metadata);
                        ChunkMeta {
                            chunk_index: meta.chunk_index.unwrap_or(chunk.chunk_index),
                            page: meta.page,
                            section: meta.section,
                            content: chunk.content.clone(),
                            char_start: span("charStart"),
                            char_end: span("charEnd"),
                            overlap_prefix_len: span("overlapPrefixLen"),
                            chunk_type: ChunkType::from_storage(
                                chunk
                                    .metadata
                                    .get("chunkType")
                                    .and_then(serde_json::Value::as_str),
                            ),
                            asset_key: chunk
                                .metadata
                                .get("assetKey")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|meta| meta.chunk_index);
        Ok(out)
    }

    async fn health_check(&self) -> Result<(), PortError> {
        Ok(())
    }

    fn capabilities(&self) -> IndexCapabilities {
        IndexCapabilities {
            dense_dims: Some(1024),
            sparse: Some(kb_ports::KeywordSemantics::AnyTerm),
            server_side_fusion: true,
            multivector: false,
            approximate_dense: false,
        }
    }
}

/// One durable outbox row in the fake. `attempts` is incremented by the claim,
/// exactly as in Postgres, so a failed item can be claimed again.
#[derive(Clone, Debug)]
pub struct FakeOutboxRow {
    pub item: OutboxItem,
    pub last_error: Option<String>,
}

/// In-memory `ScopeOutbox` for reconciler tests.
#[derive(Clone, Default)]
pub struct FakeScopeOutbox {
    pub rows: Arc<RwLock<Vec<FakeOutboxRow>>>,
    /// Id source; a counter keeps the fakes free of a uuid dependency.
    pub next_id: Arc<std::sync::atomic::AtomicI64>,
}

impl FakeScopeOutbox {
    /// Queue one repair, as a document write would in Postgres.
    pub fn push(&self, tenant: TenantId, document: DocumentId, op: OutboxOp) {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.rows.write().unwrap().push(FakeOutboxRow {
            item: OutboxItem {
                id,
                tenant,
                document,
                op,
                attempts: 0,
            },
            last_error: None,
        });
    }

    /// The errors recorded by the last drain, in claim order.
    pub fn errors(&self) -> Vec<Option<String>> {
        self.rows
            .read()
            .unwrap()
            .iter()
            .map(|row| row.last_error.clone())
            .collect()
    }
}

#[async_trait]
impl ScopeOutbox for FakeScopeOutbox {
    async fn claim_batch(&self, limit: u32) -> Result<Vec<OutboxItem>, PortError> {
        let mut rows = self.rows.write().unwrap();
        let mut claimed = Vec::new();
        for row in rows.iter_mut() {
            if claimed.len() >= limit as usize || row.item.attempts >= 10 {
                continue;
            }
            row.item.attempts += 1;
            claimed.push(row.item.clone());
        }
        Ok(claimed)
    }

    async fn complete(&self, ids: &[i64]) -> Result<(), PortError> {
        self.rows
            .write()
            .unwrap()
            .retain(|row| !ids.contains(&row.item.id));
        Ok(())
    }

    async fn fail(&self, id: i64, error: &str) -> Result<(), PortError> {
        if let Some(row) = self
            .rows
            .write()
            .unwrap()
            .iter_mut()
            .find(|row| row.item.id == id)
        {
            row.last_error = Some(error.to_string());
        }
        Ok(())
    }

    async fn lag(&self) -> Result<OutboxLag, PortError> {
        let pending = self.rows.read().unwrap().len() as u64;
        Ok(OutboxLag {
            pending,
            oldest_pending_secs: None,
        })
    }
}

/// Fake in-memory embedder.
#[derive(Clone, Default)]
pub struct FakeEmbedder {
    pub fail: Arc<AtomicBool>,
    /// What `embed_query_detailed` reports as a cache hit. The default is a
    /// miss, which is what an embedder that does not cache should say.
    pub cached: Arc<AtomicBool>,
}

#[async_trait]
impl Embedder for FakeEmbedder {
    async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, PortError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable("forced embed failure".to_string()));
        }
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }

    async fn embed_query_detailed(&self, text: &str) -> Result<QueryEmbedding, PortError> {
        let vector = self.embed_query(text).await?;
        Ok(QueryEmbedding {
            vector,
            cached: self.cached.load(Ordering::Relaxed),
        })
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
    /// Return only the last candidate, as a reranker with a score threshold would.
    pub only_last: Arc<AtomicBool>,
    /// Every candidate handed to the reranker, in the order it received them.
    pub seen: Arc<RwLock<Vec<RerankCandidate>>>,
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
        self.seen
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(candidates.iter().cloned());
        let candidates = if self.only_last.load(Ordering::Relaxed) {
            candidates.into_iter().last().into_iter().collect()
        } else {
            candidates
        };
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

    /// The fake scores every candidate it is handed (by position), like a
    /// cross-encoder: asking it for more results is free.
    fn scores_every_candidate(&self) -> bool {
        true
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

/// In-memory table store (`TBL-1`) for the HTTP and engine tests.
///
/// It implements the same tenant rule as the real adapter — no record is ever
/// returned under another tenant — and records `(operation, tenant)` for every
/// call, so a route test can prove the handler passed the caller's tenant
/// rather than the table's.
#[derive(Clone, Default)]
pub struct FakeTableStore {
    pub records: Arc<RwLock<Vec<TableRecord>>>,
    pub calls: Arc<RwLock<Vec<(String, String)>>>,
}

impl FakeTableStore {
    fn note(&self, operation: &str, tenant: &TenantId) {
        self.calls
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((operation.to_string(), tenant.0.clone()));
    }

    /// Seed one stored table.
    pub fn seed(&self, record: TableRecord) {
        self.records
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(record);
    }

    /// The operations recorded, in order.
    pub fn calls(&self) -> Vec<(String, String)> {
        self.calls
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The current records of a tenant, latest version per document.
    fn current(&self, tenant: &TenantId, document: Option<&DocumentId>) -> Vec<TableRecord> {
        let records = self
            .records
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out: Vec<TableRecord> = records
            .iter()
            .filter(|r| &r.tenant_id == tenant)
            .filter(|r| document.is_none_or(|d| &r.document_id == d))
            .filter(|r| {
                records
                    .iter()
                    .filter(|o| o.tenant_id == r.tenant_id && o.document_id == r.document_id)
                    .map(|o| o.version)
                    .max()
                    .is_none_or(|latest| r.version == latest)
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| (&a.document_id.0, a.index).cmp(&(&b.document_id.0, b.index)));
        out
    }
}

#[async_trait]
impl TableStore for FakeTableStore {
    async fn put_tables(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
        tables: Vec<(kb_core::tables::TableGrid, kb_core::tables::TableManifest)>,
    ) -> Result<Vec<TableRecord>, PortError> {
        self.note("put", tenant);
        let version = self
            .current(tenant, Some(document))
            .iter()
            .map(|r| r.version)
            .max();
        let version = version.unwrap_or(0) + 1;
        let mut written = Vec::new();
        for (index, (grid, manifest)) in tables.into_iter().enumerate() {
            let index = index as u32;
            let record = TableRecord {
                id: TableId::of(document, index),
                tenant_id: tenant.clone(),
                document_id: document.clone(),
                index,
                version,
                source: grid.source.unwrap_or(kb_core::tables::TableSource::Csv),
                sheet: grid.sheet,
                page_span: grid.page_span,
                title: grid.title,
                caption: grid.caption,
                header: grid.header,
                manifest,
                grid_key: format!(
                    "tables/{}/{}_t{index}/v{version}.parquet",
                    tenant.0, document.0
                ),
                created_at: FAKE_TIMESTAMP.to_string(),
            };
            self.seed(record.clone());
            written.push(record);
        }
        Ok(written)
    }

    async fn list_tables(
        &self,
        tenant: &TenantId,
        document: Option<&DocumentId>,
    ) -> Result<Vec<TableRecord>, PortError> {
        self.note("list", tenant);
        Ok(self.current(tenant, document))
    }

    async fn get_table(
        &self,
        tenant: &TenantId,
        id: &TableId,
    ) -> Result<Option<TableRecord>, PortError> {
        self.note("get", tenant);
        Ok(self.current(tenant, None).into_iter().find(|r| &r.id == id))
    }

    async fn query(
        &self,
        tenant: &TenantId,
        id: &TableId,
        query: &kb_core::tables::TableQuery,
        policy: &kb_core::tables::QueryPolicy,
    ) -> Result<kb_core::tables::QueryResult, TableQueryFailure> {
        self.note("query", tenant);
        let Some(record) = self.current(tenant, None).into_iter().find(|r| &r.id == id) else {
            return Err(TableQueryFailure::Store(PortError::Invalid(format!(
                "table {id} does not exist in this tenant"
            ))));
        };
        if policy.aggregate_only
            && query
                .select
                .iter()
                .any(|item| matches!(item, kb_core::tables::SelectItem::Column { .. }))
        {
            return Err(TableQueryFailure::Invalid(
                kb_core::tables::QueryError::Restricted {
                    reason: "this table only answers aggregates".to_string(),
                },
            ));
        }
        Ok(kb_core::tables::QueryResult {
            columns: vec!["count".to_string()],
            rows: vec![vec![serde_json::json!(record.manifest.n_rows)]],
            rows_matched: u64::from(record.manifest.n_rows),
            excluded_derived_rows: record.manifest.derived_rows.len() as u32,
            suppressed_groups: 0,
            truncated: false,
            warnings: vec![format!("fake table {}", record.id)],
        })
    }

    async fn delete_document_tables(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
    ) -> Result<(), PortError> {
        self.note("delete", tenant);
        self.records
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|r| !(&r.tenant_id == tenant && &r.document_id == document));
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
    pub progress: Arc<RwLock<Vec<ProgressCall>>>,
    /// Detailed step updates with sub-counters.
    pub detailed_progress: Arc<RwLock<Vec<DetailedProgressCall>>>,
    pub finished: Arc<RwLock<Vec<FinishedJob>>>,
    pub reclaimed: Arc<RwLock<u64>>,
    /// Job ids passed to `touch`, in call order: the heartbeat's trail.
    pub touches: Arc<RwLock<Vec<String>>>,
    pub job_views: Arc<RwLock<HashMap<(TenantId, String), JobView>>>,
    pub reapables: Arc<RwLock<Vec<ReapableJob>>>,
    /// The jobs that are waiting for a backend (`#36`).
    pub parked: Arc<RwLock<Vec<ParkedJob>>>,
    /// Every job the store has seen, by id, so a parked job can be queued
    /// again when its wait is over.
    pub records: Arc<RwLock<HashMap<String, JobRecord>>>,
}

/// A job that waits for a backend to come back (`#36`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedJob {
    /// The job's id.
    pub job_id: String,
    /// Why the job is waiting.
    pub reason: String,
    /// When the job may be tried again, in milliseconds since the epoch.
    pub resume_at_ms: i64,
}

impl FakeJobStore {
    /// Queue a job for the next claim.
    pub fn push(&self, job: JobRecord) {
        self.records
            .write()
            .unwrap()
            .insert(job.id.clone(), job.clone());
        self.pending.write().unwrap().push_back(job);
    }

    pub fn pending_len(&self) -> usize {
        self.pending.read().unwrap().len()
    }

    /// The jobs that are waiting for a backend (`#36`).
    pub fn parked(&self) -> Vec<ParkedJob> {
        self.parked.read().unwrap().clone()
    }
}

#[async_trait]
impl JobStore for FakeJobStore {
    async fn claim_next_pending(&self) -> Result<Option<JobRecord>, PortError> {
        let job = self.pending.write().unwrap().pop_front();
        if let Some(job) = &job {
            self.records
                .write()
                .unwrap()
                .insert(job.id.clone(), job.clone());
        }
        Ok(job)
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

    async fn park(&self, job_id: &str, reason: &str, resume_at_ms: i64) -> Result<(), PortError> {
        self.parked.write().unwrap().push(ParkedJob {
            job_id: job_id.to_string(),
            reason: reason.to_string(),
            resume_at_ms,
        });
        // The store counts the attempt when it parks the job, exactly as the
        // Postgres store does in `PARK_JOB`: a job that keeps meeting an
        // unavailable backend runs out of tries instead of waiting forever.
        let mut records = self.records.write().unwrap();
        if let Some(record) = records.get_mut(job_id) {
            record.attempt = record.attempt.saturating_add(1);
        }
        Ok(())
    }

    async fn resume_parked(&self, now_ms: i64) -> Result<u32, PortError> {
        let mut parked = self.parked.write().unwrap();
        let mut resumed = Vec::new();
        let mut kept = Vec::new();
        for job in parked.drain(..) {
            if job.resume_at_ms <= now_ms {
                resumed.push(job);
            } else {
                kept.push(job);
            }
        }
        *parked = kept;
        let count = resumed.len() as u32;
        for job in resumed {
            if let Some(record) = self.records.read().unwrap().get(&job.job_id).cloned() {
                self.pending.write().unwrap().push_back(record);
            }
        }
        Ok(count)
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
            // The fake has no clock: a job created here reports no creation
            // time, and the seats serve such a job last.
            created_at: None,
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
    /// The document cannot be read: a parse failure, which fails the job.
    pub fail: Arc<AtomicBool>,
    /// The backend is not there: an unreachable OCR provider, which makes the
    /// job wait instead of failing (`#36`).
    pub unavailable: Arc<AtomicBool>,
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
            unavailable: Arc::new(AtomicBool::new(false)),
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
            unavailable: Arc::new(AtomicBool::new(false)),
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
        if self.unavailable.load(Ordering::Relaxed) {
            return Err(PortError::Unavailable(
                "the OCR provider is unreachable".to_string(),
            ));
        }
        if self.fail.load(Ordering::Relaxed) {
            return Err(PortError::Malformed("forced extract failure".to_string()));
        }
        if let Some(res) = self.result.read().unwrap().as_ref() {
            return Ok(res.clone());
        }
        Ok(ExtractionResult {
            ocr: None,
            content: self.content.read().unwrap().clone(),
            file_type: self.file_type.read().unwrap().clone(),
            ..ExtractionResult::default()
        })
    }
}

/// Fake document-intelligence reader.
///
/// The write half of the port is gone with SurrealDB: ingest writes entities
/// through `GraphStore`, and tests assert there. This fake only answers the
/// intelligence route.
#[derive(Clone, Default)]
pub struct FakeEntityStore {
    pub intelligences: Arc<RwLock<HashMap<DocumentId, DocumentIntelligenceData>>>,
}

#[async_trait]
impl EntityStore for FakeEntityStore {
    async fn get_document_intelligence(
        &self,
        _tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<DocumentIntelligenceData, PortError> {
        Ok(self
            .intelligences
            .read()
            .unwrap()
            .get(document_id)
            .cloned()
            .unwrap_or(DocumentIntelligenceData {
                entities: Vec::new(),
                relations: Vec::new(),
            }))
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
