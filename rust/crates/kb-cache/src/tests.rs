#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use kb_ports::{
    Embedder, ExtractedFigure, ExtractionOptions, ExtractionResult, Extractor, PortError,
};

use crate::{CachedEmbedder, CachedExtractor, DiskCache};

fn temp_root() -> PathBuf {
    std::env::temp_dir().join(format!("kb-cache-test-{}", uuid::Uuid::new_v4()))
}

/// Embeds a text as `[len, first byte, 1.0]`, and counts what it was asked.
#[derive(Default)]
struct CountingEmbedder {
    passages: AtomicUsize,
    queries: AtomicUsize,
    calls: AtomicUsize,
}

fn vec_for(t: &str) -> Vec<f32> {
    vec![
        t.len() as f32,
        f32::from(t.bytes().next().unwrap_or(0)),
        1.0,
    ]
}

#[async_trait]
impl Embedder for CountingEmbedder {
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(vec_for(text))
    }
    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.passages.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts.iter().map(|t| vec_for(t)).collect())
    }
    fn dimension(&self) -> u32 {
        3
    }
}

fn texts(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

#[tokio::test]
async fn second_pass_makes_no_provider_calls_and_keeps_order() {
    let root = temp_root();
    let inner = Arc::new(CountingEmbedder::default());
    let cached = CachedEmbedder::new(inner.clone(), DiskCache::new(&root), "model-a|3");

    let first = cached
        .embed_passages(&texts(&["alpha", "bb", "c"]))
        .await
        .unwrap();
    // A new wrapper on the same directory: the cache survives a restart.
    let cached2 = CachedEmbedder::new(inner.clone(), DiskCache::new(&root), "model-a|3");
    let second = cached2
        .embed_passages(&texts(&["alpha", "bb", "c"]))
        .await
        .unwrap();

    assert_eq!(first, second);
    assert_eq!(first[0], vec_for("alpha"));
    assert_eq!(inner.passages.load(Ordering::SeqCst), 3);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn only_misses_are_sent_in_one_call_and_results_stay_aligned() {
    let root = temp_root();
    let inner = Arc::new(CountingEmbedder::default());
    let cached = CachedEmbedder::new(inner.clone(), DiskCache::new(&root), "m");
    cached.embed_passages(&texts(&["b", "d"])).await.unwrap();

    let out = cached
        .embed_passages(&texts(&["a", "b", "c", "d", "eeee"]))
        .await
        .unwrap();

    let want: Vec<Vec<f32>> = ["a", "b", "c", "d", "eeee"]
        .iter()
        .map(|t| vec_for(t))
        .collect();
    assert_eq!(out, want);
    assert_eq!(inner.passages.load(Ordering::SeqCst), 2 + 3);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_different_fingerprint_or_kind_is_a_different_entry() {
    let root = temp_root();
    let inner = Arc::new(CountingEmbedder::default());
    let a = CachedEmbedder::new(inner.clone(), DiskCache::new(&root), "model-a");
    let b = CachedEmbedder::new(inner.clone(), DiskCache::new(&root), "model-b");
    a.embed_passages(&texts(&["x"])).await.unwrap();
    b.embed_passages(&texts(&["x"])).await.unwrap();
    assert_eq!(inner.passages.load(Ordering::SeqCst), 2);

    // A passage vector is never served as a query vector.
    let q = a.embed_query_detailed("x").await.unwrap();
    assert!(!q.cached);
    assert_eq!(inner.queries.load(Ordering::SeqCst), 1);
    let q2 = a.embed_query_detailed("x").await.unwrap();
    assert!(q2.cached);
    assert_eq!(inner.queries.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_corrupt_entry_is_a_miss_not_a_wrong_vector() {
    let root = temp_root();
    let inner = Arc::new(CountingEmbedder::default());
    let cached = CachedEmbedder::new(inner.clone(), DiskCache::new(&root), "m");
    cached.embed_passages(&texts(&["hello"])).await.unwrap();
    // Truncate every stored file.
    for e in walk(&root) {
        std::fs::write(e, [0u8; 5]).unwrap();
    }
    let out = cached.embed_passages(&texts(&["hello"])).await.unwrap();
    assert_eq!(out[0], vec_for("hello"));
    assert_eq!(inner.passages.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_unwritable_cache_degrades_to_the_provider() {
    let root = temp_root();
    std::fs::write(&root, b"not a directory").unwrap();
    let inner = Arc::new(CountingEmbedder::default());
    let disk = DiskCache::new(&root);
    let cached = CachedEmbedder::new(inner.clone(), disk.clone(), "m");
    let out = cached.embed_passages(&texts(&["a", "b"])).await.unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(disk.stats().write_errors(), 2);
    let _ = std::fs::remove_file(root);
}

struct WrongDim;
#[async_trait]
impl Embedder for WrongDim {
    async fn embed_query(&self, _: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![1.0])
    }
    async fn embed_passages(&self, t: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        Ok(t.iter().map(|_| vec![1.0]).collect())
    }
    fn dimension(&self) -> u32 {
        3
    }
}

#[tokio::test]
async fn a_wrong_dimension_is_an_error_and_is_not_stored() {
    let root = temp_root();
    let cached = CachedEmbedder::new(Arc::new(WrongDim), DiskCache::new(&root), "m");
    assert!(cached.embed_passages(&texts(&["a"])).await.is_err());
    assert!(walk(&root).is_empty());
    let _ = std::fs::remove_dir_all(root);
}

fn walk(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Returns a fixed result with one figure, or an error result when asked.
struct FakeExtractor {
    calls: AtomicUsize,
    fail: bool,
}

#[async_trait]
impl Extractor for FakeExtractor {
    async fn extract_with_options(
        &self,
        _filename: &str,
        _mime: &str,
        bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Ok(ExtractionResult {
                error: Some("sidecar down".into()),
                ..Default::default()
            });
        }
        Ok(ExtractionResult {
            content: format!("{} bytes, figures={}", bytes.len(), options.figures),
            file_type: "pdf".into(),
            used_ocr: true,
            figures: vec![ExtractedFigure {
                kind: "chart".into(),
                page: 2,
                bbox: [1.0, 2.0, 3.0, 4.0],
                caption: Some("cap".into()),
                image_bytes: vec![9, 8, 7, 6],
                id: Some("f1".into()),
            }],
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn extraction_is_cached_by_content_not_name() {
    let root = temp_root();
    let inner = Arc::new(FakeExtractor {
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let cached = CachedExtractor::new(inner.clone(), DiskCache::new(&root), "chain-v1");
    let a = cached
        .extract("report.pdf", "application/pdf", b"%PDF-1")
        .await
        .unwrap();
    let b = cached
        .extract("renamed.PDF", "application/pdf", b"%PDF-1")
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(a.content, b.content);
    assert_eq!(b.figures[0].image_bytes, vec![9, 8, 7, 6]);
    assert_eq!(b.figures[0].page, 2);
    assert!(b.used_ocr);

    // Different bytes, options or fingerprint: a new parse.
    cached
        .extract("report.pdf", "application/pdf", b"%PDF-2")
        .await
        .unwrap();
    let opts = ExtractionOptions {
        figures: true,
        ..Default::default()
    };
    cached
        .extract_with_options("report.pdf", "application/pdf", b"%PDF-1", opts)
        .await
        .unwrap();
    let other = CachedExtractor::new(inner.clone(), DiskCache::new(&root), "chain-v2");
    other
        .extract("report.pdf", "application/pdf", b"%PDF-1")
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 4);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn failed_extractions_are_retried_not_cached() {
    let root = temp_root();
    let inner = Arc::new(FakeExtractor {
        calls: AtomicUsize::new(0),
        fail: true,
    });
    let cached = CachedExtractor::new(inner.clone(), DiskCache::new(&root), "c");
    for _ in 0..2 {
        let r = cached
            .extract("a.pdf", "application/pdf", b"x")
            .await
            .unwrap();
        assert!(r.error.is_some());
    }
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(root);
}

/// Counts page and query embeds; vector = [bytes len, 1.0].
#[derive(Default)]
struct CountingPages {
    pages: AtomicUsize,
    queries: AtomicUsize,
}

#[async_trait]
impl kb_ports::PageEmbedder for CountingPages {
    async fn embed_pages(&self, p: &[kb_ports::PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        self.pages.fetch_add(p.len(), Ordering::SeqCst);
        Ok(p.iter().map(|x| vec![x.bytes.len() as f32, 1.0]).collect())
    }
    async fn embed_query(&self, t: &str) -> Result<Vec<f32>, PortError> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(vec![t.len() as f32, 0.0])
    }
    fn dimension(&self) -> u32 {
        2
    }
}

#[tokio::test]
async fn page_vectors_are_cached_by_image_bytes_and_queries_by_text() {
    use kb_ports::{PageEmbedder, PageImage};
    let root = temp_root();
    let inner = Arc::new(CountingPages::default());
    let cached = crate::CachedPageEmbedder::new(inner.clone(), DiskCache::new(&root), "voyage|2");
    let img = |page: u32, n: usize| PageImage {
        page,
        mime: "image/jpeg".into(),
        bytes: vec![1; n],
        text: String::new(),
    };
    let a = cached.embed_pages(&[img(0, 3), img(1, 5)]).await.unwrap();
    // Same bytes on another page number (a repeated slide): a hit.
    let b = cached
        .embed_pages(&[img(7, 5), img(2, 9), img(0, 3)])
        .await
        .unwrap();
    assert_eq!(a, vec![vec![3.0, 1.0], vec![5.0, 1.0]]);
    assert_eq!(b, vec![vec![5.0, 1.0], vec![9.0, 1.0], vec![3.0, 1.0]]);
    assert_eq!(inner.pages.load(Ordering::SeqCst), 3);

    cached.embed_query("wages").await.unwrap();
    cached.embed_query("wages").await.unwrap();
    assert_eq!(inner.queries.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_new_query_prefix_keeps_the_passage_vectors() {
    let root = temp_root();
    let inner = Arc::new(CountingEmbedder::default());
    let a =
        CachedEmbedder::new(inner.clone(), DiskCache::new(&root), "m").with_prefixes("Q1: ", "");
    a.embed_passages(&texts(&["x", "y"])).await.unwrap();
    a.embed_query("q").await.unwrap();
    let b =
        CachedEmbedder::new(inner.clone(), DiskCache::new(&root), "m").with_prefixes("Q2: ", "");
    b.embed_passages(&texts(&["x", "y"])).await.unwrap();
    b.embed_query("q").await.unwrap();
    assert_eq!(inner.passages.load(Ordering::SeqCst), 2, "passages reused");
    assert_eq!(
        inner.queries.load(Ordering::SeqCst),
        2,
        "a new query prefix re-embeds queries"
    );
    // A new passage prefix does re-embed passages.
    let c =
        CachedEmbedder::new(inner.clone(), DiskCache::new(&root), "m").with_prefixes("Q2: ", "P: ");
    c.embed_passages(&texts(&["x"])).await.unwrap();
    assert_eq!(inner.passages.load(Ordering::SeqCst), 3);
    let _ = std::fs::remove_dir_all(root);
}

/// Orders candidates by text length, longest first, and counts calls.
#[derive(Default)]
struct LengthReranker {
    calls: AtomicUsize,
}

#[async_trait]
impl kb_ports::Reranker for LengthReranker {
    async fn rerank(
        &self,
        _q: &str,
        c: Vec<kb_ports::RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<kb_ports::RerankResult>, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut v: Vec<_> = c.into_iter().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.text.len()));
        Ok(v.into_iter()
            .take(final_k as usize)
            .enumerate()
            .map(|(i, x)| kb_ports::RerankResult {
                id: x.id,
                final_rank: i as u32,
                score: x.text.len() as f32,
            })
            .collect())
    }
}

fn rc(id: &str, text: &str, rank: u32) -> kb_ports::RerankCandidate {
    kb_ports::RerankCandidate {
        id: kb_ports::ChunkId(id.into()),
        text: text.into(),
        original_rank: rank,
        original_score: 0.0,
    }
}

#[tokio::test]
async fn a_repeated_rerank_is_served_from_disk_with_the_callers_ids() {
    use kb_ports::Reranker;
    let root = temp_root();
    let inner = Arc::new(LengthReranker::default());
    let r = crate::CachedReranker::new(inner.clone(), DiskCache::new(&root), "cohere|voyage");
    let first = r
        .rerank(
            "q",
            vec![rc("a", "xx", 0), rc("b", "xxxx", 1), rc("c", "x", 2)],
            2,
        )
        .await
        .unwrap();
    // Same question, same texts, different ids (a new ingest): a hit, mapped
    // onto the ids of this call.
    let second = r
        .rerank(
            "q",
            vec![rc("a2", "xx", 0), rc("b2", "xxxx", 1), rc("c2", "x", 2)],
            2,
        )
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        first.iter().map(|x| x.id.0.as_str()).collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert_eq!(
        second.iter().map(|x| x.id.0.as_str()).collect::<Vec<_>>(),
        ["b2", "a2"]
    );
    assert_eq!(second[0].score, 4.0);

    // Another query, another final_k or another order of texts: a new call.
    r.rerank("q2", vec![rc("a", "xx", 0), rc("b", "xxxx", 1)], 2)
        .await
        .unwrap();
    r.rerank(
        "q",
        vec![rc("a", "xx", 0), rc("b", "xxxx", 1), rc("c", "x", 2)],
        3,
    )
    .await
    .unwrap();
    r.rerank(
        "q",
        vec![rc("b", "xxxx", 0), rc("a", "xx", 1), rc("c", "x", 2)],
        2,
    )
    .await
    .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 4);
    let _ = std::fs::remove_dir_all(root);
}
