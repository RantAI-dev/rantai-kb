#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// Shared helpers only: this file keeps its own harness because it also builds an
// ingest worker over fakes.
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
            rerank_candidate_limit: 10,
            rerank_char_budget: 350,
            rerank_timeout_ms: 3000,
            rerank_final_k: 10,
            max_chunks: 10,
            min_similarity: 0.30,
            neighbour_window: 1,
            figure_limit: 3,
            expansion_enabled: false,
            expansion_paraphrases: 3,
            hybrid_bm25_enabled: true,
            rerank_enabled: false,
        };

        let state = AppState::from_parts(
            StateParts {
                documents: Arc::new(documents.clone()),
                index: Arc::new(chunks.clone()),
                entities: Some(Arc::new(entities.clone())),
                graph: None,
                graph_arm: None,
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
            external_ref: None,
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

    let chunks = body.chunks.expect("chunks present");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].id, "document_chunk:⟨c_1⟩");
    assert_eq!(
        kb_server::dto::chunk_id_from_client(&chunks[0].id),
        "c_1",
        "the id goes out in Bun's form and comes back to the bare ident the store keys on"
    );
    assert_eq!(chunks[0].document_title, "Happy Path Document");
    let sources = body.sources.expect("sources present");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].document_title, "Happy Path Document");
    let context = body.context.expect("context present");
    assert!(context.contains("[1] Happy Path Document"));
    assert!(context.contains("Happy path content line"));
}

#[tokio::test]
async fn test_search_chunks_carry_the_documents_external_ref() {
    let harness = Harness::new();
    let tenant = TenantId("tenant-1".into());
    harness
        .documents
        .insert_api_key("valid-key", tenant.clone());
    harness.documents.insert_doc(
        tenant,
        DocumentMeta {
            id: DocumentId("doc-1".into()),
            title: "KU Document".into(),
            categories: vec![],
            subcategory: None,
            status: "ready".into(),
            external_ref: Some("ku-42".into()),
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
    harness
        .chunks
        .bm25_hits
        .write()
        .unwrap()
        .push(make_fake_hit(
            "c_1",
            "doc-1",
            "KU Document",
            "The knowledge unit body",
            0,
            0.0,
        ));

    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer valid-key")
        .body(Body::from(r#"{"query": "knowledge unit"}"#))
        .unwrap();
    let response = harness.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let chunks = body["chunks"].as_array().expect("chunks");
    assert_eq!(chunks.len(), 1, "{body}");
    assert_eq!(
        chunks[0]["externalRef"], "ku-42",
        "a chunk names the KU its document came from: {body}"
    );
    // Additive only: every Bun key is still there.
    assert_eq!(chunks[0]["documentId"], "doc-1");
    assert_eq!(chunks[0]["documentTitle"], "KU Document");
}

/// A harness with one tenant, one document and two arms' worth of hits, ready
/// for a timing assertion.
fn timing_harness() -> Harness {
    let harness = Harness::new();
    let tenant = TenantId("tenant-1".into());
    harness
        .documents
        .insert_api_key("valid-key", tenant.clone());
    harness.documents.insert_doc(
        tenant,
        DocumentMeta {
            id: DocumentId("doc-1".into()),
            title: "Timing Document".into(),
            categories: vec![],
            subcategory: None,
            status: "READY".into(),
            external_ref: None,
            chunk_count: 1,
            file_type: None,
            file_size: None,
            mime_type: None,
            s3_key: None,
            created_at: "".into(),
            updated_at: "".into(),
            knowledge_base_ids: vec![],
            metadata: None,
        },
    );
    harness
        .chunks
        .vector_hits
        .write()
        .unwrap()
        .push(make_fake_hit(
            "c_1",
            "doc-1",
            "Timing Document",
            "Vector content",
            1,
            0.9,
        ));
    harness
        .chunks
        .bm25_hits
        .write()
        .unwrap()
        .push(make_fake_hit(
            "c_2",
            "doc-1",
            "Timing Document",
            "Keyword content",
            2,
            0.8,
        ));
    harness
}

async fn search_body(harness: &Harness, query: &str) -> serde_json::Value {
    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer valid-key")
        .body(Body::from(
            serde_json::json!({"query": query, "maxChunks": 5}).to_string(),
        ))
        .unwrap();
    let response = harness.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // The response is spliced into bytes by hand rather than serialised by the
    // framework, so its content type is worth an assertion of its own: a
    // client that sees text/plain will not parse it.
    assert_eq!(
        content_type.as_deref(),
        Some("application/json; charset=utf-8"),
        "search answers with JSON"
    );
    value
}

/// Every stage the pipeline runs is named in the response, and what they add up
/// to is bounded by the total. Without this, a timing instrument is a set of
/// numbers nobody can trust, and a regression that double-counts two stages
/// would look like a speed-up.
#[tokio::test]
async fn search_reports_stage_timings_that_add_up_to_the_total() {
    let harness = timing_harness();
    let body = search_body(&harness, "timing query").await;

    let stats = body.get("stats").expect("stats present");
    let stages = stats.get("stageTimingsMs").expect("stageTimingsMs present");
    for key in [
        "scopeBuild",
        "expand",
        "arms",
        "enrich",
        "fusion",
        "rerank",
        "neighbours",
        "figures",
        "context",
        "serialise",
        "total",
        "detail",
    ] {
        assert!(stages.get(key).is_some(), "stage {key} is missing");
    }

    let total = stages["total"].as_f64().expect("total is a number");
    assert!(total > 0.0, "a search that ran reports time, not zero");

    let accounted = kb_server::dto::accounted_pairs(&stats_from(&body));
    let sum: f64 = accounted.iter().map(|(_, ms)| *ms).sum();
    assert!(
        sum <= total + 1.0,
        "stages sum to {sum} ms but the request took {total} ms; \
         a stage is being counted twice or the total is not the whole request"
    );

    // The detail is overlapping, so it is reported but never summed — and on a
    // request that actually embedded and searched, it is not all zeros.
    let detail = &stages["detail"];
    assert!(detail["engineTotal"].as_f64().unwrap() > 0.0);
    assert!(detail["queryEmbed"].as_f64().unwrap() > 0.0);
    assert!(detail["vectorArm"].as_f64().unwrap() > 0.0);
    assert!(detail["keywordArm"].as_f64().unwrap() > 0.0);
    assert!(
        detail["queryEmbedCached"].is_boolean(),
        "cache hit or miss is reported, not guessed"
    );
}

/// The legacy Bun fields keep their names, so a client reading them keeps
/// working — and on the standard path they now carry the real numbers.
#[tokio::test]
async fn search_stats_keep_the_bun_fields_and_carry_real_numbers() {
    let harness = timing_harness();
    let body = search_body(&harness, "another timing query").await;
    let stats = body["stats"].clone();

    for key in [
        "totalTimeMs",
        "embeddingTimeMs",
        "vectorSearchTimeMs",
        "entitySearchTimeMs",
        "graphSearchTimeMs",
        "fusionTimeMs",
        "vectorResults",
        "entityResults",
        "graphResults",
        "totalResults",
        "stageTimingsMs",
    ] {
        assert!(stats.get(key).is_some(), "{key} disappeared");
    }
    assert!(
        stats["embeddingTimeMs"].as_f64().unwrap() > 0.0,
        "the query was embedded; zero would be the old lie"
    );
    assert_eq!(stats["totalResults"].as_u64(), Some(2));
    assert_eq!(
        stats["totalTimeMs"].as_f64().unwrap(),
        stats["stageTimingsMs"]["total"].as_f64().unwrap(),
        "the legacy total and the stage total are the same number"
    );
}

/// The hybrid branch runs nothing, so it reports zeros — but in the same shape
/// as every other branch, so a client does not have to know which one it hit.
#[tokio::test]
async fn the_hybrid_stub_reports_the_same_stats_shape_with_zeros() {
    let harness = timing_harness();
    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer valid-key")
        .body(Body::from(r#"{"query": "hybrid", "hybrid": true}"#))
        .unwrap();
    let response = harness.app.clone().oneshot(request).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert!(body["stats"]["stageTimingsMs"]["arms"].as_f64().unwrap() >= 0.0);
    assert_eq!(body["stats"]["totalResults"], 0);
    assert!(body["stats"]["stageTimingsMs"]["total"].as_f64().unwrap() > 0.0);
}

/// The `documentIds` branch runs its own, shorter pipeline; it reports its own
/// stages rather than borrowing the engine's.
#[tokio::test]
async fn the_document_ids_branch_reports_its_own_stages() {
    let harness = timing_harness();
    let request = Request::builder()
        .uri("/v1/search")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer valid-key")
        .body(Body::from(
            r#"{"query": "narrow", "documentIds": ["doc-1"]}"#,
        ))
        .unwrap();
    let response = harness.app.clone().oneshot(request).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let stages = &body["stats"]["stageTimingsMs"];
    assert!(stages["scopeBuild"].as_f64().unwrap() > 0.0);
    assert!(stages["arms"].as_f64().unwrap() > 0.0);
    assert!(stages["detail"]["vectorArm"].as_f64().unwrap() > 0.0);
    assert!(stages["total"].as_f64().unwrap() > 0.0);
    assert!(!body["chunks"].as_array().unwrap().is_empty());
}

/// Parse the typed stats back out of a response, so the test asserts on the
/// same numbers a client would see rather than on its own arithmetic.
fn stats_from(body: &serde_json::Value) -> kb_server::dto::StageTimingsDto {
    serde_json::from_value(body["stats"]["stageTimingsMs"].clone()).unwrap()
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
            external_ref: None,
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

    assert_eq!(body1.chunks.as_deref().unwrap_or(&[]).len(), 0);
    assert_eq!(body1.sources.as_deref().unwrap_or(&[]).len(), 0);
    assert_eq!(body1.context.as_deref().unwrap_or(""), "");

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

    assert_eq!(body2.chunks.as_deref().unwrap_or(&[]).len(), 0);
    assert_eq!(body2.sources.as_deref().unwrap_or(&[]).len(), 0);
    assert_eq!(body2.context.as_deref().unwrap_or(""), "");
}

fn delete_test_doc(id: &str) -> DocumentMeta {
    DocumentMeta {
        id: DocumentId(id.into()),
        title: "Delete Me".into(),
        categories: vec![],
        subcategory: None,
        status: "READY".into(),
        external_ref: None,
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
