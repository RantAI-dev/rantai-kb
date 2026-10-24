#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{
    BlobStore, ChunkId, ChunkType, DocumentId, IndexUnit, PageEmbedder, PageImage, PageRenderer,
    PageVisionScorer, PortError, ProvenanceArm, RetrievalDegrade, RetrievalIndex, SearchHit,
    TenantId, page_image_path,
};

use crate::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_hit,
    make_fake_scope,
};
use crate::in_memory_index::InMemoryIndex;
use crate::page_arm::{
    PageArm, PageArmConfig, PageIngest, PageOutcome, PageVision, PageVisionRerankConfig,
    merge_pages, page_unit_id, vision_rerank,
};
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
        vision: None,
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
    let outcome = merge_pages(
        &text,
        &scope,
        text_hits,
        &pages,
        &PageArmConfig::default(),
        10,
        None,
    )
    .await;
    assert!(!outcome.fetch_partial);
    let merged = outcome.hits;
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
    let merged = merge_pages(
        &text,
        &scope,
        Vec::new(),
        &pages,
        &PageArmConfig::default(),
        10,
        None,
    )
    .await
    .hits;
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
            vision: None,
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
                text: String::new(),
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
        blobs: None,
        text_chars: 0,
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
            &HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(n.indexed, 3);
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
            &HashMap::new(),
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
        .index_document(
            &t("t1"),
            &d("d1"),
            None,
            "text/plain",
            b"hi",
            &by_page,
            &HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(n, PageOutcome::default());
    assert!(
        index
            .ordered_chunks(&t("t1"), &d("d1"))
            .await
            .unwrap()
            .is_empty()
    );
}

/// Scores a candidate 1.0 when its text mentions "answer", else 0.0, or fails.
struct KeywordReranker {
    fail: bool,
}

#[async_trait]
impl kb_ports::Reranker for KeywordReranker {
    async fn rerank(
        &self,
        _query: &str,
        candidates: Vec<kb_ports::RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<kb_ports::RerankResult>, PortError> {
        if self.fail {
            return Err(PortError::Unavailable("reranker down".into()));
        }
        // Like voyage rerank-2.5: one empty document fails the whole call.
        if candidates.iter().any(|c| c.text.trim().is_empty()) {
            return Err(PortError::Unavailable("empty document".into()));
        }
        let mut scored: Vec<(f32, kb_ports::RerankCandidate)> = candidates
            .into_iter()
            .map(|c| (if c.text.contains("answer") { 1.0 } else { 0.0 }, c))
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap()
                .then(a.1.original_rank.cmp(&b.1.original_rank))
        });
        Ok(scored
            .into_iter()
            .take(final_k as usize)
            .enumerate()
            .map(|(i, (score, c))| kb_ports::RerankResult {
                id: c.id,
                final_rank: i as u32,
                score,
            })
            .collect())
    }
}

/// Page 2's stored text holds the answer, but the fused order puts it last.
/// The page reranker reads each page's stored text (not one chunk) and lifts it.
#[tokio::test]
async fn page_rerank_reads_page_text_and_lifts_the_answer_page() {
    let text: Arc<dyn RetrievalIndex> = Arc::new(InMemoryIndex::new(2));
    text.upsert(vec![
        text_unit("t1", "d1", 0, 0, "intro chunk"),
        text_unit("t1", "d1", 1, 1, "methods chunk"),
        text_unit("t1", "d1", 2, 2, "a table header only"),
    ])
    .await
    .unwrap();
    let page_index: Arc<dyn RetrievalIndex> = Arc::new(InMemoryIndex::new(3));
    let mut p2 = page_unit("t1", "d1", 2, vec![1.0, 0.0, 0.0], &["d1_2"]);
    p2.content = "full page text with the answer in a table row".into();
    let mut p0 = page_unit("t1", "d1", 0, vec![0.0, 1.0, 0.0], &["d1_0"]);
    p0.content = "intro page".into();
    page_index.upsert(vec![p0, p2]).await.unwrap();
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let text_hits: Vec<_> = {
        let mut v: Vec<_> = text
            .fetch(&scope, &[ChunkId("d1_0".into()), ChunkId("d1_1".into())])
            .await
            .unwrap()
            .into_iter()
            .map(candidate_to_hit)
            .collect();
        v.sort_by_key(|h| h.chunk.metadata.chunk_index);
        v
    };
    let pages = vec![crate::page_arm::RankedPage {
        document_id: d("d1"),
        page: 2,
        score: 0.1,
        chunk_ids: vec![ChunkId("d1_2".into())],
    }];
    let config = PageArmConfig {
        image_weight: 0.1,
        rerank: Some(crate::page_arm::PageRerankConfig::default()),
        ..PageArmConfig::default()
    };
    let ids = |hits: &[kb_ports::SearchHit]| -> Vec<String> {
        hits.iter().map(|h| h.chunk.id.0.clone()).collect()
    };

    let plain = merge_pages(&text, &scope, text_hits.clone(), &pages, &config, 10, None).await;
    assert_eq!(ids(&plain.hits).last().map(String::as_str), Some("d1_2"));

    let reranker: Arc<dyn kb_ports::Reranker> = Arc::new(KeywordReranker { fail: false });
    let ctx = crate::page_arm::PageRerankContext {
        page_index: &page_index,
        reranker: &reranker,
        query: "q",
    };
    let reranked = merge_pages(
        &text,
        &scope,
        text_hits.clone(),
        &pages,
        &config,
        10,
        Some(ctx),
    )
    .await;
    assert!(!reranked.rerank_failed);
    assert_eq!(ids(&reranked.hits)[0], "d1_2", "{:?}", ids(&reranked.hits));

    let failing: Arc<dyn kb_ports::Reranker> = Arc::new(KeywordReranker { fail: true });
    let ctx = crate::page_arm::PageRerankContext {
        page_index: &page_index,
        reranker: &failing,
        query: "q",
    };
    let degraded = merge_pages(&text, &scope, text_hits, &pages, &config, 10, Some(ctx)).await;
    assert!(degraded.rerank_failed);
    assert_eq!(ids(&degraded.hits), ids(&plain.hits), "fused order stands");
}

/// An image-only page with no stored text and no chunk hits must not be sent
/// to the reranker as an empty document (voyage rejects the whole call).
#[tokio::test]
async fn page_rerank_skips_pages_with_no_text() {
    let text: Arc<dyn RetrievalIndex> = Arc::new(InMemoryIndex::new(2));
    text.upsert(vec![
        text_unit("t1", "d1", 0, 0, "intro chunk"),
        text_unit("t1", "d1", 2, 2, "a table header only"),
    ])
    .await
    .unwrap();
    let page_index: Arc<dyn RetrievalIndex> = Arc::new(InMemoryIndex::new(3));
    let mut p2 = page_unit("t1", "d1", 2, vec![1.0, 0.0, 0.0], &["d1_2"]);
    p2.content = "full page text with the answer".into();
    let mut p0 = page_unit("t1", "d1", 0, vec![0.0, 1.0, 0.0], &["d1_0"]);
    p0.content = "intro page".into();
    let blank = page_unit("t1", "d1", 7, vec![0.0, 0.0, 1.0], &[]);
    page_index.upsert(vec![p0, p2, blank]).await.unwrap();
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let text_hits: Vec<_> = text
        .fetch(&scope, &[ChunkId("d1_0".into())])
        .await
        .unwrap()
        .into_iter()
        .map(candidate_to_hit)
        .collect();
    let page = |n: u32, chunks: Vec<ChunkId>| crate::page_arm::RankedPage {
        document_id: d("d1"),
        page: n,
        score: 0.1,
        chunk_ids: chunks,
    };
    let pages = vec![page(7, vec![]), page(2, vec![ChunkId("d1_2".into())])];
    let config = PageArmConfig {
        rerank: Some(crate::page_arm::PageRerankConfig::default()),
        ..PageArmConfig::default()
    };
    let reranker: Arc<dyn kb_ports::Reranker> = Arc::new(KeywordReranker { fail: false });
    let ctx = crate::page_arm::PageRerankContext {
        page_index: &page_index,
        reranker: &reranker,
        query: "q",
    };
    let out = merge_pages(&text, &scope, text_hits, &pages, &config, 10, Some(ctx)).await;
    assert!(!out.rerank_failed, "a blank page must not fail the rerank");
    assert_eq!(out.hits[0].chunk.id.0, "d1_2");
}

/// A long PDF's pages are all stored even though they go in several batches.
#[tokio::test]
async fn page_ingest_stores_every_page_of_a_long_pdf() {
    let index = Arc::new(InMemoryIndex::new(3));
    let ingest = PageIngest {
        renderer: Arc::new(FakeRenderer(150)),
        embedder: embedder(vec![1.0, 0.0, 0.0]),
        index: index.clone(),
        max_pages: 500,
        blobs: None,
        text_chars: 0,
    };
    let n = ingest
        .index_document(
            &t("t1"),
            &d("d1"),
            None,
            "application/pdf",
            b"%PDF",
            &HashMap::new(),
            &HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(n.indexed, 150);
    let stored = index.ordered_chunks(&t("t1"), &d("d1")).await.unwrap();
    assert_eq!(stored.len(), 150);
}

/// Fails any request that contains one given page.
struct FailOnPage(u32);

#[async_trait]
impl PageEmbedder for FailOnPage {
    async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        if pages.iter().any(|p| p.page == self.0) {
            return Err(PortError::Unavailable("provider down".into()));
        }
        Ok(pages.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
    }
    async fn embed_query(&self, _: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![1.0, 0.0, 0.0])
    }
    fn dimension(&self) -> u32 {
        3
    }
}

/// One failed request costs its own window of pages, not the whole document,
/// and the outcome says how many pages are missing.
#[tokio::test]
async fn a_failed_page_window_loses_only_its_own_pages() {
    let index = Arc::new(InMemoryIndex::new(3));
    let ingest = PageIngest {
        renderer: Arc::new(FakeRenderer(150)),
        embedder: Arc::new(FailOnPage(70)),
        index: index.clone(),
        max_pages: 500,
        blobs: None,
        text_chars: 0,
    };
    let out = ingest
        .index_document(
            &t("t1"),
            &d("d1"),
            None,
            "application/pdf",
            b"%PDF",
            &HashMap::new(),
            &HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        (out.indexed, out.failed),
        (86, 64),
        "window 64..128 is lost"
    );
    assert!(out.error.as_deref().unwrap().contains("provider down"));
    let stored = index.ordered_chunks(&t("t1"), &d("d1")).await.unwrap();
    assert_eq!(stored.len(), 86);
    assert!(stored.iter().all(|c| !(64..128).contains(&c.chunk_index)));
}

/// Three pages, the middle one unrenderable (no bytes).
struct OneBadPage;

#[async_trait]
impl PageRenderer for OneBadPage {
    async fn render(&self, _: &str, _: &[u8], _: u32) -> Result<Vec<PageImage>, PortError> {
        Ok((0..3)
            .map(|page| PageImage {
                page,
                mime: "image/jpeg".into(),
                bytes: if page == 1 { Vec::new() } else { vec![1] },
                text: String::new(),
            })
            .collect())
    }
}

/// A page that could not be rendered is counted, never sent to the embedder.
#[tokio::test]
async fn an_unrendered_page_is_counted_and_skipped() {
    let index = Arc::new(InMemoryIndex::new(3));
    let ingest = PageIngest {
        renderer: Arc::new(OneBadPage),
        embedder: Arc::new(FailOnPage(1)),
        index: index.clone(),
        max_pages: 10,
        blobs: None,
        text_chars: 0,
    };
    let out = ingest
        .index_document(
            &t("t1"),
            &d("d1"),
            None,
            "application/pdf",
            b"%PDF",
            &HashMap::new(),
            &HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!((out.indexed, out.failed), (2, 1));
    assert!(out.error.unwrap().contains("first: page 2"));
}

// ── Live vision rerank ──────────────────────────────────────────────────────

/// A blob store that records every key it is asked for, so a test can assert
/// which pages were read — and, just as important, which were not.
#[derive(Clone, Default)]
struct RecordingBlob {
    objects: Arc<std::sync::RwLock<HashMap<String, Vec<u8>>>>,
    downloads: Arc<std::sync::RwLock<Vec<String>>>,
}

impl RecordingBlob {
    async fn put(&self, document: &str, page: u32) {
        let key = page_image_path(Some("t1"), document, page);
        self.objects
            .write()
            .unwrap()
            .insert(key, vec![page as u8; 4]);
    }

    fn downloaded(&self) -> Vec<String> {
        self.downloads.read().unwrap().clone()
    }
}

#[async_trait]
impl BlobStore for RecordingBlob {
    async fn upload(&self, key: &str, body: Vec<u8>, _: &str) -> Result<u64, PortError> {
        let len = body.len() as u64;
        self.objects.write().unwrap().insert(key.to_string(), body);
        Ok(len)
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, PortError> {
        self.downloads.write().unwrap().push(key.to_string());
        self.objects
            .read()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| PortError::Unavailable(format!("no blob at {key}")))
    }

    async fn delete(&self, key: &str) -> Result<(), PortError> {
        self.objects.write().unwrap().remove(key);
        Ok(())
    }
}

/// Returns fixed scores, optionally after a delay, or an error when `scores`
/// is `None`.
struct FakeVisionScorer {
    scores: std::sync::Mutex<Option<Vec<f32>>>,
    delay: Duration,
    calls: AtomicUsize,
}

impl FakeVisionScorer {
    fn new(scores: Vec<f32>) -> Arc<Self> {
        Arc::new(Self {
            scores: std::sync::Mutex::new(Some(scores)),
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            scores: std::sync::Mutex::new(None),
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
        })
    }

    fn slow(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            scores: std::sync::Mutex::new(Some(vec![5.0, 4.0, 3.0, 2.0])),
            delay,
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl PageVisionScorer for FakeVisionScorer {
    async fn score(&self, _: &str, pages: &[PageImage]) -> Result<Vec<f32>, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let guard = self.scores.lock().unwrap();
        match guard.as_ref() {
            Some(scores) => Ok(scores.iter().copied().take(pages.len()).collect()),
            None => Err(PortError::Unavailable("vision scorer down".into())),
        }
    }
}

fn page_hit(id: &str, document: &str, page: u32, score: f32) -> SearchHit {
    let mut hit = make_fake_hit(id, document, "Title", "content", page, score);
    hit.chunk.metadata.page = Some(page);
    hit
}

fn recorded_hits(hits: &[SearchHit]) -> Vec<String> {
    hits.iter().map(|h| h.chunk.id.0.clone()).collect()
}

fn vision_config(depth: usize, timeout: Duration) -> PageVisionRerankConfig {
    PageVisionRerankConfig {
        depth,
        weight: 0.6,
        rrf_k: 5,
        timeout,
    }
}

/// The blend formula: `(1-w)/(k + run_rank + 1) + w/(k + vision_rank + 1)`.
///
/// Pages 0,1,2 have run ranks 0,1,2 and vision scores 2,9,1, so the vision
/// order is 1,0,2. With w=0.6, k=5:
///   page 1: 0.4/7 + 0.6/6 = 0.15714
///   page 0: 0.4/6 + 0.6/7 = 0.15238
///   page 2: 0.4/8 + 0.6/8 = 0.12500
/// Page 3 is outside `depth` and keeps its place even though its score is high.
#[tokio::test]
async fn vision_rerank_reorders_only_the_top_depth_page_slots() {
    let pages = ["p0", "p1", "p2", "p3"];
    let mut hits: Vec<_> = pages
        .iter()
        .enumerate()
        .map(|(i, id)| page_hit(id, "d1", u32::try_from(i).unwrap(), 1.0))
        .collect();
    let blobs = Arc::new(RecordingBlob::default());
    for page in 1..=3 {
        blobs.put("d1", page).await;
    }
    let scorer: Arc<dyn PageVisionScorer> = FakeVisionScorer::new(vec![2.0, 9.0, 1.0, 8.0]);
    let blobs: Arc<dyn BlobStore> = blobs.clone();
    let config = vision_config(3, Duration::from_secs(5));
    let ctx = crate::page_arm::PageVisionContext {
        scorer: &scorer,
        blobs: &blobs,
        config: &config,
        query: "q",
        tenant: &t("t1"),
    };
    vision_rerank(&mut hits, &ctx).await.unwrap();
    assert_eq!(recorded_hits(&hits), vec!["p1", "p0", "p2", "p3"]);
}

#[tokio::test]
async fn vision_rerank_keeps_the_order_when_the_scorer_fails() {
    let mut hits: Vec<_> = (0..3)
        .map(|i| page_hit(&format!("p{i}"), "d1", i, 1.0))
        .collect();
    let before = recorded_hits(&hits);
    let blobs = Arc::new(RecordingBlob::default());
    for page in 1..=3 {
        blobs.put("d1", page).await;
    }
    let scorer: Arc<dyn PageVisionScorer> = FakeVisionScorer::failing();
    let blobs: Arc<dyn BlobStore> = blobs.clone();
    let config = vision_config(3, Duration::from_secs(5));
    let ctx = crate::page_arm::PageVisionContext {
        scorer: &scorer,
        blobs: &blobs,
        config: &config,
        query: "q",
        tenant: &t("t1"),
    };
    assert!(vision_rerank(&mut hits, &ctx).await.is_err());
    assert_eq!(recorded_hits(&hits), before);
}

#[tokio::test]
async fn vision_rerank_keeps_the_order_on_timeout() {
    let mut hits: Vec<_> = (0..3)
        .map(|i| page_hit(&format!("p{i}"), "d1", i, 1.0))
        .collect();
    let before = recorded_hits(&hits);
    let blobs = Arc::new(RecordingBlob::default());
    for page in 1..=3 {
        blobs.put("d1", page).await;
    }
    let scorer: Arc<dyn PageVisionScorer> = FakeVisionScorer::slow(Duration::from_millis(200));
    let blobs: Arc<dyn BlobStore> = blobs.clone();
    let config = vision_config(3, Duration::from_millis(5));
    let ctx = crate::page_arm::PageVisionContext {
        scorer: &scorer,
        blobs: &blobs,
        config: &config,
        query: "q",
        tenant: &t("t1"),
    };
    let err = vision_rerank(&mut hits, &ctx).await.unwrap_err();
    assert!(matches!(err, PortError::Timeout), "{err}");
    assert_eq!(recorded_hits(&hits), before);
}

#[tokio::test]
async fn vision_rerank_keeps_the_order_when_a_page_image_is_missing() {
    let mut hits: Vec<_> = (0..3)
        .map(|i| page_hit(&format!("p{i}"), "d1", i, 1.0))
        .collect();
    let before = recorded_hits(&hits);
    let blobs = Arc::new(RecordingBlob::default());
    // Page 2's image was never stored.
    blobs.put("d1", 1).await;
    blobs.put("d1", 3).await;
    let scorer: Arc<dyn PageVisionScorer> = FakeVisionScorer::new(vec![1.0, 2.0, 3.0]);
    let blobs: Arc<dyn BlobStore> = blobs.clone();
    let config = vision_config(3, Duration::from_secs(5));
    let ctx = crate::page_arm::PageVisionContext {
        scorer: &scorer,
        blobs: &blobs,
        config: &config,
        query: "q",
        tenant: &t("t1"),
    };
    assert!(vision_rerank(&mut hits, &ctx).await.is_err());
    assert_eq!(recorded_hits(&hits), before);
}

/// Runs a search with a page arm and a vision stage over a small scoped
/// corpus. The text index returns `text_hits`; the page index returns `pages`.
#[allow(clippy::too_many_arguments)]
async fn run_pipeline_with_vision(
    scope: &kb_ports::DocumentScope,
    pages: Vec<IndexUnit>,
    text_hits: Vec<SearchHit>,
    vision: Option<PageVision>,
    rerank_enabled: bool,
) -> crate::RetrievalOutcome {
    let page_index = Arc::new(InMemoryIndex::new(3));
    page_index.upsert(pages).await.unwrap();
    let doc_store = Arc::new(FakeDocumentStore::default());
    let chunk_store = Arc::new(FakeChunkStore::default());
    chunk_store.vector_hits.write().unwrap().extend(text_hits);
    let opts = RetrievalOptions {
        page_arm: Some(Arc::new(PageArm {
            embedder: embedder(vec![1.0, 0.0, 0.0]),
            index: page_index,
            config: PageArmConfig::default(),
            vision,
        })),
        min_similarity: 0.0,
        rerank_enabled,
        ..RetrievalOptions::default()
    };
    execute_pipeline(
        &(doc_store as Arc<_>),
        &(chunk_store as Arc<_>),
        &(Arc::new(FakeEmbedder::default()) as Arc<_>),
        &(Arc::new(FakeReranker::default()) as Arc<_>),
        &(Arc::new(crate::NoopExpander) as Arc<_>),
        scope,
        "query",
        &opts,
        &SystemClock,
    )
    .await
    .unwrap()
}

fn vision_slot(
    scorer: Arc<dyn PageVisionScorer>,
    blobs: Arc<RecordingBlob>,
    config: PageVisionRerankConfig,
) -> PageVision {
    PageVision {
        scorer,
        blobs: blobs as Arc<dyn BlobStore>,
        config,
    }
}

fn pipeline_corpus() -> (Vec<IndexUnit>, Vec<SearchHit>) {
    let pages = (0..3)
        .map(|page| {
            let dense = vec![1.0, 0.01 * f32::from(u8::try_from(page).unwrap()), 0.0];
            page_unit("t1", "d1", page, dense, &[])
        })
        .collect();
    let hits = (0..3)
        .map(|i| page_hit(&format!("d1_{i}"), "d1", i, 0.9 - 0.1 * f32::from(i as u8)))
        .collect();
    (pages, hits)
}

#[tokio::test]
async fn a_failing_vision_rerank_degrades_and_keeps_the_page_order() {
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let (pages, hits) = pipeline_corpus();
    let blobs = Arc::new(RecordingBlob::default());
    for page in 1..=3 {
        blobs.put("d1", page).await;
    }
    let baseline = run_pipeline_with_vision(&scope, pages.clone(), hits.clone(), None, false).await;
    let outcome = run_pipeline_with_vision(
        &scope,
        pages,
        hits,
        Some(vision_slot(
            FakeVisionScorer::failing(),
            Arc::clone(&blobs),
            vision_config(3, Duration::from_secs(5)),
        )),
        true,
    )
    .await;
    assert!(
        outcome
            .degrades
            .contains(&RetrievalDegrade::VisionRerankUnavailable),
        "{:?}",
        outcome.degrades
    );
    assert_eq!(
        recorded_hits(&outcome.chunks),
        recorded_hits(&baseline.chunks)
    );
    assert!(outcome.timings.vision_rerank_ms > 0.0);
}

#[tokio::test]
async fn a_timed_out_vision_rerank_degrades_and_keeps_the_page_order() {
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let (pages, hits) = pipeline_corpus();
    let blobs = Arc::new(RecordingBlob::default());
    for page in 1..=3 {
        blobs.put("d1", page).await;
    }
    let baseline = run_pipeline_with_vision(&scope, pages.clone(), hits.clone(), None, false).await;
    let outcome = run_pipeline_with_vision(
        &scope,
        pages,
        hits,
        Some(vision_slot(
            FakeVisionScorer::slow(Duration::from_millis(200)),
            Arc::clone(&blobs),
            vision_config(3, Duration::from_millis(5)),
        )),
        true,
    )
    .await;
    assert!(
        outcome
            .degrades
            .contains(&RetrievalDegrade::VisionRerankUnavailable),
        "{:?}",
        outcome.degrades
    );
    assert_eq!(
        recorded_hits(&outcome.chunks),
        recorded_hits(&baseline.chunks)
    );
}

#[tokio::test]
async fn a_missing_page_image_degrades_and_keeps_the_page_order() {
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let (pages, hits) = pipeline_corpus();
    let blobs = Arc::new(RecordingBlob::default());
    // Page 2's image is absent.
    blobs.put("d1", 1).await;
    blobs.put("d1", 3).await;
    let baseline = run_pipeline_with_vision(&scope, pages.clone(), hits.clone(), None, false).await;
    let outcome = run_pipeline_with_vision(
        &scope,
        pages,
        hits,
        Some(vision_slot(
            FakeVisionScorer::new(vec![3.0, 2.0, 1.0]),
            Arc::clone(&blobs),
            vision_config(3, Duration::from_secs(5)),
        )),
        true,
    )
    .await;
    assert!(
        outcome
            .degrades
            .contains(&RetrievalDegrade::VisionRerankUnavailable),
        "{:?}",
        outcome.degrades
    );
    assert_eq!(
        recorded_hits(&outcome.chunks),
        recorded_hits(&baseline.chunks)
    );
}

#[tokio::test]
async fn the_vision_stage_reads_no_images_when_it_is_off() {
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let (pages, hits) = pipeline_corpus();
    let blobs = Arc::new(RecordingBlob::default());
    for page in 1..=3 {
        blobs.put("d1", page).await;
    }
    // Not configured at all.
    let outcome = run_pipeline_with_vision(&scope, pages.clone(), hits.clone(), None, true).await;
    assert!(outcome.timings.vision_rerank_ms <= 0.0);
    // Configured, but reranking is off for this search.
    let outcome = run_pipeline_with_vision(
        &scope,
        pages,
        hits,
        Some(vision_slot(
            FakeVisionScorer::new(vec![1.0, 2.0, 3.0]),
            Arc::clone(&blobs),
            vision_config(3, Duration::from_secs(5)),
        )),
        false,
    )
    .await;
    assert!(outcome.timings.vision_rerank_ms <= 0.0);
    assert!(
        blobs.downloaded().is_empty(),
        "the stage must not read images when it will not run: {:?}",
        blobs.downloaded()
    );
}

/// A page of another tenant's document matches the query exactly, and its image
/// exists. The search must never read it: only pages of scoped hits are fetched.
#[tokio::test]
async fn vision_never_reads_a_page_outside_the_scope() {
    let scope = make_fake_scope(t("t1"), vec![d("d1")]);
    let mut pages: Vec<IndexUnit> = (0..2)
        .map(|page| page_unit("t1", "d1", page, vec![1.0, 0.0, 0.0], &[]))
        .collect();
    pages.push(page_unit("t2", "d2", 0, vec![1.0, 0.0, 0.0], &[]));
    let hits = (0..2)
        .map(|i| page_hit(&format!("d1_{i}"), "d1", i, 0.9))
        .collect();
    let blobs = Arc::new(RecordingBlob::default());
    for page in 1..=2 {
        blobs.put("d1", page).await;
    }
    // The out-of-scope tenant's page image exists and is readable.
    let foreign = page_image_path(Some("t2"), "d2", 1);
    blobs
        .objects
        .write()
        .unwrap()
        .insert(foreign.clone(), vec![9; 4]);
    let outcome = run_pipeline_with_vision(
        &scope,
        pages,
        hits,
        Some(vision_slot(
            FakeVisionScorer::new(vec![9.0, 1.0]),
            Arc::clone(&blobs),
            vision_config(2, Duration::from_secs(5)),
        )),
        true,
    )
    .await;
    assert!(
        !outcome
            .degrades
            .contains(&RetrievalDegrade::VisionRerankUnavailable)
    );
    let downloaded = blobs.downloaded();
    assert!(!downloaded.is_empty(), "the stage did run");
    assert!(
        downloaded
            .iter()
            .all(|key| key.starts_with("documents/t1/d1/")),
        "a page outside the scope was read: {downloaded:?}"
    );
    assert!(!downloaded.contains(&foreign));
}

// ── Storing page images at ingest ───────────────────────────────────────────

#[tokio::test]
async fn page_images_are_stored_under_the_page_key() {
    let index = Arc::new(InMemoryIndex::new(3));
    let blobs = Arc::new(FakeBlobStore::default());
    let ingest = PageIngest {
        renderer: Arc::new(FakeRenderer(3)),
        embedder: embedder(vec![1.0, 0.0, 0.0]),
        index: index.clone(),
        blobs: Some(blobs.clone()),
        max_pages: 100,
        text_chars: 0,
    };
    let out = ingest
        .index_document(
            &t("t1"),
            &d("d1"),
            None,
            "application/pdf",
            b"%PDF",
            &HashMap::new(),
            &HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(out.indexed, 3);
    assert_eq!(out.images_failed, 0);
    let objects = blobs.objects.read().unwrap();
    for page in 1..=3 {
        let key = page_image_path(Some("t1"), "d1", page);
        assert_eq!(
            objects.get(&key).map(Vec::as_slice),
            Some(vec![(page - 1) as u8; 4].as_slice()),
            "page {page} image missing or wrong: {objects:?}"
        );
    }
}

/// A blob store that refuses every write.
#[derive(Clone, Default)]
struct FailingBlob;

#[async_trait]
impl BlobStore for FailingBlob {
    async fn upload(&self, _: &str, _: Vec<u8>, _: &str) -> Result<u64, PortError> {
        Err(PortError::Unavailable("bucket is down".into()))
    }
    async fn download(&self, _: &str) -> Result<Vec<u8>, PortError> {
        Err(PortError::Unavailable("bucket is down".into()))
    }
    async fn delete(&self, _: &str) -> Result<(), PortError> {
        Err(PortError::Unavailable("bucket is down".into()))
    }
}

#[tokio::test]
async fn a_failed_page_image_write_does_not_fail_the_window() {
    let index = Arc::new(InMemoryIndex::new(3));
    let ingest = PageIngest {
        renderer: Arc::new(FakeRenderer(3)),
        embedder: embedder(vec![1.0, 0.0, 0.0]),
        index: index.clone(),
        blobs: Some(Arc::new(FailingBlob)),
        max_pages: 100,
        text_chars: 0,
    };
    let out = ingest
        .index_document(
            &t("t1"),
            &d("d1"),
            None,
            "application/pdf",
            b"%PDF",
            &HashMap::new(),
            &HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(out.indexed, 3, "the pages are still searchable");
    assert_eq!(out.failed, 0);
    assert_eq!(out.images_failed, 3);
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(
        index
            .ordered_chunks(&t("t1"), &d("d1"))
            .await
            .unwrap()
            .len(),
        3
    );
}
