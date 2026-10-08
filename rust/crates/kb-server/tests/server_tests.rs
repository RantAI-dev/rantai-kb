#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kb_engine::EngineSettings;
use kb_engine::fakes::{
    FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_hit,
};
use kb_ports::{DocumentId, DocumentMeta, TenantId};
use kb_server::dto::SearchResponseDto;
use kb_server::state::AppState;
use tower::ServiceExt;

fn setup_test_app() -> (axum::Router, FakeDocumentStore, FakeChunkStore) {
    let doc_store = FakeDocumentStore::default();
    let chunk_store = FakeChunkStore::default();
    let embedder = FakeEmbedder::default();
    let reranker = FakeReranker::default();

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

    let state = AppState::from_arcs(
        Arc::new(doc_store.clone()),
        Arc::new(chunk_store.clone()),
        Arc::new(embedder),
        Arc::new(reranker),
        settings,
    );

    let app = kb_server::build_router(state);
    (app, doc_store, chunk_store)
}

#[tokio::test]
async fn test_missing_key_returns_401() {
    let (app, _, _) = setup_test_app();

    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"query": "test query"}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_key_without_kb_read_returns_403() {
    let (app, doc_store, _) = setup_test_app();
    doc_store.insert_api_key_with_scopes(
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

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_search_happy_path_fields() {
    let (app, doc_store, chunk_store) = setup_test_app();
    let tenant = TenantId("tenant-1".into());
    doc_store.insert_api_key("valid-key", tenant.clone());

    let doc_id = "doc-1";
    doc_store.insert_doc(
        tenant,
        DocumentMeta {
            id: DocumentId(doc_id.into()),
            title: "Happy Path Document".into(),
            categories: vec!["guide".into()],
            subcategory: None,
            status: "ready".into(),
            chunk_count: 1,
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
    chunk_store.bm25_hits.write().unwrap().push(hit);

    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer valid-key")
        .body(Body::from(r#"{"query": "happy path", "maxChunks": 5}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
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
async fn test_tenant_b_key_searching_fakes_with_tenant_a_docs_gets_zero_results() {
    let (app, doc_store, chunk_store) = setup_test_app();

    // Tenant A owns doc-a
    let tenant_a = TenantId("tenant-a".into());
    let doc_a_id = "doc-a";
    doc_store.insert_doc(
        tenant_a.clone(),
        DocumentMeta {
            id: DocumentId(doc_a_id.into()),
            title: "Tenant A Confidential Doc".into(),
            categories: vec![],
            subcategory: None,
            status: "ready".into(),
            chunk_count: 1,
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
    chunk_store.bm25_hits.write().unwrap().push(hit.clone());
    chunk_store.vector_hits.write().unwrap().push(hit);

    // Tenant B authenticates with their own key
    let tenant_b = TenantId("tenant-b".into());
    doc_store.insert_api_key("tenant-b-key", tenant_b);

    // Test without hybrid
    let request1 = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer tenant-b-key")
        .body(Body::from(r#"{"query": "confidential"}"#))
        .unwrap();

    let response1 = app.clone().oneshot(request1).await.unwrap();
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

    let response2 = app.oneshot(request2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::OK);
    let bytes2 = axum::body::to_bytes(response2.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body2: SearchResponseDto = serde_json::from_slice(&bytes2).unwrap();

    assert_eq!(body2.chunks.len(), 0);
    assert_eq!(body2.sources.len(), 0);
    assert_eq!(body2.context, "");
}
