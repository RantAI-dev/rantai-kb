//! End-to-end check of the eval harness against a real HTTP server backed by
//! the in-process fakes.
//!
//! The runner is a black-box client, so the only way to test it honestly is to
//! serve the real router on a loopback port and point the runner at it. The
//! corpus is not uploaded (`--skip-ingest`): a pre-written `ingest-map.json`
//! maps the dataset's document keys onto documents seeded in the fake store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use kb_engine::EngineSettings;
use kb_engine::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeEntityStore, FakeJobStore,
    FakeReranker,
};
use kb_ports::{
    Chunk, ChunkId, ChunkMetadata, ChunkType, DocumentId, DocumentMeta, SearchHit, TenantId,
};
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};

fn engine_settings() -> EngineSettings {
    EngineSettings {
        candidate_pool_size: 20,
        rerank_final_k: 10,
        max_chunks: 10,
        min_similarity: 0.30,
        neighbour_window: 1,
        figure_limit: 3,
        expansion_enabled: false,
        expansion_paraphrases: 3,
        hybrid_bm25_enabled: false,
        rerank_enabled: false,
    }
}

fn doc_meta(id: &str, title: &str) -> DocumentMeta {
    DocumentMeta {
        id: DocumentId(id.to_string()),
        title: title.to_string(),
        categories: vec![],
        subcategory: None,
        status: "ready".to_string(),
        chunk_count: 1,
        file_type: Some("text/plain".to_string()),
        file_size: Some(10),
        mime_type: Some("text/plain".to_string()),
        s3_key: None,
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        knowledge_base_ids: vec![],
        metadata: None,
    }
}

fn hit(id: &str, doc_id: &str, title: &str, content: &str, page: Option<u32>, similarity: f32) -> SearchHit {
    SearchHit {
        chunk: Chunk {
            id: ChunkId(id.to_string()),
            document_id: DocumentId(doc_id.to_string()),
            document_title: title.to_string(),
            content: content.to_string(),
            chunk_type: ChunkType::Text,
            metadata: ChunkMetadata {
                page,
                ..Default::default()
            },
            categories: vec![],
            subcategory: None,
        },
        similarity,
        rank: 0,
        is_bm25: false,
    }
}

async fn write_fixture_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kb-eval-e2e-{}", std::process::id()));
    let corpus = dir.join("corpus");
    let qrels = dir.join("qrels");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&qrels).unwrap();
    std::fs::write(corpus.join("alpha.txt"), "alpha is about rivers\n").unwrap();
    std::fs::write(corpus.join("beta.txt"), "beta is about mountains\n").unwrap();
    std::fs::write(
        dir.join("queries.jsonl"),
        "{\"_id\":\"q1\",\"text\":\"tell me about alpha\"}\n{\"_id\":\"q2\",\"text\":\"tell me about beta\"}\n",
    )
    .unwrap();
    // q1 is graded on doc alpha and its first page; q2 on doc beta.
    std::fs::write(
        qrels.join("test.tsv"),
        "query-id\tcorpus-id\tscore\nq1\talpha\t2\nq1\talpha#p1\t2\nq2\tbeta\t2\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ingest-map.json"),
        "{\"alpha\":\"doc-alpha\",\"beta\":\"doc-beta\"}\n",
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn runner_measures_a_live_server_end_to_end() {
    let documents = FakeDocumentStore::default();
    let chunks = FakeChunkStore::default();
    let blobs = FakeBlobStore::default();
    let jobs = FakeJobStore::default();
    let entities = FakeEntityStore::default();
    let embedder = FakeEmbedder::default();
    let events = EventBus::new();

    let tenant = TenantId("eval-tenant".to_string());
    documents.insert_api_key("eval-key", tenant.clone());
    documents.insert_doc(tenant.clone(), doc_meta("doc-alpha", "Alpha"));
    documents.insert_doc(tenant, doc_meta("doc-beta", "Beta"));
    chunks
        .vector_hits
        .write()
        .unwrap()
        .push(hit("c-alpha", "doc-alpha", "Alpha", "river content", Some(0), 0.9));
    chunks
        .vector_hits
        .write()
        .unwrap()
        .push(hit("c-beta", "doc-beta", "Beta", "mountain content", None, 0.8));

    let state = AppState::from_parts(
        StateParts {
            documents: Arc::new(documents),
            chunks: Arc::new(chunks),
            entities: Arc::new(entities),
            blob: Arc::new(blobs),
            embedder: Arc::new(embedder),
            jobs: Arc::new(jobs),
            events: Arc::clone(&events),
            version: "test".to_string(),
        },
        Arc::new(FakeReranker::default()),
        engine_settings(),
    );
    let router = kb_server::build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let dataset_dir = write_fixture_dir().await;
    let out_dir = dataset_dir.join("out");
    let mut config =
        kb_eval::RunnerConfig::new(format!("http://{addr}"), "eval-key", &dataset_dir);
    config.out_dir = out_dir.clone();
    config.skip_ingest = true;
    config.k = 5;
    config.label = "e2e".to_string();

    let report = kb_eval::runner::run(config).await.expect("run harness");

    assert_eq!(report.metrics.n_queries, 2);
    assert_eq!(report.metrics.n_failed, 0);
    // The fake vector search returns alpha first for both queries; q1 finds its
    // gold at rank 1, q2 at rank 2.
    assert!(report.metrics.document.ndcg.mean > 0.5, "{:?}", report.metrics.document);
    // The page-level arm only has q1, which is a perfect hit.
    assert_eq!(report.metrics.page.n_queries, 1);
    assert!(report.metrics.page.ndcg.mean > 0.99);

    let metrics = std::fs::read_to_string(out_dir.join("metrics.json")).unwrap();
    assert!(metrics.contains("\"dataset\": \"kb-eval-e2e-"));
    assert!(out_dir.join("report.md").is_file());
    let run = std::fs::read_to_string(out_dir.join("run.trec")).unwrap();
    assert!(run.contains("q1 Q0 alpha 1 1.000000 e2e"), "{run}");
    assert!(out_dir.join("run.page.trec").is_file());

    server.abort();
    let _ = std::fs::remove_dir_all(&dataset_dir);
}
