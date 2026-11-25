//! SEC-2b integration: the outbox restores the recall predicate mode loses
//! when a document's KB membership changes in Postgres.
//!
//! The stale-payload half is the SEC-2a fixture: the verify hides the document,
//! but `scopeVerifyDropped` counts the lost candidates. After one reconciler
//! pass the same query drops nothing, the KB-1 key still cannot see the moved
//! document, and a KB-2 key can see it again.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use kb_engine::EngineSettings;
use kb_engine::fakes::{
    FakeBlobStore, FakeChunkPayload, FakeChunkStore, FakeDocumentStore, FakeEmbedder,
    FakeEntityStore, FakeJobStore, FakeReranker, FakeScopeOutbox, FakeTableStore,
};
use kb_engine::{CancelToken, OutboxReconciler};
use kb_ports::{
    Chunk, ChunkId, ChunkMetadata, ChunkType, DocumentId, DocumentMeta, DocumentStore,
    KnowledgeBaseId, OutboxOp, SearchHit, TenantId,
};
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};
use serde_json::Value;

const TENANT: &str = "tenant-a";
const KB_1: &str = "kb-1";
const KB_2: &str = "kb-2";
const DOC: &str = "doc-d";
const CANARY: &str = "outbox-reconciler-canary-42";

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

fn doc(id: &str, kbs: &[&str]) -> DocumentMeta {
    DocumentMeta {
        doc_key: None,
        version: None,
        supersedes_document_id: None,
        effective_from: None,
        effective_to: None,
        is_latest: true,
        sha256: None,
        id: DocumentId(id.to_string()),
        title: id.to_string(),
        categories: vec!["cat-1".to_string()],
        subcategory: None,
        status: "ready".to_string(),
        external_ref: Some(format!("ref-{id}")),
        chunk_count: 1,
        file_type: Some("text/markdown".to_string()),
        file_size: Some(1024),
        mime_type: Some("text/markdown".to_string()),
        s3_key: None,
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        knowledge_base_ids: kbs.iter().map(|k| (*k).to_string()).collect(),
        metadata: None,
    }
}

fn hit(id: &str, document: &str) -> SearchHit {
    SearchHit {
        chunk: Chunk {
            id: ChunkId(id.to_string()),
            document_id: DocumentId(document.to_string()),
            document_title: document.to_string(),
            external_ref: None,
            content: CANARY.to_string(),
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

struct Fixture {
    documents: Arc<FakeDocumentStore>,
    chunks: Arc<FakeChunkStore>,
    outbox: Arc<FakeScopeOutbox>,
}

fn fixture() -> Fixture {
    let documents = Arc::new(FakeDocumentStore {
        scope_id_limit: Some(1),
        ..FakeDocumentStore::default()
    });
    documents.insert_api_key_with_scopes(
        "kb1-key",
        TenantId(TENANT.to_string()),
        vec!["kb:read".to_string()],
        vec![KnowledgeBaseId(KB_1.to_string())],
    );
    documents.insert_api_key_with_scopes(
        "kb2-key",
        TenantId(TENANT.to_string()),
        vec!["kb:read".to_string()],
        vec![KnowledgeBaseId(KB_2.to_string())],
    );
    // E and F stay in both KBs so both keys remain in predicate mode (3 visible
    // documents with the limit set to 1) after D moves.
    documents.insert_doc(TenantId(TENANT.to_string()), doc(DOC, &[KB_1]));
    documents.insert_doc(TenantId(TENANT.to_string()), doc("doc-e", &[KB_1, KB_2]));
    documents.insert_doc(TenantId(TENANT.to_string()), doc("doc-f", &[KB_1, KB_2]));
    let chunks = Arc::new(FakeChunkStore::default());
    chunks
        .vector_hits
        .write()
        .unwrap()
        .push(hit("chunk-d", DOC));
    chunks
        .bm25_hits
        .write()
        .unwrap()
        .push(hit("chunk-d-bm25", DOC));
    chunks.set_payload(
        &DocumentId(DOC.to_string()),
        FakeChunkPayload {
            tenant: Some(TENANT.to_string()),
            knowledge_base_ids: vec![KB_1.to_string()],
            category: Some("cat-1".to_string()),
        },
    );
    let outbox = Arc::new(FakeScopeOutbox::default());
    Fixture {
        documents,
        chunks,
        outbox,
    }
}

async fn serve(f: &Fixture) -> String {
    let state = AppState::from_parts(
        StateParts {
            documents: f.documents.clone(),
            index: f.chunks.clone(),
            entities: Some(Arc::new(FakeEntityStore::default())),
            graph: None,
            graph_arm: None,
            blob: Arc::new(FakeBlobStore::default()),
            embedder: Arc::new(FakeEmbedder::default()),
            jobs: Arc::new(FakeJobStore::default()),
            events: EventBus::new(),
            tables: Some(Arc::new(FakeTableStore::default())),
            version: "outbox-test".to_string(),
            upload_dedupe: kb_core::policy::UploadDedupe::default(),
        },
        Arc::new(FakeReranker::default()),
        engine_settings(),
    )
    .with_outbox(f.outbox.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, kb_server::build_router(state)).await;
    });
    format!("http://{addr}")
}

async fn search(base: &str, key: &str) -> Value {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let response = client
        .post(format!("{base}/v1/search"))
        .header("authorization", format!("Bearer {key}"))
        .json(&serde_json::json!({"query": CANARY}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
    response.json::<Value>().await.unwrap()
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

fn dropped(body: &Value) -> u64 {
    body["stats"]["scopeVerifyDropped"].as_u64().unwrap_or(0)
}

fn move_d_to_kb2(f: &Fixture) {
    let mut docs = f.documents.docs.write().unwrap();
    let list = docs.get_mut(&TenantId(TENANT.to_string())).unwrap();
    list[0].knowledge_base_ids = vec![KB_2.to_string()];
}

#[tokio::test]
async fn a_repaired_kb_move_restores_recall_without_leaking_the_old_kb() {
    let f = fixture();
    move_d_to_kb2(&f);
    // The write path wrote this row in the same transaction as the move; the
    // payload is deliberately left stale, as it is between a commit and a drain.
    f.outbox.push(
        TenantId(TENANT.to_string()),
        DocumentId(DOC.to_string()),
        OutboxOp::KbChanged,
    );
    let url = serve(&f).await;

    let stale = search(&url, "kb1-key").await;
    assert!(!doc_ids(&stale).contains(&DOC.to_string()), "{stale}");
    assert!(
        dropped(&stale) >= 1,
        "the stale payload must be caught: {stale}"
    );
    let before = search(&url, "kb2-key").await;
    assert!(
        !doc_ids(&before).contains(&DOC.to_string()),
        "recall lost: {before}"
    );

    let reconciler = OutboxReconciler::new(f.outbox.clone(), f.documents.clone(), f.chunks.clone())
        .with_batch(10)
        .with_interval(Duration::from_millis(1))
        .with_max_attempts(10);
    let report = reconciler.reconcile_once().await.unwrap();
    assert_eq!(report.repaired, 1);
    assert!(f.outbox.rows.read().unwrap().is_empty());
    assert_eq!(
        f.chunks.payloads.read().unwrap()[&DocumentId(DOC.to_string())].knowledge_base_ids,
        vec![KB_2.to_string()]
    );

    let after = search(&url, "kb1-key").await;
    assert!(!doc_ids(&after).contains(&DOC.to_string()), "leak: {after}");
    assert_eq!(
        dropped(&after),
        0,
        "no candidate should be stale now: {after}"
    );
    let restored = search(&url, "kb2-key").await;
    assert!(
        doc_ids(&restored).contains(&DOC.to_string()),
        "recall: {restored}"
    );
    assert_eq!(dropped(&restored), 0, "{restored}");
}

#[tokio::test]
async fn a_deleted_documents_points_are_removed_by_the_reconciler() {
    let f = fixture();
    f.documents
        .soft_delete(&TenantId(TENANT.to_string()), &DocumentId(DOC.to_string()))
        .await
        .unwrap();
    f.outbox.push(
        TenantId(TENANT.to_string()),
        DocumentId(DOC.to_string()),
        OutboxOp::Deleted,
    );
    let url = serve(&f).await;

    let stale = search(&url, "kb1-key").await;
    assert!(!doc_ids(&stale).contains(&DOC.to_string()), "{stale}");

    let report = OutboxReconciler::new(f.outbox.clone(), f.documents.clone(), f.chunks.clone())
        .reconcile_once()
        .await
        .unwrap();
    assert_eq!(report.deleted, 1);
    assert_eq!(
        f.chunks.deleted_documents.read().unwrap().as_slice(),
        &[DocumentId(DOC.to_string())]
    );
    let after = search(&url, "kb1-key").await;
    assert!(!doc_ids(&after).contains(&DOC.to_string()), "{after}");
    // The document is gone from Postgres, so no search can propose it again.
    assert_eq!(dropped(&after), 0, "{after}");
    assert!(f.outbox.rows.read().unwrap().is_empty());
}

#[tokio::test]
async fn a_hand_built_state_has_no_outbox_until_one_is_attached() {
    let f = fixture();
    let state = AppState::from_parts(
        StateParts {
            documents: f.documents.clone(),
            index: f.chunks.clone(),
            entities: None,
            graph: None,
            graph_arm: None,
            blob: Arc::new(FakeBlobStore::default()),
            embedder: Arc::new(FakeEmbedder::default()),
            jobs: Arc::new(FakeJobStore::default()),
            events: EventBus::new(),
            tables: None,
            version: "outbox-test".to_string(),
            upload_dedupe: kb_core::policy::UploadDedupe::default(),
        },
        Arc::new(FakeReranker::default()),
        engine_settings(),
    );
    assert!(state.outbox.is_none());
    let state = state.with_outbox(f.outbox.clone());
    assert!(state.outbox.is_some());
    // The cancel token is deliberately left unused: this asserts wiring only.
    let _cancel = CancelToken::new();
}
