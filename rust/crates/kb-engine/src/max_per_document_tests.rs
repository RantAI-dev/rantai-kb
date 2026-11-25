//! `RET-17`: the deterministic per-document diversity cap.
//!
//! The instrument is the real pipeline over in-memory ports: ten hits from A
//! and two from B are seeded in a known rerank order, then the cap must keep
//! the first `N` of each document, drop the rest (not defer them), and report
//! the drop count. The neighbour weave runs after the cap, so it gets its own
//! test: a capped document must not regain chunks as neighbour context.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use kb_ports::{DocumentId, DocumentMeta, TenantId};

use crate::fakes::{
    FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_hit, make_fake_scope,
};
use crate::retrieval::{RetrievalOptions, execute_pipeline};
use crate::timing::SystemClock;

const TENANT: &str = "t1";

struct Fixture {
    docs: Arc<FakeDocumentStore>,
    chunks: Arc<FakeChunkStore>,
    embedder: Arc<FakeEmbedder>,
    reranker: Arc<FakeReranker>,
    scope: kb_ports::DocumentScope,
}

fn doc(id: &str) -> DocumentMeta {
    DocumentMeta {
        sha256: None,
        doc_key: None,
        version: None,
        supersedes_document_id: None,
        effective_from: None,
        effective_to: None,
        is_latest: true,
        acl_principals: Vec::new(),
        id: DocumentId(id.into()),
        title: format!("Title {id}"),
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
    }
}

impl Fixture {
    /// Twelve hits in rerank order: `a1..a10` then `b1, b2`. Descending
    /// similarities fix the fused order, and the fake reranker returns its
    /// candidates in that order with descending scores, so the rerank order
    /// is the same known order.
    fn new() -> Self {
        let tenant = TenantId(TENANT.into());
        let docs = Arc::new(FakeDocumentStore::default());
        for id in ["A", "B"] {
            docs.insert_doc(tenant.clone(), doc(id));
        }
        let chunks = Arc::new(FakeChunkStore::default());
        for i in 1..=10u32 {
            chunks.vector_hits.write().unwrap().push(make_fake_hit(
                &format!("a{i}"),
                "A",
                "Title A",
                "vendor A text",
                i,
                1.0 - i as f32 * 0.01,
            ));
        }
        for (i, sim) in [0.89f32, 0.88].into_iter().enumerate() {
            chunks.vector_hits.write().unwrap().push(make_fake_hit(
                &format!("b{}", i + 1),
                "B",
                "Title B",
                "vendor B text",
                11 + i as u32,
                sim,
            ));
        }
        Self {
            docs,
            chunks,
            embedder: Arc::new(FakeEmbedder::default()),
            reranker: Arc::new(FakeReranker::default()),
            scope: make_fake_scope(tenant, vec![DocumentId("A".into()), DocumentId("B".into())]),
        }
    }

    fn seed_neighbours(&self) {
        self.chunks.neighbour_hits.write().unwrap().extend([
            make_fake_hit("a-neighbour", "A", "Title A", "neighbour of A", 0, 0.5),
            make_fake_hit("b-neighbour", "B", "Title B", "neighbour of B", 10, 0.5),
        ]);
    }

    async fn run(&self, opts: RetrievalOptions) -> crate::RetrievalOutcome {
        let expander: Arc<dyn crate::QueryExpander> = Arc::new(crate::NoopExpander);
        execute_pipeline(
            &(self.docs.clone() as Arc<_>),
            &(self.chunks.clone() as Arc<_>),
            &(self.embedder.clone() as Arc<_>),
            &(self.reranker.clone() as Arc<_>),
            &expander,
            &self.scope,
            "query",
            &opts,
            &SystemClock,
        )
        .await
        .unwrap()
    }
}

fn opts(max_per_document: u32, max_chunks: u32) -> RetrievalOptions {
    RetrievalOptions {
        max_chunks,
        max_per_document,
        rerank_enabled: true,
        // Keep all twelve candidates inside the reranked head so the order is
        // the one the fake reranker saw.
        rerank_final_k: 12,
        rerank_max_k: 12,
        ..RetrievalOptions::default()
    }
}

fn ids(outcome: &crate::RetrievalOutcome) -> Vec<&str> {
    outcome
        .chunks
        .iter()
        .map(|h| h.chunk.id.0.as_str())
        .collect()
}

#[tokio::test]
async fn the_cap_keeps_n_per_document_in_rerank_order_and_drops_the_rest() {
    let fixture = Fixture::new();
    let outcome = fixture.run(opts(2, 4)).await;
    assert_eq!(
        ids(&outcome),
        ["a1", "a2", "b1", "b2"],
        "A's extra hits are dropped, not moved past B's"
    );
    assert_eq!(
        outcome.per_document_capped,
        Some(8),
        "eight of the twelve hits lost to the cap, before the final cut"
    );
}

#[tokio::test]
async fn a_zero_cap_leaves_the_ranking_and_stats_untouched() {
    let fixture = Fixture::new();
    let outcome = fixture.run(opts(0, 4)).await;
    assert_eq!(ids(&outcome), ["a1", "a2", "a3", "a4"]);
    assert_eq!(outcome.per_document_capped, None, "off is absent, not zero");
}

#[tokio::test]
async fn a_cap_of_one_keeps_exactly_one_hit_per_document() {
    let fixture = Fixture::new();
    let outcome = fixture.run(opts(1, 4)).await;
    assert_eq!(ids(&outcome), ["a1", "b1"]);
    assert_eq!(outcome.per_document_capped, Some(10));
}

#[tokio::test]
async fn the_cap_reports_zero_when_nothing_was_dropped() {
    let fixture = Fixture::new();
    // A cap as wide as the largest document keeps every hit; the final cut
    // still happens, but that is not a cap drop.
    let outcome = fixture.run(opts(10, 4)).await;
    assert_eq!(ids(&outcome), ["a1", "a2", "a3", "a4"]);
    assert_eq!(outcome.per_document_capped, Some(0));
}

#[tokio::test]
async fn neighbours_cannot_give_a_capped_document_chunks_back() {
    let fixture = Fixture::new();
    fixture.seed_neighbours();

    let capped = fixture
        .run(RetrievalOptions {
            neighbour_window: 1,
            ..opts(2, 4)
        })
        .await;
    let found = ids(&capped);
    assert!(
        !found.contains(&"a-neighbour") && !found.contains(&"b-neighbour"),
        "a capped document must not regain chunks through the neighbour weave: {found:?}"
    );

    let uncapped = fixture
        .run(RetrievalOptions {
            neighbour_window: 1,
            ..opts(0, 4)
        })
        .await;
    assert!(
        ids(&uncapped).contains(&"a-neighbour"),
        "the same neighbours are woven in when the cap is off"
    );
}
