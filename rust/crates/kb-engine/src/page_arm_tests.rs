#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use kb_ports::{
    ChunkId, ChunkType, DocumentId, IndexUnit, PageEmbedder, PageImage, PageRenderer, PortError,
    ProvenanceArm, RetrievalDegrade, RetrievalIndex, TenantId,
};

use crate::fakes::{
    FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_hit, make_fake_scope,
};
use crate::in_memory_index::InMemoryIndex;
use crate::page_arm::{PageArm, PageArmConfig, PageIngest, merge_pages, page_unit_id};
use crate::retrieval::{RetrievalOptions, candidate_to_hit, execute_pipeline};
use crate::timing::SystemClock;

fn t(s: &str) -> TenantId {
    TenantId(s.into())
}
fn d(s: &str) -> DocumentId {
    DocumentId(s.into())
}

/// Returns a fixed query vector, or fails.
struct FixedPageEmbedder {
    query: Vec<f32>,
    fail: AtomicBool,
}

#[async_trait]
impl PageEmbedder for FixedPageEmbedder {
    async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        // Page n points along a different axis, so each page is distinguishable.
        Ok(pages
            .iter()
            .map(|p| {
                let mut v = vec![0.0; 3];
                v[p.page as usize % 3] = 1.0;
                v
            })
            .collect())
    }
    async fn embed_query(&self, _: &str) -> Result<Vec<f32>, PortError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(PortError::Unavailable("page embedder down".into()));
        }
        Ok(self.query.clone())
    }
    fn dimension(&self) -> u32 {
        3
    }
}

fn embedder(query: Vec<f32>) -> Arc<FixedPageEmbedder> {
    Arc::new(FixedPageEmbedder {
        query,
        fail: AtomicBool::new(false),
    })
}

fn page_unit(tenant: &str, doc: &str, page: u32, dense: Vec<f32>, chunks: &[&str]) -> IndexUnit {
    IndexUnit {
        id: page_unit_id(&d(doc), page),
        document_id: d(doc),
        tenant: t(tenant),
        knowledge_base_id: None,
        category: None,
        chunk_index: page,
        page: Some(page),
        chunk_type: ChunkType::Page,
        content: String::new(),
        contextual_prefix: None,
        metadata: serde_json::json!({ "chunkIds": chunks, "page": page }),
        dense,
        sparse: kb_core::sparse::SparseVector::default(),
    }
}

fn text_unit(tenant: &str, doc: &str, index: u32, page: u32, content: &str) -> IndexUnit {
    IndexUnit {
        id: ChunkId(format!("{doc}_{index}")),
        document_id: d(doc),
        tenant: t(tenant),
        knowledge_base_id: None,
        category: None,
        chunk_index: index,
        page: Some(page),
        chunk_type: ChunkType::Text,
        content: content.into(),
        contextual_prefix: None,
        metadata: serde_json::json!({ "page": page, "chunkIndex": index }),
        dense: vec![1.0, 0.0],
        sparse: kb_core::sparse::encode_document(content),
    }
}

/// The page index holds an exact match for the query in another tenant's
/// document. Searching tenant 1's scope must never surface it.
#[tokio::test]
async fn the_page_arm_never_returns_a_page_outside_the_scope() {
    let pages = Arc::new(InMemoryIndex::new(3));
    pages
        .upsert(vec![
            page_unit("t1", "d1", 0, vec![0.0, 1.0, 0.0], &["d1_0"]),
            page_unit("t1", "d1", 1, vec![0.6, 0.8, 0.0], &["d1_1"]),
            page_unit("t2", "d2", 0, vec![1.0, 0.0, 0.0], &["d2_0"]),
            page_unit("t1", "d3", 0, vec![1.0, 0.0, 0.0], &["d3_0"]),
        ])
        .await
        .unwrap();
    let arm = PageArm {
        embedder: embedder(vec![1.0, 0.0, 0.0]),
        index: pages,
        config: PageArmConfig::default(),
    };
    // Tenant 1, but only d1 visible: d3 (same tenant) is out of scope too.
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let ranked = arm.rank(&scope, "q").await.unwrap();
    assert_eq!(ranked.len(), 2);
    assert!(
        ranked.iter().all(|p| p.document_id == d("d1")),
        "{ranked:?}"
    );
    assert_eq!(ranked[0].page, 1, "the closer page ranks first");
    assert_eq!(ranked[0].chunk_ids, vec![ChunkId("d1_1".into())]);

    let empty = make_fake_scope(t("t1"), vec![]);
    assert!(arm.rank(&empty, "q").await.unwrap().is_empty());
}

#[tokio::test]
async fn merging_fuses_at_page_level_and_brings_in_image_only_pages() {
    let text: Arc<dyn RetrievalIndex> = Arc::new(InMemoryIndex::new(2));
    text.upsert(vec![
        text_unit("t1", "d1", 0, 0, "alpha page zero"),
        text_unit("t1", "d1", 1, 1, "beta page one"),
        text_unit("t1", "d1", 2, 2, "gamma page two"),
        text_unit("t1", "d1", 3, 2, "gamma second chunk"),
    ])
    .await
    .unwrap();
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let fetched = text
        .fetch(&scope, &[ChunkId("d1_0".into()), ChunkId("d1_1".into())])
        .await
        .unwrap();
    let mut text_hits: Vec<_> = fetched.into_iter().map(candidate_to_hit).collect();
    text_hits.sort_by_key(|h| h.chunk.metadata.chunk_index);

    let pages = vec![
        crate::page_arm::RankedPage {
            document_id: d("d1"),
            page: 2,
            score: 0.7,
            chunk_ids: vec![ChunkId("d1_2".into()), ChunkId("d1_3".into())],
        },
        crate::page_arm::RankedPage {
            document_id: d("d1"),
            page: 1,
            score: 0.5,
            chunk_ids: vec![ChunkId("d1_1".into())],
        },
    ];
    let (merged, partial) = merge_pages(
        &text,
        &scope,
        text_hits,
        &pages,
        &PageArmConfig::default(),
        10,
    )
    .await;
    assert!(!partial);
    let ids: Vec<&str> = merged.iter().map(|h| h.chunk.id.0.as_str()).collect();
    // Page 1 is in both lists and wins; page 0 (text #1) and page 2 (image #1)
    // tie, and text order breaks the tie. Page 2 brings only its first chunk.
    assert_eq!(ids, vec!["d1_1", "d1_0", "d1_2"]);
    let arms = |i: usize| {
        merged[i]
            .retrieval
            .as_ref()
            .map(|r| r.arms.clone())
            .unwrap_or_default()
    };
    assert!(arms(0).contains(&ProvenanceArm::Page));
    assert!(!arms(1).contains(&ProvenanceArm::Page));
    assert_eq!(arms(2), vec![ProvenanceArm::Page]);
    assert_eq!(merged[2].similarity, 0.7);
    assert_eq!(
        merged.iter().map(|h| h.rank).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

/// A page whose chunks are not visible (a stale page unit pointing at chunks of
/// a document outside the scope) brings nothing in.
#[tokio::test]
async fn an_image_page_cannot_fetch_chunks_outside_the_scope() {
    let text: Arc<dyn RetrievalIndex> = Arc::new(InMemoryIndex::new(2));
    text.upsert(vec![text_unit("t2", "d2", 0, 0, "secret")])
        .await
        .unwrap();
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let pages = vec![crate::page_arm::RankedPage {
        document_id: d("d1"),
        page: 0,
        score: 0.9,
        chunk_ids: vec![ChunkId("d2_0".into())],
    }];
    let (merged, _) = merge_pages(
        &text,
        &scope,
        Vec::new(),
        &pages,
        &PageArmConfig::default(),
        10,
    )
    .await;
    assert!(merged.is_empty(), "{merged:?}");
}

#[tokio::test]
async fn a_failing_page_arm_degrades_and_keeps_the_text_results() {
    let doc_store = Arc::new(FakeDocumentStore::default());
    let chunk_store = Arc::new(FakeChunkStore::default());
    chunk_store.vector_hits.write().unwrap().push(make_fake_hit(
        "c1",
        "d1",
        "Title 1",
        "Content 1",
        1,
        0.9,
    ));
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let failing = embedder(vec![1.0, 0.0, 0.0]);
    failing.fail.store(true, Ordering::SeqCst);
    let opts = RetrievalOptions {
        page_arm: Some(Arc::new(PageArm {
            embedder: failing,
            index: Arc::new(InMemoryIndex::new(3)),
            config: PageArmConfig::default(),
        })),
        min_similarity: 0.0,
        ..RetrievalOptions::default()
    };
    let outcome = execute_pipeline(
        &(doc_store as Arc<_>),
        &(chunk_store as Arc<_>),
        &(Arc::new(FakeEmbedder::default()) as Arc<_>),
        &(Arc::new(FakeReranker::default()) as Arc<_>),
        &(Arc::new(crate::NoopExpander) as Arc<_>),
        &scope,
        "query",
        &opts,
        &SystemClock,
    )
    .await
    .unwrap();
    assert!(
        outcome
            .degrades
            .contains(&RetrievalDegrade::PageArmUnavailable)
    );
    assert_eq!(outcome.chunks.len(), 1);
}

struct FakeRenderer(u32);

#[async_trait]
impl PageRenderer for FakeRenderer {
    async fn render(&self, mime: &str, _: &[u8], max: u32) -> Result<Vec<PageImage>, PortError> {
        if mime != "application/pdf" {
            return Ok(Vec::new());
        }
        Ok((0..self.0.min(max))
            .map(|page| PageImage {
                page,
                mime: "image/jpeg".into(),
                bytes: vec![page as u8; 4],
            })
            .collect())
    }
}

#[tokio::test]
async fn page_ingest_stores_one_unit_per_page_and_a_reingest_replaces_them() {
    let index = Arc::new(InMemoryIndex::new(3));
    let ingest = |n: u32| PageIngest {
        renderer: Arc::new(FakeRenderer(n)),
        embedder: embedder(vec![1.0, 0.0, 0.0]),
        index: index.clone(),
        max_pages: 100,
    };
    let mut by_page: HashMap<u32, Vec<ChunkId>> = HashMap::new();
    by_page.insert(1, vec![ChunkId("d1_4".into()), ChunkId("d1_5".into())]);
    let n = ingest(3)
        .index_document(
            &t("t1"),
            &d("d1"),
            None,
            "application/pdf",
            b"%PDF",
            &by_page,
        )
        .await
        .unwrap();
    assert_eq!(n, 3);
    let stored = index.ordered_chunks(&t("t1"), &d("d1")).await.unwrap();
    assert_eq!(stored.len(), 3);
    assert_eq!(
        stored[1].metadata["chunkIds"],
        serde_json::json!(["d1_4", "d1_5"])
    );
    assert_eq!(stored[1].chunk_type, ChunkType::Page);

    ingest(2)
        .index_document(
            &t("t1"),
            &d("d1"),
            None,
            "application/pdf",
            b"%PDF",
            &by_page,
        )
        .await
        .unwrap();
    assert_eq!(
        index
            .ordered_chunks(&t("t1"), &d("d1"))
            .await
            .unwrap()
            .len(),
        2
    );

    // Not a PDF: nothing rendered, and the old pages are gone.
    let n = ingest(5)
        .index_document(&t("t1"), &d("d1"), None, "text/plain", b"hi", &by_page)
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert!(
        index
            .ordered_chunks(&t("t1"), &d("d1"))
            .await
            .unwrap()
            .is_empty()
    );
}
