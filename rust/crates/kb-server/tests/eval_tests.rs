//! End-to-end check of the eval harness against a real HTTP server backed by
//! the in-process fakes.
//!
//! The runner is a black-box client, so the only honest way to test it is to
//! serve the real router on a loopback port and point the runner at it: the
//! fakes sit behind real HTTP, real auth and real JSON. Nothing about the
//! retrieval code is stubbed out from the harness's side.
//!
//! The corpus is not uploaded (`--skip-ingest`): a pre-written `ingest-map.json`
//! in the output directory maps the dataset's document keys onto documents
//! seeded in the fake store. The ingest path itself is exercised by
//! `upload_tests.rs` against the same fakes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use std::sync::Arc;

use kb_engine::EngineSettings;
use kb_engine::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeEntityStore, FakeJobStore,
    FakeReranker,
};
use kb_ports::{Chunk, ChunkId, ChunkMetadata, ChunkType, DocumentId, SearchHit, TenantId};
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};

use harness::doc_meta;

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

fn hit(
    id: &str,
    doc_id: &str,
    title: &str,
    content: &str,
    page: Option<u32>,
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
                page,
                ..Default::default()
            },
            categories: vec![],
            subcategory: None,
        },
        similarity,
        rank: 0,
        is_bm25: false,
        retrieval: None,
    }
}

/// A dataset directory plus the output directory the harness is told to use.
/// `q3` has a blank text, which the server answers with a 400: that is the
/// failed query the harness must count rather than score.
fn write_dataset(dir: &std::path::Path) {
    let corpus = dir.join("corpus");
    let qrels = dir.join("qrels");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&qrels).unwrap();
    std::fs::write(corpus.join("alpha.txt"), "alpha is about rivers\n").unwrap();
    std::fs::write(corpus.join("beta.txt"), "beta is about mountains\n").unwrap();
    std::fs::write(
        dir.join("queries.jsonl"),
        concat!(
            "{\"_id\":\"q1\",\"text\":\"tell me about alpha\"}\n",
            "{\"_id\":\"q2\",\"text\":\"tell me about beta\"}\n",
            "{\"_id\":\"q3\",\"text\":\"   \"}\n"
        ),
    )
    .unwrap();
    // q1 is graded on doc alpha and its first page; q2 on doc beta. q3 is
    // graded too: the server answers it with a 400, so it must be scored as a
    // zero in the strict means rather than quietly dropped.
    std::fs::write(
        qrels.join("test.tsv"),
        "query-id\tcorpus-id\tscore\nq1\talpha\t2\nq1\talpha#p1\t2\nq2\tbeta\t2\nq3\talpha\t2\n",
    )
    .unwrap();
}

/// Serve the router with two seeded documents and return its address.
async fn serve() -> String {
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
    chunks.vector_hits.write().unwrap().push(hit(
        "c-alpha",
        "doc-alpha",
        "Alpha",
        "river content",
        Some(0),
        0.9,
    ));
    chunks.vector_hits.write().unwrap().push(hit(
        "c-beta",
        "doc-beta",
        "Beta",
        "mountain content",
        None,
        0.8,
    ));

    let state = AppState::from_parts(
        StateParts {
            documents: Arc::new(documents),
            index: Arc::new(chunks),
            entities: Arc::new(entities),
            graph: None,
            graph_arm: None,
            blob: Arc::new(blobs),
            embedder: Arc::new(embedder),
            jobs: Arc::new(jobs),
            events: Arc::clone(&events),
            version: "test".to_string(),
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

#[tokio::test]
async fn runner_measures_a_live_server_end_to_end() {
    let base_url = serve().await;
    let dataset_dir = std::env::temp_dir().join(format!("kb-eval-e2e-{}", std::process::id()));
    write_dataset(&dataset_dir);
    let out_dir = dataset_dir.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    // --skip-ingest reads this from the *output* directory, which is where a
    // previous ingest run would have written it.
    std::fs::write(
        out_dir.join("ingest-map.json"),
        "{\"alpha\":\"doc-alpha\",\"beta\":\"doc-beta\"}\n",
    )
    .unwrap();

    let mut config = kb_eval::RunnerConfig::new(&base_url, "eval-key", &dataset_dir);
    config.out_dir = out_dir.clone();
    config.skip_ingest = true;
    config.k = 5;
    config.label = "e2e".to_string();

    let report = kb_eval::runner::run(config).await.expect("run harness");

    // Three queries were asked. One of them (q3) is answered 400 by the server.
    assert_eq!(report.metrics.n_queries, 3);
    assert_eq!(report.metrics.n_failed, 1);
    assert_eq!(report.metrics.n_documents, 2);
    assert_eq!(report.metrics.n_ingest_failed, 0);
    // Strict means: q1, q2 and the failed q3 are all evaluable at the document
    // level, so all three are in — the failure as a zero.
    assert_eq!(report.metrics.document.n_queries, 3);
    assert_eq!(report.metrics.document.answered.n_queries, 2);
    assert!(
        report.metrics.document.ndcg.mean < report.metrics.document.answered.ndcg.mean,
        "a failed query must lower the strict mean, not raise it: {:?} vs {:?}",
        report.metrics.document.ndcg,
        report.metrics.document.answered.ndcg
    );
    // The fake vector search returns alpha first for both queries; q1 finds its
    // gold at rank 1, q2 at rank 2.
    assert!(
        report.metrics.document.ndcg.mean > 0.5,
        "{:?}",
        report.metrics.document
    );
    // The page-level arm only has q1, which is a perfect hit on its first page.
    // q3 has no page judgement, so its failure is not a page zero either.
    assert_eq!(report.metrics.page.n_queries, 1);
    assert_eq!(report.metrics.page.answered.n_queries, 1);
    assert!(report.metrics.page.ndcg.mean > 0.99);
    // Latency is measured over the two answered queries.
    assert_eq!(report.metrics.latency_ms.n, 2);
    assert!(report.metrics.latency_ms.p95 >= report.metrics.latency_ms.p50);

    let metrics = std::fs::read_to_string(out_dir.join("metrics.json")).unwrap();
    assert!(metrics.contains("\"dataset\": \"kb-eval-e2e-"), "{metrics}");
    assert!(metrics.contains("\"n_failed\": 1"), "{metrics}");
    // Provenance: the harness names the server it measured (the fake state's
    // version is "test") and records that no route exposes a server commit.
    assert!(
        metrics.contains("\"server_version\": \"test\""),
        "{metrics}"
    );
    assert!(metrics.contains("\"server_commit\": null"), "{metrics}");
    assert!(metrics.contains("\"harness_commit\": \""), "{metrics}");
    let md = std::fs::read_to_string(out_dir.join("report.md")).unwrap();
    assert!(md.contains("1 of 3 queries failed"), "{md}");
    assert!(md.contains("Answered only (secondary)"), "{md}");
    assert!(md.contains("| document (answered only) | 2 |"), "{md}");

    let run = std::fs::read_to_string(out_dir.join("run.trec")).unwrap();
    assert!(run.contains("q1 Q0 alpha 1 1.000000 e2e"), "{run}");
    // q3 was never answered, so it must not appear as an empty ranking.
    assert!(!run.contains("q3"), "{run}");
    let page_run = std::fs::read_to_string(out_dir.join("run.page.trec")).unwrap();
    // The API's 0-based page 0 became the qrels' 1-based `#p1`.
    assert!(
        page_run.contains("q1 Q0 alpha#p1 1 1.000000 e2e"),
        "{page_run}"
    );

    let _ = std::fs::remove_dir_all(&dataset_dir);
}
