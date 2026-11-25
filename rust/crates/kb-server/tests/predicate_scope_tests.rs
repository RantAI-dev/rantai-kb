//! SEC-2a integration tests: a predicate scope must behave like an id-list
//! scope for honest payloads, and must fail closed for every kind of drift.
//!
//! `KB_SCOPE_ID_LIMIT=1` is represented by `FakeDocumentStore.scope_id_limit`:
//! an edition-2024 integration test cannot mutate the process environment.
//! `None` reads `KB_SCOPE_ID_LIMIT` exactly as the Postgres adapter does.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use kb_engine::EngineSettings;
use kb_engine::fakes::{
    FakeBlobStore, FakeChunkPayload, FakeChunkStore, FakeDocumentStore, FakeEmbedder,
    FakeEntityStore, FakeJobStore, FakeReranker, FakeTableStore,
};
use kb_ports::{
    Chunk, ChunkId, ChunkMetadata, ChunkType, DocumentId, DocumentMeta, DocumentStore,
    KnowledgeBaseId, SearchHit, TenantId,
};
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};
use serde_json::Value;

const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";
const KB_1: &str = "kb-1";
const KB_2: &str = "kb-2";
const DOC: &str = "doc-d";
const CANARY: &str = "predicate-scope-canary-42";

fn engine_settings() -> EngineSettings {
    EngineSettings {
        candidate_pool_size: 20,
        max_chunks: 10,
        neighbour_window: 1,
        figure_limit: 1,
        hybrid_bm25_enabled: true,
        ..EngineSettings::default()
    }
}

fn doc(_tenant: &str, id: &str, kb: &str) -> DocumentMeta {
    DocumentMeta {
        doc_key: None,
        version: None,
        supersedes_document_id: None,
        effective_from: None,
        effective_to: None,
        is_latest: true,
        acl_principals: Vec::new(),
        sha256: None,
        id: DocumentId(id.to_string()),
        title: id.to_string(),
        categories: vec!["cat-1".to_string()],
        subcategory: None,
        status: "ready".to_string(),
        external_ref: Some(format!("ref-{id}")),
        chunk_count: 2,
        file_type: Some("text/markdown".to_string()),
        file_size: Some(1024),
        mime_type: Some("text/markdown".to_string()),
        s3_key: None,
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        knowledge_base_ids: vec![kb.to_string()],
        metadata: None,
    }
}

fn hit(id: &str, document: &str, content: &str) -> SearchHit {
    SearchHit {
        chunk: Chunk {
            id: ChunkId(id.to_string()),
            document_id: DocumentId(document.to_string()),
            document_title: document.to_string(),
            external_ref: None,
            content: content.to_string(),
            chunk_type: ChunkType::Text,
            metadata: ChunkMetadata {
                page: Some(0),
                chunk_index: Some(0),
                ..ChunkMetadata::default()
            },
            categories: Vec::new(),
            subcategory: None,
        },
        similarity: 0.9,
        rank: 0,
        is_bm25: false,
        retrieval: None,
        dense_score: Some(0.9),
        rerank_score: None,
    }
}

/// The honest fixture: one document in KB-1, one chunk carrying the canary,
/// tenant and KB payloads in agreement with Postgres.
struct Fixture {
    documents: FakeDocumentStore,
    chunks: FakeChunkStore,
}

fn fixture(scope_id_limit: Option<usize>) -> Fixture {
    let documents = FakeDocumentStore {
        scope_id_limit,
        ..FakeDocumentStore::default()
    };
    documents.insert_api_key("predicate-key", TenantId(TENANT_A.to_string()));
    // Three visible documents so the scope stays in predicate mode with the
    // limit set to 1 even after one is moved out of a KB or soft-deleted.
    documents.insert_doc(TenantId(TENANT_A.to_string()), doc(TENANT_A, DOC, KB_1));
    documents.insert_doc(TenantId(TENANT_A.to_string()), doc(TENANT_A, "doc-e", KB_1));
    documents.insert_doc(TenantId(TENANT_A.to_string()), doc(TENANT_A, "doc-f", KB_1));
    let chunks = FakeChunkStore::default();
    chunks
        .vector_hits
        .write()
        .unwrap()
        .push(hit("chunk-d", DOC, CANARY));
    chunks
        .bm25_hits
        .write()
        .unwrap()
        .push(hit("chunk-d-bm25", DOC, CANARY));
    chunks.set_payload(
        &DocumentId(DOC.to_string()),
        FakeChunkPayload {
            tenant: Some(TENANT_A.to_string()),
            knowledge_base_ids: vec![KB_1.to_string()],
            category: Some("cat-1".to_string()),
        },
    );
    Fixture { documents, chunks }
}

async fn serve(f: Fixture) -> String {
    let Fixture { documents, chunks } = f;
    let state = AppState::from_parts(
        StateParts {
            documents: Arc::new(documents),
            index: Arc::new(chunks),
            entities: Some(Arc::new(FakeEntityStore::default())),
            graph: None,
            graph_arm: None,
            blob: Arc::new(FakeBlobStore::default()),
            embedder: Arc::new(FakeEmbedder::default()),
            jobs: Arc::new(FakeJobStore::default()),
            events: EventBus::new(),
            tables: Some(Arc::new(FakeTableStore::default())),
            version: "predicate-test".to_string(),
            upload_dedupe: kb_core::policy::UploadDedupe::default(),
        },
        Arc::new(FakeReranker::default()),
        engine_settings(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, kb_server::build_router(state)).await;
    });
    format!("http://{addr}")
}

async fn search(base: &str, key: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
    let response = client
        .post(format!("{base}/v1/search"))
        .header("authorization", format!("Bearer {key}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json::<Value>().await.unwrap())
}

fn query() -> Value {
    serde_json::json!({"query": CANARY})
}

fn doc_ids(body: &Value) -> Vec<String> {
    body.get("chunks")
        .and_then(Value::as_array)
        .map(|chunks| {
            chunks
                .iter()
                .map(|chunk| chunk["documentId"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn dropped(body: &Value) -> Option<u64> {
    body["stats"]["scopeVerifyDropped"].as_u64()
}

#[tokio::test]
async fn predicate_and_ids_mode_return_the_same_hits_on_an_honest_fixture() {
    let predicate_url = serve(fixture(Some(1))).await;
    let ids_url = serve(fixture(Some(0))).await;
    let (status_p, predicate) = search(&predicate_url, "predicate-key", query()).await;
    let (status_i, ids) = search(&ids_url, "predicate-key", query()).await;
    assert_eq!(status_p, reqwest::StatusCode::OK);
    assert_eq!(status_i, reqwest::StatusCode::OK);
    assert_eq!(doc_ids(&predicate), vec![DOC, DOC]);
    assert_eq!(doc_ids(&predicate), doc_ids(&ids));
    // The stat is additive: absent in Ids mode, present (zero drops) in predicate.
    assert_eq!(dropped(&predicate), Some(0));
    assert_eq!(dropped(&ids), None);
}

#[tokio::test]
async fn a_stale_kb_payload_cannot_show_a_document_moved_out_of_that_kb() {
    let f = fixture(Some(1));
    // A key limited to KB-1; the index payload still says KB-1.
    f.documents.insert_api_key_with_scopes(
        "kb1-key",
        TenantId(TENANT_A.to_string()),
        vec!["kb:read".to_string()],
        vec![KnowledgeBaseId(KB_1.to_string())],
    );
    // Move the document in Postgres only. No outbox in this task, so the chunk
    // payload is deliberately stale (SEC-2a's whole reason for existing).
    {
        let mut docs = f.documents.docs.write().unwrap();
        let list = docs.get_mut(&TenantId(TENANT_A.to_string())).unwrap();
        list[0].knowledge_base_ids = vec![KB_2.to_string()];
    }
    let url = serve(f).await;
    let (status, body) = search(&url, "kb1-key", query()).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(!doc_ids(&body).contains(&DOC.to_string()), "{body}");
    assert!(dropped(&body).unwrap_or(0) >= 1, "{body}");
}

#[tokio::test]
async fn a_stale_tenant_payload_cannot_show_another_tenants_document() {
    let f = fixture(Some(1));
    // The document belongs to B; the chunk payload claims A. Even if the index
    // coarse filter admits it, the Postgres verify is tenant-scoped.
    {
        let mut docs = f.documents.docs.write().unwrap();
        let list = docs.get_mut(&TenantId(TENANT_A.to_string())).unwrap();
        let moved = list.remove(0);
        docs.entry(TenantId(TENANT_B.to_string()))
            .or_default()
            .push(moved);
    }
    let url = serve(f).await;
    let (status, body) = search(&url, "predicate-key", query()).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(!doc_ids(&body).contains(&DOC.to_string()), "{body}");
    assert!(dropped(&body).unwrap_or(0) >= 1, "{body}");
}

#[tokio::test]
async fn a_deleted_documents_chunks_are_never_returned() {
    let f = fixture(Some(1));
    f.documents
        .soft_delete(
            &TenantId(TENANT_A.to_string()),
            &DocumentId(DOC.to_string()),
        )
        .await
        .unwrap();
    let url = serve(f).await;
    let (status, body) = search(&url, "predicate-key", query()).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(!doc_ids(&body).contains(&DOC.to_string()), "{body}");
    assert!(dropped(&body).unwrap_or(0) >= 1, "{body}");
}

#[tokio::test]
async fn a_failed_verify_fails_the_search_instead_of_returning_unverified_hits() {
    let f = fixture(Some(1));
    f.documents
        .fail_verify
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let url = serve(f).await;
    let (status, body) = search(&url, "predicate-key", query()).await;
    assert!(
        status.is_server_error(),
        "expected 5xx, got {status}: {body}"
    );
    assert_ne!(doc_ids(&body), vec![DOC]);
}
