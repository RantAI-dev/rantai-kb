#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// Shared helpers only: this file keeps its own harness because it also builds an
// ingest worker over fakes.
#[allow(dead_code)]
mod harness;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kb_engine::EngineSettings;
use kb_engine::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeEntityStore, FakeJobStore,
    FakeReranker, make_fake_hit,
};
use kb_ports::{ChunkToStore, DocumentId, DocumentMeta, TenantId};
use kb_server::dto::SearchResponseDto;
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};
use tower::ServiceExt;

use harness::{assert_error, json_body};

/// The whole port set behind a test app, so a test can assert on the fakes
/// behind the router.
pub struct Harness {
    pub app: axum::Router,
    pub documents: FakeDocumentStore,
    pub chunks: FakeChunkStore,
    pub blobs: FakeBlobStore,
    pub jobs: FakeJobStore,
    pub entities: FakeEntityStore,
    pub events: Arc<EventBus>,
}

impl Harness {
    pub fn new() -> Self {
        let documents = FakeDocumentStore::default();
        let chunks = FakeChunkStore::default();
        let blobs = FakeBlobStore::default();
        let jobs = FakeJobStore::default();
        let entities = FakeEntityStore::default();
        let embedder = FakeEmbedder::default();
        let events = EventBus::new();

        let settings = EngineSettings {
            candidate_pool_size: 20,
            rerank_final_k: 10,
            max_chunks: 10,
            min_similarity: 0.30,
            neighbour_window: 1,
            figure_limit: 3,
            expansion_enabled: false,
            expansion_paraphrases: 3,
            hybrid_bm25_enabled: true,
        };

        let state = AppState::from_parts(
            StateParts {
                documents: Arc::new(documents.clone()),
                chunks: Arc::new(chunks.clone()),
                entities: Arc::new(entities.clone()),
                blob: Arc::new(blobs.clone()),
                embedder: Arc::new(embedder),
                jobs: Arc::new(jobs.clone()),
                events: Arc::clone(&events),
                version: "test".to_string(),
            },
            Arc::new(FakeReranker::default()),
            settings,
        );

        Self {
            app: kb_server::build_router(state),
            documents,
            chunks,
            blobs,
            jobs,
            entities,
            events,
        }
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// A tenant with a read+write key, which is what most route tests need.
pub fn tenant_with_key(name: &str) -> (TenantId, String) {
    (TenantId(name.to_string()), format!("{name}-key"))
}

fn authed(method: &str, uri: &str, key: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .method(method)
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn test_missing_key_returns_401() {
    let harness = Harness::new();

    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"query": "test query"}"#))
        .unwrap();

    let response = harness.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_key_without_kb_read_returns_403() {
    let harness = Harness::new();
    harness.documents.insert_api_key_with_scopes(
        "write-only-key",
        TenantId("tenant-1".into()),
        vec!["kb:write".into()],
        vec![],
    );

    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer write-only-key")
        .body(Body::from(r#"{"query": "test query"}"#))
        .unwrap();

    let response = harness.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_search_happy_path_fields() {
    let harness = Harness::new();
    let tenant = TenantId("tenant-1".into());
    harness
        .documents
        .insert_api_key("valid-key", tenant.clone());

    let doc_id = "doc-1";
    harness.documents.insert_doc(
        tenant,
        DocumentMeta {
            id: DocumentId(doc_id.into()),
            title: "Happy Path Document".into(),
            categories: vec!["guide".into()],
            subcategory: None,
            status: "ready".into(),
            chunk_count: 1,
            file_type: None,
            file_size: None,
            mime_type: None,
            s3_key: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
            knowledge_base_ids: vec![],
            metadata: None,
        },
    );

    let hit = make_fake_hit(
        "c_1",
        doc_id,
        "Happy Path Document",
        "Happy path content line",
        1,
        0.9,
    );
    harness.chunks.bm25_hits.write().unwrap().push(hit);

    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer valid-key")
        .body(Body::from(r#"{"query": "happy path", "maxChunks": 5}"#))
        .unwrap();

    let response = harness.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: SearchResponseDto = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body.chunks.len(), 1);
    assert_eq!(body.chunks[0].id, "c_1");
    assert_eq!(body.chunks[0].document_title, "Happy Path Document");
    assert_eq!(body.sources.len(), 1);
    assert_eq!(body.sources[0].document_title, "Happy Path Document");
    assert!(body.context.contains("[1] Happy Path Document"));
    assert!(body.context.contains("Happy path content line"));
}

#[tokio::test]
async fn test_key_is_accepted_from_bearer_in_any_case_or_from_x_api_key() {
    let harness = Harness::new();
    harness
        .documents
        .insert_api_key("valid-key", TenantId("tenant-1".into()));

    for header in ["Bearer valid-key", "bearer valid-key", "BEARER  valid-key "] {
        let request = Request::builder()
            .uri("/v1/documents")
            .method("GET")
            .header("authorization", header)
            .body(Body::empty())
            .unwrap();
        let response = harness.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "header {header}");
    }

    let request = Request::builder()
        .uri("/v1/documents")
        .method("GET")
        .header("x-api-key", "valid-key")
        .body(Body::empty())
        .unwrap();
    let response = harness.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_search_with_an_empty_knowledge_base_list_answers_with_the_note() {
    let harness = Harness::new();
    let tenant = TenantId("tenant-1".into());
    harness.documents.insert_api_key("valid-key", tenant);

    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer valid-key")
        .body(Body::from(
            r#"{"query": "anything", "knowledgeBaseIds": []}"#,
        ))
        .unwrap();

    let response = harness.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["chunks"], serde_json::json!([]));
    assert_eq!(body["context"], "");
    assert_eq!(
        body["note"],
        "API key is not bound to any of the requested knowledge bases"
    );
}

#[tokio::test]
async fn test_search_without_a_query_is_a_400_with_the_bun_text() {
    let harness = Harness::new();
    harness
        .documents
        .insert_api_key("valid-key", TenantId("tenant-1".into()));

    for body in [r#"{}"#, r#"{"query": 5}"#, r#"not json"#] {
        let request = Request::builder()
            .uri("/v1/search")
            .method("POST")
            .header("content-type", "application/json")
            .header("authorization", "Bearer valid-key")
            .body(Body::from(body))
            .unwrap();
        let response = harness.app.clone().oneshot(request).await.unwrap();
        assert_error(
            response,
            StatusCode::BAD_REQUEST,
            "Body must include a 'query' string",
        )
        .await;
    }
}

#[tokio::test]
async fn test_tenant_b_key_searching_fakes_with_tenant_a_docs_gets_zero_results() {
    let harness = Harness::new();

    // Tenant A owns doc-a
    let tenant_a = TenantId("tenant-a".into());
    let doc_a_id = "doc-a";
    harness.documents.insert_doc(
        tenant_a.clone(),
        DocumentMeta {
            id: DocumentId(doc_a_id.into()),
            title: "Tenant A Confidential Doc".into(),
            categories: vec![],
            subcategory: None,
            status: "ready".into(),
            chunk_count: 1,
            file_type: None,
            file_size: None,
            mime_type: None,
            s3_key: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
            knowledge_base_ids: vec![],
            metadata: None,
        },
    );

    let hit = make_fake_hit(
        "c_a",
        doc_a_id,
        "Tenant A Confidential Doc",
        "Confidential text for A only",
        0,
        0.95,
    );
    harness.chunks.bm25_hits.write().unwrap().push(hit.clone());
    harness.chunks.vector_hits.write().unwrap().push(hit);

    // Tenant B authenticates with their own key
    let tenant_b = TenantId("tenant-b".into());
    harness.documents.insert_api_key("tenant-b-key", tenant_b);

    // Test without hybrid
    let request1 = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer tenant-b-key")
        .body(Body::from(r#"{"query": "confidential"}"#))
        .unwrap();

    let response1 = harness.app.clone().oneshot(request1).await.unwrap();
    assert_eq!(response1.status(), StatusCode::OK);
    let bytes1 = axum::body::to_bytes(response1.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body1: SearchResponseDto = serde_json::from_slice(&bytes1).unwrap();

    assert_eq!(body1.chunks.len(), 0);
    assert_eq!(body1.sources.len(), 0);
    assert_eq!(body1.context, "");

    // Test with hybrid: true
    let request2 = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer tenant-b-key")
        .body(Body::from(r#"{"query": "confidential", "hybrid": true}"#))
        .unwrap();

    let response2 = harness.app.oneshot(request2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::OK);
    let bytes2 = axum::body::to_bytes(response2.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body2: SearchResponseDto = serde_json::from_slice(&bytes2).unwrap();

    assert_eq!(body2.chunks.len(), 0);
    assert_eq!(body2.sources.len(), 0);
    assert_eq!(body2.context, "");
}

fn delete_test_doc(id: &str) -> DocumentMeta {
    DocumentMeta {
        id: DocumentId(id.into()),
        title: "Delete Me".into(),
        categories: vec![],
        subcategory: None,
        status: "READY".into(),
        chunk_count: 0,
        file_type: None,
        file_size: None,
        mime_type: None,
        s3_key: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
        knowledge_base_ids: vec![],
        metadata: None,
    }
}

async fn send_delete(app: axum::Router, uri: &str, key: &str) -> StatusCode {
    app.oneshot(authed("DELETE", uri, key))
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn test_delete_without_kb_write_returns_403() {
    let harness = Harness::new();
    harness.documents.insert_api_key_with_scopes(
        "read-only-key",
        TenantId("tenant-1".into()),
        vec!["kb:read".into()],
        vec![],
    );
    harness
        .documents
        .insert_doc(TenantId("tenant-1".into()), delete_test_doc("doc-del-1"));

    assert_eq!(
        send_delete(
            harness.app.clone(),
            "/v1/documents/doc-del-1",
            "read-only-key"
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        harness.documents.docs.read().unwrap()[&TenantId("tenant-1".into())].len(),
        1
    );
}

#[tokio::test]
async fn test_soft_delete_hides_document_from_reads() {
    let harness = Harness::new();
    harness.documents.insert_api_key_with_scopes(
        "writer-key",
        TenantId("tenant-1".into()),
        vec!["kb:read".into(), "kb:write".into()],
        vec![],
    );
    harness
        .documents
        .insert_doc(TenantId("tenant-1".into()), delete_test_doc("doc-del-2"));

    assert_eq!(
        send_delete(harness.app.clone(), "/v1/documents/doc-del-2", "writer-key").await,
        StatusCode::OK
    );
    assert_eq!(
        harness
            .app
            .clone()
            .oneshot(authed("GET", "/v1/documents/doc-del-2", "writer-key"))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(harness.documents.deleted.read().unwrap().len(), 1);
}

#[tokio::test]
async fn test_delete_other_tenant_document_is_not_found() {
    let harness = Harness::new();
    harness.documents.insert_api_key_with_scopes(
        "tenant-2-key",
        TenantId("tenant-2".into()),
        vec!["kb:read".into(), "kb:write".into()],
        vec![],
    );
    harness
        .documents
        .insert_doc(TenantId("tenant-1".into()), delete_test_doc("doc-del-3"));

    assert_eq!(
        send_delete(
            harness.app.clone(),
            "/v1/documents/doc-del-3",
            "tenant-2-key"
        )
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        harness.documents.docs.read().unwrap()[&TenantId("tenant-1".into())].len(),
        1
    );
}

#[tokio::test]
async fn test_hard_delete_removes_document() {
    let harness = Harness::new();
    harness.documents.insert_api_key_with_scopes(
        "hard-key",
        TenantId("tenant-1".into()),
        vec!["kb:read".into(), "kb:write".into()],
        vec![],
    );
    harness
        .documents
        .insert_doc(TenantId("tenant-1".into()), delete_test_doc("doc-del-4"));

    assert_eq!(
        send_delete(
            harness.app.clone(),
            "/v1/documents/doc-del-4?hard=true",
            "hard-key"
        )
        .await,
        StatusCode::OK
    );
    assert!(harness.documents.docs.read().unwrap()[&TenantId("tenant-1".into())].is_empty());
}

#[tokio::test]
async fn test_hard_delete_cross_tenant_returns_404_and_leaves_chunks_unchanged() {
    let harness = Harness::new();

    // Tenant A owns document doc-tenant-a
    let tenant_a = TenantId("tenant-a".into());
    let doc_id = DocumentId("doc-tenant-a".into());
    let mut doc = delete_test_doc("doc-tenant-a");
    doc.s3_key = Some("documents/tenant-a/doc-tenant-a/sample.pdf".into());
    harness.documents.insert_doc(tenant_a.clone(), doc);

    // Tenant A document has 2 chunks stored
    let fake_chunks = vec![
        ChunkToStore {
            content: "chunk 1 content".into(),
            chunk_index: 0,
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            metadata: serde_json::json!({"chunkIndex": 0, "documentTitle": "A"}),
            contextual_prefix: None,
        },
        ChunkToStore {
            content: "chunk 2 content".into(),
            chunk_index: 1,
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            metadata: serde_json::json!({"chunkIndex": 1, "documentTitle": "A"}),
            contextual_prefix: None,
        },
    ];
    harness
        .chunks
        .stored_chunks
        .write()
        .unwrap()
        .insert(doc_id.clone(), fake_chunks);

    // Verify initial chunk count for doc_id is 2
    let counts_before = harness
        .chunks
        .stored_chunks
        .read()
        .unwrap()
        .get(&doc_id)
        .map(|c| c.len())
        .unwrap_or(0);
    assert_eq!(counts_before, 2);

    // Tenant B authenticates with a valid kb:write key
    let tenant_b = TenantId("tenant-b".into());
    harness.documents.insert_api_key_with_scopes(
        "tenant-b-write-key",
        tenant_b,
        vec!["kb:read".into(), "kb:write".into()],
        vec![],
    );

    // Tenant B attempts hard delete on tenant A's document
    let status = send_delete(
        harness.app.clone(),
        "/v1/documents/doc-tenant-a?hard=true",
        "tenant-b-write-key",
    )
    .await;

    // Assert: must return 404 NOT FOUND
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Assert: Tenant A's document row still exists
    assert_eq!(harness.documents.docs.read().unwrap()[&tenant_a].len(), 1);

    // Assert through fake ChunkStore: Tenant A's chunks are untouched (still 2)
    let counts_after = harness
        .chunks
        .stored_chunks
        .read()
        .unwrap()
        .get(&doc_id)
        .map(|c| c.len())
        .unwrap_or(0);
    assert_eq!(counts_after, 2);
}

#[tokio::test]
async fn test_ingest_worker_with_real_local_extractor() {
    use kb_engine::fakes::FakeJobStore;
    use kb_engine::{IngestSettings, IngestWorker};
    use kb_ports::{BlobStore, JobRecord};

    let doc_store = FakeDocumentStore::default();
    let chunk_store = FakeChunkStore::default();
    let blob_store = FakeBlobStore::default();
    let embedder = FakeEmbedder::default();
    let jobs = FakeJobStore::default();

    let tenant = TenantId("tenant-1".into());
    let doc_id = DocumentId("doc-extract-1".into());
    let mut doc = delete_test_doc("doc-extract-1");
    doc.s3_key = Some("uploads/doc-extract-1/notes.txt".into());
    doc_store.insert_doc(tenant.clone(), doc);

    blob_store
        .upload(
            "uploads/doc-extract-1/notes.txt",
            b"Hello world from real extractor test".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();

    let job = JobRecord {
        id: "job-1".into(),
        tenant_id: tenant,
        user_id: None,
        document_id: Some(doc_id.clone()),
        s3_key: Some("uploads/doc-extract-1/notes.txt".into()),
        filename: "notes.txt".into(),
        mime_type: Some("text/plain".into()),
        attempt: 1,
        params: None,
    };
    jobs.push(job);

    let extractor = kb_extract::LocalExtractor::shared();
    let worker = IngestWorker::new(
        Arc::new(jobs.clone()),
        Arc::new(doc_store.clone()),
        Arc::new(chunk_store.clone()),
        Arc::new(blob_store),
        extractor,
        Arc::new(embedder),
        IngestSettings::default(),
    );

    assert!(worker.run_once().await.unwrap());

    let finished = jobs.finished.read().unwrap();
    assert_eq!(finished.len(), 1);
    assert!(finished[0].success);

    // Chunks were created and stored
    let stored = chunk_store.stored_chunks.read().unwrap();
    let chunks = stored.get(&doc_id).expect("stored chunks for doc");
    assert!(!chunks.is_empty());
    assert!(
        chunks[0]
            .content
            .contains("Hello world from real extractor test")
    );
}
