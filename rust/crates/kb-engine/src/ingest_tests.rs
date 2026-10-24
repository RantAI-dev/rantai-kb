//! Worker tests. Every port is a fake, so nothing here touches a network or a
//! database; the Postgres `FOR UPDATE SKIP LOCKED` claim is verified against a
//! live database in a later phase.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kb_ports::{
    BlobStore, DocumentId, DocumentMeta, Embedder, Entity, ExtractedFigure, ExtractionResult,
    Extractor, JobRecord, PageBlock, PageBlockKind, PageText, PortError, Relation, TenantId,
    document_path,
};

use super::*;
use crate::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeEntityExtractor,
    FakeEntityStore, FakeExtractor, FakeJobStore,
};
use crate::ingest::HEARTBEAT_INTERVAL;

const TENANT: &str = "tenant-1";
const DOC: &str = "doc_1";
const BLOB_KEY: &str = "documents/tenant-1/doc_1/report.pdf";

const PARAGRAPH: &str = "Section one explains the quarter in detail. It continues across \
several sentences so the chunker has real work to do. Section two follows with the breakdown by \
region and by product line, and section three states the outlook for the coming period, with \
the usual caveats about currency and about the assumptions behind the forecast.";

/// Six paragraphs is comfortably past the 800-unit chunk size, so the fixture
/// produces several chunks instead of one.
fn long_text() -> String {
    (0..6).map(|_| PARAGRAPH).collect::<Vec<_>>().join("\n\n")
}

/// An extractor over [`long_text`], which every happy-path test shares.
fn extractor() -> Arc<FakeExtractor> {
    Arc::new(FakeExtractor::returning(&long_text(), "markdown"))
}

struct Harness {
    worker: IngestWorker,
    jobs: Arc<FakeJobStore>,
    documents: Arc<FakeDocumentStore>,
    chunks: Arc<FakeChunkStore>,
    blobs: Arc<FakeBlobStore>,
}

fn tenant() -> TenantId {
    TenantId::new_unchecked(TENANT)
}

fn job() -> JobRecord {
    JobRecord {
        id: "job_1".to_string(),
        tenant_id: tenant(),
        user_id: None,
        document_id: Some(DocumentId::new_unchecked(DOC)),
        s3_key: Some(BLOB_KEY.to_string()),
        filename: "report.pdf".to_string(),
        mime_type: Some("application/pdf".to_string()),
        attempt: 1,
        params: Some(serde_json::json!({
            "title": "Quarterly report",
            "categories": ["finance"],
            "subcategory": "q3",
        })),
    }
}

fn document() -> DocumentMeta {
    DocumentMeta {
        id: DocumentId::new_unchecked(DOC),
        title: "Quarterly report".to_string(),
        categories: vec!["finance".to_string()],
        subcategory: Some("q3".to_string()),
        status: "processing".to_string(),
        chunk_count: 0,
        file_type: None,
        file_size: None,
        mime_type: Some("application/pdf".to_string()),
        s3_key: Some(BLOB_KEY.to_string()),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        knowledge_base_ids: vec![],
        metadata: None,
    }
}

fn parts() -> (
    Arc<FakeJobStore>,
    Arc<FakeDocumentStore>,
    Arc<FakeChunkStore>,
    Arc<FakeBlobStore>,
) {
    (
        Arc::new(FakeJobStore::default()),
        Arc::new(FakeDocumentStore::default()),
        Arc::new(FakeChunkStore::default()),
        Arc::new(FakeBlobStore::default()),
    )
}

fn worker_with(
    jobs: Arc<FakeJobStore>,
    documents: Arc<FakeDocumentStore>,
    chunks: Arc<FakeChunkStore>,
    blobs: Arc<FakeBlobStore>,
    extractor: Arc<dyn Extractor>,
    embedder: Arc<dyn Embedder>,
) -> IngestWorker {
    IngestWorker::new(
        jobs,
        documents,
        chunks,
        blobs,
        extractor,
        embedder,
        IngestSettings {
            embedding_model: Some("bge-m3".to_string()),
            ..IngestSettings::default()
        },
    )
}

/// A worker wired to fakes, with the fixture blob already stored.
async fn harness(extractor: Arc<FakeExtractor>) -> Harness {
    let (jobs, documents, chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"%PDF-1.7 fake".to_vec(), "application/pdf")
        .await
        .unwrap();
    let worker = worker_with(
        jobs.clone(),
        documents.clone(),
        chunks.clone(),
        blobs.clone(),
        extractor.clone(),
        Arc::new(FakeEmbedder::default()),
    );
    Harness {
        worker,
        jobs,
        documents,
        chunks,
        blobs,
    }
}

fn stored_chunks(h: &Harness) -> usize {
    h.chunks
        .stored_chunks
        .read()
        .unwrap()
        .get(&DocumentId::new_unchecked(DOC))
        .map_or(0, Vec::len)
}

fn document_status(h: &Harness) -> String {
    h.documents.docs.read().unwrap()[&tenant()][0]
        .status
        .clone()
}

#[tokio::test]
async fn happy_path_indexes_chunks_and_finishes_the_job() {
    let h = harness(extractor()).await;
    h.jobs.push(job());

    assert!(h.worker.run_once().await.unwrap());
    assert_eq!(h.jobs.pending_len(), 0);

    let finished = h.jobs.finished.read().unwrap();
    assert_eq!(finished.len(), 1);
    assert!(finished[0].success, "{:?}", finished[0]);
    assert_eq!(finished[0].detail, DOC, "success links the document");

    // The document is ready and carries the extracted file metadata.
    assert_eq!(document_status(&h), "ready");
    let docs = h.documents.docs.read().unwrap();
    let stored = &docs[&tenant()][0];
    assert_eq!(stored.file_type.as_deref(), Some("markdown"));
    assert_eq!(stored.file_size, Some(b"%PDF-1.7 fake".len() as i32));
    drop(docs);

    // Chunks are stored under the document in the TS metadata shape.
    let chunks = h.chunks.stored_chunks.read().unwrap();
    let written = chunks.get(&DocumentId::new_unchecked(DOC)).unwrap();
    assert!(written.len() >= 2, "{}", written.len());
    assert_eq!(written[0].chunk_index, 0);
    assert_eq!(written[1].chunk_index, 1);
    assert!(!written[0].embedding.is_empty());
    assert_eq!(written[0].metadata["documentTitle"], "Quarterly report");
    assert_eq!(written[0].metadata["category"], "finance");
    assert_eq!(written[0].metadata["subcategory"], "q3");
    assert_eq!(written[0].metadata["chunkIndex"], 0);
    assert!(written[0].metadata.get("chunkType").is_some());

    // The steps that ran were reported to the job row.
    let steps: Vec<String> = h
        .jobs
        .progress
        .read()
        .unwrap()
        .iter()
        .map(|(_, step, _)| step.clone())
        .collect();
    for expected in ["extracting", "chunking", "embedding", "storing"] {
        assert!(steps.contains(&expected.to_string()), "{steps:?}");
    }

    // A job this short finishes inside one heartbeat period, so nothing was
    // touched — the guard stops the task rather than leaking a write per job.
    assert!(h.jobs.touches.read().unwrap().is_empty());
}

#[tokio::test]
async fn missing_blob_fails_the_job_without_touching_the_document() {
    let h = harness(extractor()).await;
    h.jobs.push(job());
    h.blobs.delete(BLOB_KEY).await.unwrap();

    assert!(h.worker.run_once().await.unwrap());

    let finished = h.jobs.finished.read().unwrap();
    assert_eq!(finished.len(), 1);
    assert!(!finished[0].success);
    assert!(
        finished[0].detail.starts_with("blob download failed"),
        "{}",
        finished[0].detail
    );
    assert_eq!(document_status(&h), "failed");
    assert_eq!(stored_chunks(&h), 0);
}

#[tokio::test]
async fn extractor_failure_marks_the_job_failed() {
    let extractor = extractor();
    extractor
        .fail
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let h = harness(extractor.clone()).await;
    h.jobs.push(job());

    assert!(h.worker.run_once().await.unwrap());

    let finished = h.jobs.finished.read().unwrap();
    assert_eq!(finished.len(), 1, "the job was finished exactly once");
    assert!(!finished[0].success);
    assert!(
        finished[0].detail.contains("forced extract failure"),
        "{}",
        finished[0].detail
    );
    assert_eq!(document_status(&h), "failed");
    assert_eq!(stored_chunks(&h), 0);
    assert!(extractor.calls.load(std::sync::atomic::Ordering::Relaxed));
}

/// An extractor that reports a parse failure the way the local parsers do:
/// empty content plus a reason (`src/lib/ingest/extract.ts:192`).
struct UnreadableExtractor {
    reason: String,
}

#[async_trait::async_trait]
impl Extractor for UnreadableExtractor {
    async fn extract(
        &self,
        _filename: &str,
        _mime_type: &str,
        _file_bytes: &[u8],
    ) -> Result<ExtractionResult, PortError> {
        Ok(ExtractionResult::failed("markdown", self.reason.clone()))
    }
}

#[tokio::test]
async fn an_unreadable_document_fails_the_job_instead_of_indexing_empty_text() {
    let (jobs, documents, chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"x".to_vec(), "application/pdf")
        .await
        .unwrap();
    let reason = "Text extraction failed for \"report.pdf\": GLB file too small";
    let worker = worker_with(
        jobs.clone(),
        documents.clone(),
        chunks.clone(),
        blobs.clone(),
        Arc::new(UnreadableExtractor {
            reason: reason.to_string(),
        }),
        Arc::new(FakeEmbedder::default()),
    );
    jobs.push(job());

    assert!(worker.run_once().await.unwrap());

    let finished = jobs.finished.read().unwrap();
    assert_eq!(finished.len(), 1);
    assert!(!finished[0].success);
    assert_eq!(finished[0].detail, reason);
    assert_eq!(chunks.stored_chunks.read().unwrap().len(), 0);
    assert_eq!(document_status_of(&documents), "failed");
}

#[tokio::test]
async fn a_short_embedding_batch_fails_instead_of_indexing_half_a_document() {
    let (jobs, documents, chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"x".to_vec(), "application/pdf")
        .await
        .unwrap();
    let extractor = extractor();
    let worker = worker_with(
        jobs.clone(),
        documents.clone(),
        chunks.clone(),
        blobs.clone(),
        extractor,
        Arc::new(ShortEmbedder),
    );
    jobs.push(job());

    assert!(worker.run_once().await.unwrap());

    let finished = jobs.finished.read().unwrap();
    assert!(!finished[0].success, "{:?}", finished[0]);
    assert!(
        finished[0].detail.contains("embedding batch returned"),
        "{}",
        finished[0].detail
    );
    assert!(chunks.stored_chunks.read().unwrap().is_empty());
}

#[tokio::test]
async fn two_workers_never_claim_the_same_job() {
    let (jobs, documents, chunks, blobs) = parts();
    for i in 0..3 {
        let key = format!("documents/{TENANT}/doc_{i}/report.pdf");
        blobs
            .upload(&key, b"x".to_vec(), "application/pdf")
            .await
            .unwrap();
        let mut j = job();
        j.id = format!("job_{i}");
        j.document_id = Some(DocumentId::new_unchecked(format!("doc_{i}")));
        j.s3_key = Some(key);
        jobs.push(j);
    }
    let extractor = extractor();
    let make = || {
        worker_with(
            jobs.clone(),
            documents.clone(),
            chunks.clone(),
            blobs.clone(),
            extractor.clone(),
            Arc::new(FakeEmbedder::default()),
        )
    };
    let a = make();
    let b = make();

    // Interleave: a takes a job, b takes the next, and once the queue drains
    // neither can take anything, so no job can be run twice.
    let mut runs = 0;
    for _ in 0..3 {
        if a.run_once().await.unwrap() {
            runs += 1;
        }
        if b.run_once().await.unwrap() {
            runs += 1;
        }
    }
    assert_eq!(runs, 3);

    let mut ids: Vec<String> = {
        // Scoped so the read guard is dropped before the awaits below; a
        // guard held across an await would block the worker that writes it.
        let finished = jobs.finished.read().unwrap();
        finished.iter().map(|f| f.job_id.clone()).collect()
    };
    ids.sort();
    assert_eq!(ids, ["job_0", "job_1", "job_2"], "no job ran twice");
    assert_eq!(jobs.pending_len(), 0);
    assert!(!a.run_once().await.unwrap(), "queue is drained");
    assert!(!b.run_once().await.unwrap(), "queue is drained");
}

#[tokio::test]
async fn a_job_without_a_document_or_key_fails_without_touching_anything() {
    let h = harness(extractor()).await;
    let mut j = job();
    j.document_id = None;
    h.jobs.push(j);

    h.worker.run_once().await.unwrap();
    let finished = h.jobs.finished.read().unwrap();
    assert!(!finished[0].success);
    assert_eq!(finished[0].detail, "job missing documentId or s3Key");
    assert_eq!(stored_chunks(&h), 0);
}

#[tokio::test]
async fn rerunning_a_document_replaces_its_chunks() {
    let h = harness(extractor()).await;
    h.jobs.push(job());
    h.worker.run_once().await.unwrap();
    let first = stored_chunks(&h);
    assert!(first > 0);

    // A replay must not double the chunk count: ids are deterministic and the
    // previous rows are cleared first.
    let mut again = job();
    again.id = "job_2".to_string();
    again.attempt = 2;
    h.jobs.push(again);
    h.worker.run_once().await.unwrap();
    assert_eq!(first, stored_chunks(&h));
}

#[tokio::test]
async fn a_document_with_no_text_still_finishes_as_ready() {
    let h = harness(Arc::new(FakeExtractor::returning("", "text"))).await;
    let outcome = h.worker.run_job(job()).await;
    assert_eq!(outcome, JobOutcome::Ready { chunks: 0 });
    assert_eq!(document_status(&h), "ready");
    let finished = h.jobs.finished.read().unwrap();
    assert!(finished[0].success);
}

#[tokio::test]
async fn cancellation_stops_the_poll_loop() {
    let (jobs, documents, chunks, blobs) = parts();
    let extractor = extractor();
    let worker = worker_with(
        jobs.clone(),
        documents.clone(),
        chunks.clone(),
        blobs.clone(),
        extractor,
        Arc::new(FakeEmbedder::default()),
    );
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let handle = tokio::spawn(async move { worker.run(token).await });

    tokio::time::sleep(Duration::from_millis(30)).await;
    cancel.cancel();
    handle.await.unwrap();
}

/// A backlog drains without a poll interval between jobs. Before the fix the
/// loop slept `poll_interval` after every tick, so three queued jobs at a 3 s
/// poll and one claim per tick took six seconds of sleep to finish.
#[tokio::test]
async fn a_backlog_drains_without_sleeping_between_jobs() {
    let h = harness(extractor()).await;
    for i in 0..3 {
        h.jobs.push(JobRecord {
            id: format!("job_{i}"),
            ..job()
        });
    }
    assert_eq!(
        IngestSettings::default().poll_interval,
        Duration::from_secs(3)
    );
    assert_eq!(IngestSettings::default().claims_per_tick, 1);

    let cancel = CancelToken::new();
    let token = cancel.clone();
    let jobs = h.jobs.clone();
    let worker = h.worker;
    let handle = tokio::spawn(async move { worker.run(token).await });

    let drained = tokio::time::timeout(Duration::from_secs(2), async {
        while jobs.pending_len() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    cancel.cancel();
    handle.await.unwrap();
    assert!(
        drained.is_ok(),
        "3 queued jobs were still pending after 2 s"
    );
}

#[test]
fn blob_key_layout_matches_the_helper_the_api_uses() {
    assert_eq!(document_path(Some(TENANT), DOC, "report.pdf"), BLOB_KEY);
}

/// Extractor that burns `delay` of runtime time before returning, so a
/// heartbeat has time to fire while the job is still running.
struct SlowExtractor {
    delay: Duration,
    inner: Arc<FakeExtractor>,
}

#[async_trait::async_trait]
impl Extractor for SlowExtractor {
    async fn extract(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
    ) -> Result<ExtractionResult, PortError> {
        tokio::time::sleep(self.delay).await;
        self.inner.extract(filename, mime_type, file_bytes).await
    }
}

/// A step longer than `stale_after` must keep the job alive. Time is paused so
/// a five-minute job costs no wall-clock time: tokio advances the clock to each
/// pending timer as the test awaits.
#[tokio::test(start_paused = true)]
async fn heartbeat_touches_a_running_job_and_stops_when_it_finishes() {
    let (jobs, documents, chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"x".to_vec(), "application/pdf")
        .await
        .unwrap();
    // Two heartbeat periods fit inside the extraction step.
    let slow = Arc::new(SlowExtractor {
        delay: Duration::from_secs(150),
        inner: extractor(),
    });
    let worker = IngestWorker::new(
        jobs.clone(),
        documents.clone(),
        chunks.clone(),
        blobs.clone(),
        slow,
        Arc::new(FakeEmbedder::default()),
        IngestSettings {
            heartbeat_interval: HEARTBEAT_INTERVAL,
            ..IngestSettings::default()
        },
    );
    jobs.push(job());

    assert!(worker.run_once().await.unwrap());
    let touches = jobs.touches.read().unwrap().clone();
    assert_eq!(touches, ["job_1", "job_1"], "touched at 60s and 120s");

    // The guard is gone: time keeps passing, the job does not get touched again.
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert_eq!(
        jobs.touches.read().unwrap().len(),
        touches.len(),
        "heartbeat stopped when the job finished"
    );
    assert_eq!(document_status_of(&documents), "ready");
}

fn document_status_of(documents: &Arc<FakeDocumentStore>) -> String {
    documents.docs.read().unwrap()[&tenant()][0].status.clone()
}

/// Embedder that answers with one vector fewer than it was asked for, which is
/// what `generateEmbeddings` does when part of a batch fails.
struct ShortEmbedder;

#[async_trait::async_trait]
impl Embedder for ShortEmbedder {
    async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![0.0; 2])
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        Ok(texts
            .iter()
            .map(|_| vec![0.0; 2])
            .take(texts.len().saturating_sub(1))
            .collect())
    }

    fn dimension(&self) -> u32 {
        2
    }
}

/// A PDF whose content is its pages is chunked page by page: every stored
/// chunk names its page, and none holds text from two pages.
#[tokio::test]
async fn a_paged_pdf_is_chunked_page_by_page() {
    let words = ["alpha", "beta", "gamma"];
    let page_map: Vec<PageText> = words
        .iter()
        .enumerate()
        .map(|(i, w)| PageText {
            page: i as u32,
            text: format!(
                "Report header\n{}\n{}",
                i + 1,
                format!("{w} sentence. ").repeat(300)
            ),
        })
        .collect();
    let content = page_map
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let h = harness(Arc::new(FakeExtractor::returning_result(
        ExtractionResult {
            content,
            file_type: "pdf".to_string(),
            used_ocr: false,
            figures: Vec::new(),
            pages_blocks: None,
            page_map: Some(page_map),
            error: None,
        },
    )))
    .await;
    h.jobs.push(job());
    assert!(h.worker.run_once().await.unwrap());

    let stored = h.chunks.stored_chunks.read().unwrap();
    let chunks = &stored[&DocumentId::new_unchecked(DOC)];
    assert!(
        chunks.len() > words.len(),
        "each long page split into several chunks"
    );
    for c in chunks {
        let page = c.metadata["page"].as_u64().expect("every chunk has a page") as usize;
        for (i, w) in words.iter().enumerate() {
            assert_eq!(
                c.content.contains(w),
                i == page,
                "chunk {} on page {page}",
                c.chunk_index
            );
        }
    }
}

/// With `strip_page_furniture`, the running title and page numbers that open
/// every page are not in any chunk, and every chunk keeps its page.
#[tokio::test]
async fn page_furniture_is_stripped_from_chunks_but_not_from_the_document() {
    async fn run(strip: bool) -> Vec<(Option<u64>, String)> {
        let page_map: Vec<PageText> = (0..6u32)
            .map(|i| PageText {
                page: i,
                text: format!(
                    "Annual Report on Labour Mobility 2025\n{}\nBody of page {i}: {}",
                    i + 1,
                    "wages rose in the region. ".repeat(5)
                ),
            })
            .collect();
        let content = page_map
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let (jobs, documents, chunks, blobs) = parts();
        documents.insert_doc(tenant(), document());
        blobs
            .upload(BLOB_KEY, b"%PDF-1.7 fake".to_vec(), "application/pdf")
            .await
            .unwrap();
        let worker = IngestWorker::new(
            jobs.clone(),
            documents.clone(),
            chunks.clone(),
            blobs.clone(),
            Arc::new(FakeExtractor::returning_result(ExtractionResult {
                content,
                file_type: "pdf".to_string(),
                used_ocr: false,
                figures: Vec::new(),
                pages_blocks: None,
                page_map: Some(page_map),
                error: None,
            })),
            Arc::new(FakeEmbedder::default()),
            IngestSettings {
                strip_page_furniture: strip,
                ..IngestSettings::default()
            },
        );
        jobs.push(job());
        assert!(worker.run_once().await.unwrap());
        let stored = chunks.stored_chunks.read().unwrap();
        stored[&DocumentId::new_unchecked(DOC)]
            .iter()
            .map(|c| (c.metadata["page"].as_u64(), c.content.clone()))
            .collect()
    }

    let kept = run(false).await;
    assert!(
        kept.iter()
            .all(|(_, c)| c.contains("Annual Report on Labour Mobility"))
    );

    let stripped = run(true).await;
    assert_eq!(stripped.len(), 6);
    for (i, (page, text)) in stripped.iter().enumerate() {
        assert_eq!(*page, Some(i as u64));
        assert!(!text.contains("Annual Report on Labour Mobility"), "{text}");
        assert!(text.contains(&format!("Body of page {i}")));
    }
}

#[tokio::test]
async fn test_ingest_worker_end_to_end_full_pipeline() {
    let (jobs, documents, chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"pdf-bytes".to_vec(), "application/pdf")
        .await
        .unwrap();

    let page_map = vec![
        PageText {
            page: 0,
            text: "Section one explains the quarter in detail. Here is the first part.".to_string(),
        },
        PageText {
            page: 1,
            text: "Section two follows with the breakdown by region and by product line."
                .to_string(),
        },
    ];

    let pages_blocks = vec![vec![
        PageBlock {
            kind: PageBlockKind::Text,
            id: None,
            text: Some(
                "Section one explains the quarter in detail with all financial figures."
                    .to_string(),
            ),
        },
        PageBlock {
            kind: PageBlockKind::Figure,
            id: Some("fig_1".to_string()),
            text: None,
        },
    ]];

    let figures = vec![ExtractedFigure {
        kind: "chart".to_string(),
        page: 0,
        bbox: [10.0, 20.0, 100.0, 200.0],
        caption: Some("Revenue Growth Chart".to_string()),
        image_bytes: b"png-bytes".to_vec(),
        id: Some("fig_1".to_string()),
    }];

    let extract_result = ExtractionResult {
        content: "Section one explains the quarter in detail with all financial figures.

Section two follows with the breakdown by region and by product line.

Alice works at Acme Corp."
            .to_string(),
        file_type: "markdown".to_string(),
        used_ocr: false,
        figures,
        pages_blocks: Some(pages_blocks),
        page_map: Some(page_map),
        error: None,
    };

    let fake_extractor = Arc::new(FakeExtractor::returning_result(extract_result));

    let entity_store = Arc::new(FakeEntityStore::default());
    let entity_extractor = Arc::new(FakeEntityExtractor::default());
    *entity_extractor.entities.write().unwrap() = vec![
        Entity {
            name: "Alice".to_string(),
            entity_type: "PERSON".to_string(),
            confidence: 0.95,
            document_id: None,
            chunk_id: None,
            metadata: None,
        },
        Entity {
            name: "Acme Corp".to_string(),
            entity_type: "ORGANIZATION".to_string(),
            confidence: 0.98,
            document_id: None,
            chunk_id: None,
            metadata: None,
        },
    ];
    *entity_extractor.relations.write().unwrap() = vec![Relation {
        relation_type: "WORKS_AT".to_string(),
        confidence: 0.9,
        source_entity: "Alice".to_string(),
        target_entity: "Acme Corp".to_string(),
        context: Some("Alice works at Acme Corp.".to_string()),
        metadata: Some(serde_json::json!({
            "source_entity": "Alice",
            "target_entity": "Acme Corp",
            "context": "Alice works at Acme Corp."
        })),
    }];

    let worker = IngestWorker::new(
        jobs.clone(),
        documents.clone(),
        chunks.clone(),
        blobs.clone(),
        fake_extractor,
        Arc::new(FakeEmbedder::default()),
        IngestSettings {
            embedding_model: Some("bge-m3".to_string()),
            ..IngestSettings::default()
        },
    )
    .with_entity_store(entity_store.clone())
    .with_entity_extractor(entity_extractor.clone());

    let mut j = job();
    j.params = Some(serde_json::json!({
        "title": "Quarterly report",
        "categories": ["finance"],
        "subcategory": "q3",
        "useCombined": true,
    }));
    jobs.push(j);

    let ran = worker.run_once().await.unwrap();
    assert!(ran);

    // 1. Check document ready
    assert_eq!(document_status_of(&documents), "ready");

    // 2. Check stored chunks: text chunks + figure chunk
    let stored = chunks.stored_chunks.read().unwrap();
    let doc_chunks = stored.get(&DocumentId::new_unchecked(DOC)).unwrap();
    assert!(doc_chunks.len() >= 2);

    let fig_chunk = doc_chunks
        .iter()
        .find(|c| c.metadata.get("chunkType").and_then(|v| v.as_str()) == Some("figure"))
        .expect("figure chunk should be stored");
    assert_eq!(
        fig_chunk.metadata.get("assetKey").and_then(|v| v.as_str()),
        Some("documents/tenant-1/doc_1/assets/fig-p0-1.png")
    );
    assert_eq!(
        fig_chunk
            .metadata
            .get("anchorChunkIndex")
            .and_then(|v| v.as_u64()),
        Some(0)
    );

    // Verify chunkIndex monotonically reindexed across set
    for (i, c) in doc_chunks.iter().enumerate() {
        assert_eq!(c.chunk_index, i as u32);
        assert_eq!(
            c.metadata.get("chunkIndex").and_then(|v| v.as_u64()),
            Some(i as u64)
        );
    }

    // 3. Check document metadata figures updated
    let doc_meta = documents.docs.read().unwrap()[&tenant()][0]
        .metadata
        .clone()
        .unwrap();
    let figs = doc_meta.get("figures").and_then(|v| v.as_array()).unwrap();
    assert_eq!(figs.len(), 1);
    assert_eq!(
        figs[0]["assetKey"],
        "documents/tenant-1/doc_1/assets/fig-p0-1.png"
    );
    assert_eq!(figs[0]["type"], "chart");

    // 4. Check entity upserts and relations
    let upserts = entity_store.upserted_entities.read().unwrap();
    assert_eq!(upserts.len(), 2);
    assert!(
        upserts
            .iter()
            .any(|(_, san, name, typ, ..)| san == "alice" && name == "Alice" && typ == "PERSON")
    );
    assert!(
        upserts
            .iter()
            .any(|(_, san, name, typ, ..)| san == "acme_corp"
                && name == "Acme Corp"
                && typ == "ORGANIZATION")
    );

    let rels = entity_store.relations.read().unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].0, "entity:doc_1_alice");
    assert_eq!(rels[0].1, "WORKS_AT");
    assert_eq!(rels[0].2, "entity:doc_1_acme_corp");

    // 5. Check progress step sequence
    let progress = jobs.progress.read().unwrap();
    let steps: Vec<&str> = progress.iter().map(|(_, step, _)| step.as_str()).collect();
    assert!(steps.contains(&"extracting"));
    assert!(steps.contains(&"chunking"));
    assert!(steps.contains(&"extracting_entities"));
    assert!(steps.contains(&"processing_figures"));
    assert!(steps.contains(&"embedding"));
    assert!(steps.contains(&"storing"));
    assert!(steps.contains(&"done"));

    // Check job finished success
    let finished = jobs.finished.read().unwrap();
    assert_eq!(finished.len(), 1);
    assert!(finished[0].success);
}

#[tokio::test]
async fn test_ingest_worker_entity_extractor_failure_is_non_fatal() {
    let (jobs, documents, chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"pdf-bytes".to_vec(), "application/pdf")
        .await
        .unwrap();

    let fake_extractor = Arc::new(FakeExtractor::returning("Some text here", "text"));
    let entity_extractor = Arc::new(FakeEntityExtractor::default());
    entity_extractor
        .fail
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let worker = IngestWorker::new(
        jobs.clone(),
        documents.clone(),
        chunks.clone(),
        blobs.clone(),
        fake_extractor,
        Arc::new(FakeEmbedder::default()),
        IngestSettings::default(),
    )
    .with_entity_extractor(entity_extractor);

    jobs.push(job());
    let outcome = worker.run_job(job()).await;
    assert!(matches!(outcome, JobOutcome::Ready { .. }));
    assert_eq!(document_status_of(&documents), "ready");
}

#[tokio::test]
async fn test_ingest_worker_embed_failure_marks_job_and_document_failed() {
    let (jobs, documents, chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"pdf-bytes".to_vec(), "application/pdf")
        .await
        .unwrap();

    let fake_extractor = Arc::new(FakeExtractor::returning("Some text here", "text"));
    let embedder = Arc::new(FakeEmbedder::default());
    embedder
        .fail
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let worker = IngestWorker::new(
        jobs.clone(),
        documents.clone(),
        chunks.clone(),
        blobs.clone(),
        fake_extractor,
        embedder,
        IngestSettings::default(),
    );

    jobs.push(job());
    let outcome = worker.run_job(job()).await;
    assert!(matches!(outcome, JobOutcome::Failed { .. }));
    assert_eq!(document_status_of(&documents), "failed");
    let finished = jobs.finished.read().unwrap();
    assert!(!finished[0].success);
    assert!(finished[0].detail.contains("embedding failed"));
}

// ── the graph store dual-write ──────────────────────────────────────────────────
//
// Ingest writes the same extraction to SurrealDB and to the graph store. The
// SurrealDB half is unchanged and is covered by the tests above; what is new is
// that the graph half happens at all, that a re-ingest replaces rather than
// accumulates, and that a broken graph store costs the document nothing.

mod graph_write {
    use super::*;

    use kb_ports::{
        ChunkId, DocumentScope, EntityExtractor, EntityMentions, EntityStore, EntityUpsert,
        GraphStore, RelationUpsert, TenantGraph,
    };

    use crate::fakes::{FakeDocumentStore, FakeEntityExtractor, FakeEntityStore, make_fake_scope};
    use crate::in_memory_graph_store::InMemoryGraphStore;

    /// Text with two real names in it, so the graph has an edge and two mentions.
    const GRAPH_TEXT: &str = "Alice works at Acme Corp on the Jakarta office. \
Acme Corp also sponsors the Bandung workshop run by Alice.";

    fn extractor() -> Arc<FakeExtractor> {
        Arc::new(FakeExtractor::returning(GRAPH_TEXT, "markdown"))
    }

    fn entity_extractor() -> Arc<FakeEntityExtractor> {
        let entity_extractor = Arc::new(FakeEntityExtractor::default());
        *entity_extractor.entities.write().unwrap() = vec![
            Entity {
                name: "Alice".to_string(),
                entity_type: "PERSON".to_string(),
                confidence: 0.95,
                document_id: None,
                chunk_id: None,
                metadata: None,
            },
            Entity {
                name: "Acme Corp".to_string(),
                entity_type: "ORGANIZATION".to_string(),
                confidence: 0.98,
                document_id: None,
                chunk_id: None,
                metadata: None,
            },
        ];
        *entity_extractor.relations.write().unwrap() = vec![Relation {
            relation_type: "WORKS_AT".to_string(),
            confidence: 0.9,
            source_entity: "Alice".to_string(),
            target_entity: "Acme Corp".to_string(),
            context: None,
            metadata: Some(serde_json::json!({
                "source_entity": "Alice",
                "target_entity": "Acme Corp",
            })),
        }];
        entity_extractor
    }

    struct Fixture {
        jobs: Arc<FakeJobStore>,
        documents: Arc<FakeDocumentStore>,
        chunks: Arc<FakeChunkStore>,
        blobs: Arc<FakeBlobStore>,
        graph: Arc<InMemoryGraphStore>,
        entities: Arc<FakeEntityStore>,
        extractor: Arc<FakeEntityExtractor>,
    }

    async fn fixture() -> Fixture {
        let (jobs, documents, chunks, blobs) = parts();
        documents.insert_doc(tenant(), document());
        blobs
            .upload(BLOB_KEY, b"%PDF-1.7 fake".to_vec(), "application/pdf")
            .await
            .unwrap();
        Fixture {
            jobs,
            documents,
            chunks,
            blobs,
            graph: Arc::new(InMemoryGraphStore::new()),
            entities: Arc::new(FakeEntityStore::default()),
            extractor: entity_extractor(),
        }
    }

    fn worker(f: &Fixture, graph: Arc<dyn GraphStore>) -> IngestWorker {
        IngestWorker::new(
            f.jobs.clone(),
            f.documents.clone(),
            f.chunks.clone(),
            f.blobs.clone(),
            extractor(),
            Arc::new(FakeEmbedder::default()),
            IngestSettings::default(),
        )
        .with_entity_store(Arc::clone(&f.entities) as Arc<dyn EntityStore>)
        .with_entity_extractor(Arc::clone(&f.extractor) as Arc<dyn EntityExtractor>)
        .with_graph_store(graph)
    }

    fn scope() -> DocumentScope {
        make_fake_scope(tenant(), vec![DocumentId::new_unchecked(DOC)])
    }

    #[tokio::test]
    async fn the_same_extraction_reaches_both_the_surreal_rows_and_the_graph() {
        let f = fixture().await;
        let graph: Arc<dyn GraphStore> = f.graph.clone();
        f.jobs.push(job());

        assert!(worker(&f, Arc::clone(&graph)).run_once().await.unwrap());

        // SurrealDB, unchanged: the TS ids and the lowercase sanitised names.
        let upserts = f.entities.upserted_entities.read().unwrap();
        assert_eq!(upserts.len(), 2);
        assert!(
            upserts
                .iter()
                .any(|(_, sanitised, _, _, _, _)| sanitised == "acme_corp")
        );
        assert_eq!(f.entities.relations.read().unwrap().len(), 1);

        // The graph, keyed the other way: one tenant-wide key per entity, with the
        // chunks that mention it.
        assert_eq!(
            f.graph.entity_keys(&tenant()),
            vec!["acme corp".to_string(), "alice".to_string()]
        );
        let relations = f.graph.relations(&tenant());
        assert_eq!(relations.len(), 1, "got {relations:?}");
        assert_eq!(relations[0].0, "alice");
        assert_eq!(relations[0].2, "WORKS_AT");
        assert_eq!(relations[0].3, DocumentId::new_unchecked(DOC));
    }

    #[tokio::test]
    async fn an_entity_is_attributed_to_the_chunks_that_mention_it() {
        let f = fixture().await;
        let graph: Arc<dyn GraphStore> = f.graph.clone();
        f.jobs.push(job());
        assert!(worker(&f, Arc::clone(&graph)).run_once().await.unwrap());

        let scoped = f.graph.load_tenant_graph(&scope()).await.unwrap();
        let mentions: Vec<&EntityMentions> = scoped.mentions().iter().collect();
        assert_eq!(mentions.len(), 2);
        for entity in &mentions {
            assert!(
                !entity.chunk_ids.is_empty(),
                "{} was extracted but never attributed to a chunk: it would be a node the walk could reach but never return",
                entity.key,
            );
            assert!(
                entity
                    .chunk_ids
                    .iter()
                    .all(|id| id.0.starts_with(&format!("{DOC}_"))),
                "{:?} belongs to another document",
                entity.chunk_ids,
            );
        }
    }

    #[tokio::test]
    async fn a_re_ingest_replaces_the_documents_graph_rather_than_adding_to_it() {
        let f = fixture().await;
        let graph: Arc<dyn GraphStore> = f.graph.clone();
        f.jobs.push(job());
        assert!(worker(&f, Arc::clone(&graph)).run_once().await.unwrap());
        assert_eq!(f.graph.relations(&tenant()).len(), 1);

        // The same document, re-ingested with an entity the first version did not
        // have. Without the delete-first, the old relation would still be here and
        // the graph would assert something about a version of the document that no
        // longer exists.
        *f.extractor.relations.write().unwrap() = Vec::new();
        *f.extractor.entities.write().unwrap() = vec![Entity {
            name: "Acme Corp".to_string(),
            entity_type: "ORGANIZATION".to_string(),
            confidence: 0.98,
            document_id: None,
            chunk_id: None,
            metadata: None,
        }];
        f.jobs.push(job());
        assert!(worker(&f, Arc::clone(&graph)).run_once().await.unwrap());

        assert_eq!(
            f.graph.entity_keys(&tenant()),
            vec!["acme corp".to_string()],
            "the re-ingest must replace, not accumulate"
        );
        assert!(
            f.graph.relations(&tenant()).is_empty(),
            "the old edge survived a re-ingest: {:?}",
            f.graph.relations(&tenant())
        );
    }

    /// A store that fails every call. The document must still become searchable.
    struct BrokenGraph;

    #[async_trait::async_trait]
    impl GraphStore for BrokenGraph {
        async fn upsert_entities(
            &self,
            _: &kb_ports::TenantId,
            _: &DocumentId,
            _: Vec<EntityUpsert>,
        ) -> Result<(), PortError> {
            Err(PortError::Unavailable("graph store is down".into()))
        }
        async fn relate(
            &self,
            _: &kb_ports::TenantId,
            _: &DocumentId,
            _: Vec<RelationUpsert>,
        ) -> Result<(), PortError> {
            Err(PortError::Unavailable("graph store is down".into()))
        }
        async fn delete_document(
            &self,
            _: &kb_ports::TenantId,
            _: &DocumentId,
        ) -> Result<(), PortError> {
            Err(PortError::Unavailable("graph store is down".into()))
        }
        async fn load_tenant_graph(&self, _: &DocumentScope) -> Result<TenantGraph, PortError> {
            Err(PortError::Unavailable("graph store is down".into()))
        }
        async fn graph_version(&self, _: &kb_ports::TenantId) -> Result<u64, PortError> {
            Err(PortError::Unavailable("graph store is down".into()))
        }
        async fn match_entities(
            &self,
            _: &DocumentScope,
            _: &[String],
        ) -> Result<Vec<(String, f32)>, PortError> {
            Err(PortError::Unavailable("graph store is down".into()))
        }
        async fn entity_mentions(
            &self,
            _: &DocumentScope,
            _: &str,
        ) -> Result<Vec<ChunkId>, PortError> {
            Err(PortError::Unavailable("graph store is down".into()))
        }
    }

    #[tokio::test]
    async fn a_graph_store_failure_is_non_fatal_and_the_document_still_becomes_searchable() {
        let f = fixture().await;
        f.jobs.push(job());

        let outcome = worker(&f, Arc::new(BrokenGraph)).run_job(job()).await;

        assert!(
            matches!(outcome, JobOutcome::Ready { .. }),
            "a broken graph must not cost the document its index: {outcome:?}"
        );
        assert_eq!(document_status_of(&f.documents), "ready");
        let finished = f.jobs.finished.read().unwrap();
        assert_eq!(finished.len(), 1);
        assert!(finished[0].success, "{:?}", finished[0]);

        // Chunks went in, and the SurrealDB entities went in too: the failure is
        // confined to the graph.
        let stored = f.chunks.stored_chunks.read().unwrap();
        assert!(
            !stored
                .get(&DocumentId::new_unchecked(DOC))
                .is_none_or(Vec::is_empty),
            "the chunks never landed: the graph failure took the index with it"
        );
        drop(stored);
        assert_eq!(f.entities.upserted_entities.read().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn no_graph_store_configured_is_not_an_error_and_writes_nothing() {
        let f = fixture().await;
        f.jobs.push(job());
        let worker = IngestWorker::new(
            f.jobs.clone(),
            f.documents.clone(),
            f.chunks.clone(),
            f.blobs.clone(),
            extractor(),
            Arc::new(FakeEmbedder::default()),
            IngestSettings::default(),
        )
        .with_entity_store(Arc::clone(&f.entities) as Arc<dyn EntityStore>)
        .with_entity_extractor(Arc::clone(&f.extractor) as Arc<dyn EntityExtractor>);

        assert!(worker.run_once().await.unwrap());
        assert!(f.graph.entity_keys(&tenant()).is_empty());
        assert_eq!(f.entities.upserted_entities.read().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_relation_to_an_entity_that_was_not_extracted_is_dropped_not_stored() {
        let f = fixture().await;
        *f.extractor.entities.write().unwrap() = vec![Entity {
            name: "Alice".to_string(),
            entity_type: "PERSON".to_string(),
            confidence: 0.95,
            document_id: None,
            chunk_id: None,
            metadata: None,
        }];
        // The relation names an endpoint no entity upsert introduced. `relate`
        // rejects a whole batch containing an unknown endpoint, so keeping it
        // would cost the batch.
        *f.extractor.relations.write().unwrap() = vec![Relation {
            relation_type: "WORKS_AT".to_string(),
            confidence: 0.9,
            source_entity: "Alice".to_string(),
            target_entity: "Ghost Ltd".to_string(),
            context: None,
            metadata: Some(serde_json::json!({
                "source_entity": "Alice",
                "target_entity": "Ghost Ltd",
            })),
        }];
        let graph: Arc<dyn GraphStore> = f.graph.clone();
        f.jobs.push(job());
        assert!(worker(&f, Arc::clone(&graph)).run_once().await.unwrap());

        assert_eq!(f.graph.relations(&tenant()).len(), 0);
        assert_eq!(f.graph.entity_keys(&tenant()), vec!["alice".to_string()]);
    }

    #[tokio::test]
    async fn a_zero_confidence_relation_is_dropped_because_a_zero_weight_is_not_an_edge() {
        let f = fixture().await;
        f.extractor.relations.write().unwrap()[0].confidence = 0.0;
        let graph: Arc<dyn GraphStore> = f.graph.clone();
        f.jobs.push(job());
        assert!(worker(&f, Arc::clone(&graph)).run_once().await.unwrap());
        assert!(
            f.graph.relations(&tenant()).is_empty(),
            "a zero weight is an absent edge, not a stored one: {:?}",
            f.graph.relations(&tenant())
        );
    }

    #[tokio::test]
    async fn the_entity_keys_are_normalised_the_same_way_the_extractor_normalises_them() {
        let f = fixture().await;
        *f.extractor.entities.write().unwrap() = vec![
            Entity {
                name: "Bank Indonesia".to_string(),
                entity_type: "ORGANIZATION".to_string(),
                confidence: 0.9,
                document_id: None,
                chunk_id: None,
                metadata: None,
            },
            Entity {
                name: "BANK  indonesia".to_string(),
                entity_type: "ORGANIZATION".to_string(),
                confidence: 0.9,
                document_id: None,
                chunk_id: None,
                metadata: None,
            },
        ];
        let graph: Arc<dyn GraphStore> = f.graph.clone();
        f.jobs.push(job());
        assert!(worker(&f, Arc::clone(&graph)).run_once().await.unwrap());

        assert_eq!(
            f.graph.entity_keys(&tenant()),
            vec!["bank indonesia".to_string()],
            "two surface forms of one name must be one node"
        );
    }

    #[tokio::test]
    async fn deleting_a_document_cascades_into_the_graph() {
        let f = fixture().await;
        let graph: Arc<dyn GraphStore> = f.graph.clone();
        f.jobs.push(job());
        assert!(worker(&f, Arc::clone(&graph)).run_once().await.unwrap());
        assert!(!f.graph.entity_keys(&tenant()).is_empty());

        // What `DELETE /v1/documents/:id?hard=true` does to the graph: an entity
        // left behind would keep its relations alive as paths through a document
        // nobody can see any more.
        graph
            .delete_document(&tenant(), &DocumentId::new_unchecked(DOC))
            .await
            .unwrap();

        assert!(f.graph.entity_keys(&tenant()).is_empty());
        assert!(f.graph.relations(&tenant()).is_empty());
        let scoped = f.graph.load_tenant_graph(&scope()).await.unwrap();
        assert_eq!(scoped.entity_count(), 0);
        assert_eq!(scoped.arc_count(), 0);
    }
}

// ---- page arm inside the ingest job ----

/// Renders `.0` blank pages for any PDF.
struct BlankPages(u32);

#[async_trait::async_trait]
impl kb_ports::PageRenderer for BlankPages {
    async fn render(
        &self,
        mime: &str,
        _: &[u8],
        max: u32,
    ) -> Result<Vec<kb_ports::PageImage>, PortError> {
        if mime != "application/pdf" {
            return Ok(Vec::new());
        }
        Ok((0..self.0.min(max))
            .map(|page| kb_ports::PageImage {
                page,
                mime: "image/jpeg".into(),
                bytes: vec![1],
                text: String::new(),
            })
            .collect())
    }
}

/// Page vectors of width 3; fails any request holding page `fail_on`.
struct Pages3 {
    fail_on: Option<u32>,
}

#[async_trait::async_trait]
impl kb_ports::PageEmbedder for Pages3 {
    async fn embed_pages(&self, pages: &[kb_ports::PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        if pages.iter().any(|p| Some(p.page) == self.fail_on) {
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

fn page_ingest(
    pages: u32,
    fail_on: Option<u32>,
) -> (
    Arc<crate::PageIngest>,
    Arc<crate::in_memory_index::InMemoryIndex>,
) {
    let index = Arc::new(crate::in_memory_index::InMemoryIndex::new(3));
    let ingest = Arc::new(crate::PageIngest {
        renderer: Arc::new(BlankPages(pages)),
        embedder: Arc::new(Pages3 { fail_on }),
        blobs: None,
        index: index.clone(),
        max_pages: 1000,
        text_chars: 0,
        caption: None,
    });
    (ingest, index)
}

async fn stored_pages(index: &crate::in_memory_index::InMemoryIndex) -> usize {
    index
        .ordered_chunks(&tenant(), &DocumentId::new_unchecked(DOC))
        .await
        .unwrap()
        .len()
}

fn page_images_meta(documents: &Arc<FakeDocumentStore>) -> Option<serde_json::Value> {
    documents.docs.read().unwrap()[&tenant()][0]
        .metadata
        .as_ref()
        .and_then(|m| m.get("pageImages").cloned())
}

#[tokio::test]
async fn page_images_are_indexed_and_recorded_on_the_document() {
    let mut h = harness(extractor()).await;
    let (pages, index) = page_ingest(5, None);
    h.worker = h.worker.with_page_ingest(pages);
    assert!(matches!(
        h.worker.run_job(job()).await,
        JobOutcome::Ready { .. }
    ));
    assert_eq!(stored_pages(&index).await, 5);
    assert_eq!(
        page_images_meta(&h.documents),
        Some(serde_json::json!({"indexed": 5, "failed": 0, "error": null}))
    );
}

/// A failed page window is visible on the document, and the document is still
/// ready: text search does not depend on page images.
#[tokio::test]
async fn partly_failed_page_images_are_visible_on_a_ready_document() {
    let mut h = harness(extractor()).await;
    let (pages, index) = page_ingest(100, Some(80));
    h.worker = h.worker.with_page_ingest(pages);
    assert!(matches!(
        h.worker.run_job(job()).await,
        JobOutcome::Ready { .. }
    ));
    assert_eq!(document_status(&h), "ready");
    assert_eq!(stored_pages(&index).await, 64);
    let meta = page_images_meta(&h.documents).unwrap();
    assert_eq!(
        (meta["indexed"].as_u64(), meta["failed"].as_u64()),
        (Some(64), Some(36))
    );
    assert!(meta["error"].as_str().unwrap().contains("provider down"));
}

/// A PDF with no text layer (a scan) still gets its page images: that is the
/// document the page arm helps most.
#[tokio::test]
async fn a_pdf_with_no_text_still_gets_page_images() {
    let mut h = harness(Arc::new(FakeExtractor::returning("", "text"))).await;
    let (pages, index) = page_ingest(3, None);
    h.worker = h.worker.with_page_ingest(pages);
    assert_eq!(
        h.worker.run_job(job()).await,
        JobOutcome::Ready { chunks: 0 }
    );
    assert_eq!(stored_pages(&index).await, 3);
}

/// Pages are indexed alongside text; when the text fails, the document is
/// failed and its pages must not stay searchable.
#[tokio::test]
async fn a_failed_text_step_removes_the_pages_indexed_alongside_it() {
    let (jobs, documents, chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"x".to_vec(), "application/pdf")
        .await
        .unwrap();
    let (pages, index) = page_ingest(5, None);
    let worker = worker_with(
        jobs.clone(),
        documents.clone(),
        chunks.clone(),
        blobs.clone(),
        extractor(),
        Arc::new(ShortEmbedder),
    )
    .with_page_ingest(pages);
    jobs.push(job());
    assert!(worker.run_once().await.unwrap());
    assert!(!jobs.finished.read().unwrap()[0].success);
    assert_eq!(document_status_of(&documents), "failed");
    assert_eq!(stored_pages(&index).await, 0);
}

// ---- ingest-time page captions in the text index ----

/// Describes each page as "chart on page N", or fails on `fail_on`.
struct DescribePages {
    calls: AtomicUsize,
    fail_on: Option<u32>,
}

#[async_trait::async_trait]
impl kb_ports::PageDescriber for DescribePages {
    async fn describe(&self, page: &kb_ports::PageImage) -> Result<Option<String>, PortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if Some(page.page) == self.fail_on {
            return Err(PortError::Unavailable("describer down".into()));
        }
        Ok(Some(format!("chart on page {}", page.page + 1)))
    }
}

/// Passages succeed on the first call (the document's text) and fail after
/// that (the visual descriptions).
struct FailSecondEmbedder {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Embedder for FailSecondEmbedder {
    async fn embed_query(&self, _: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }
    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) >= 1 {
            return Err(PortError::Unavailable("description embedding down".into()));
        }
        Ok(texts.iter().map(|_| vec![0.1, 0.2, 0.3, 0.4]).collect())
    }
    fn dimension(&self) -> u32 {
        4
    }
}

fn page_ingest_with_caption(
    pages: u32,
    describer: Arc<dyn kb_ports::PageDescriber>,
) -> (
    Arc<crate::PageIngest>,
    Arc<crate::in_memory_index::InMemoryIndex>,
) {
    let index = Arc::new(crate::in_memory_index::InMemoryIndex::new(3));
    let ingest = Arc::new(crate::PageIngest {
        renderer: Arc::new(BlankPages(pages)),
        embedder: Arc::new(Pages3 { fail_on: None }),
        blobs: None,
        index: index.clone(),
        max_pages: 1000,
        text_chars: 0,
        caption: Some(crate::PageCaption {
            describer,
            config: crate::PageCaptionConfig::default(),
        }),
    });
    (ingest, index)
}

fn worker_with_text_index(
    jobs: Arc<FakeJobStore>,
    documents: Arc<FakeDocumentStore>,
    index: Arc<dyn kb_ports::RetrievalIndex>,
    blobs: Arc<FakeBlobStore>,
    extractor: Arc<dyn Extractor>,
    embedder: Arc<dyn Embedder>,
) -> IngestWorker {
    IngestWorker::new(
        jobs,
        documents,
        index,
        blobs,
        extractor,
        embedder,
        IngestSettings {
            embedding_model: Some("bge-m3".to_string()),
            ..IngestSettings::default()
        },
    )
}

#[tokio::test]
async fn page_descriptions_are_indexed_as_figure_chunks_after_the_text_chunks() {
    let (jobs, documents, _chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"%PDF-1.7 fake".to_vec(), "application/pdf")
        .await
        .unwrap();
    let index = Arc::new(crate::in_memory_index::InMemoryIndex::new(4));
    let describer = Arc::new(DescribePages {
        calls: AtomicUsize::new(0),
        fail_on: Some(0),
    });
    let (pages, _page_index) = page_ingest_with_caption(3, describer.clone());
    let worker = worker_with_text_index(
        jobs,
        documents.clone(),
        index.clone(),
        blobs,
        extractor(),
        Arc::new(FakeEmbedder::default()),
    )
    .with_page_ingest(pages);

    let outcome = worker.run_job(job()).await;
    assert!(
        matches!(outcome, JobOutcome::Ready { .. }),
        "document failed: {outcome:?}"
    );
    let written = match outcome {
        JobOutcome::Ready { chunks } => chunks,
        JobOutcome::Failed { .. } => 0,
    };
    assert!(written > 0);

    let stored = index
        .ordered_chunks(&tenant(), &DocumentId::new_unchecked(DOC))
        .await
        .unwrap();
    let figures: Vec<_> = stored
        .iter()
        .filter(|c| c.chunk_type == kb_ports::ChunkType::Figure)
        .collect();
    assert_eq!(figures.len(), 2, "one chunk per described page");
    assert_eq!(figures[0].unit_id.0, format!("{DOC}_{written}"));
    assert_eq!(figures[1].unit_id.0, format!("{DOC}_{}", written + 1));
    assert_eq!(figures[0].page, Some(1));
    assert_eq!(figures[1].page, Some(2));
    assert_eq!(figures[0].content, "[Visual] chart on page 2");
    assert_eq!(figures[1].content, "[Visual] chart on page 3");
    assert_eq!(figures[0].metadata["section"], "Visual description");
    assert_eq!(figures[0].metadata["chunkType"], "figure");
    assert_eq!(figures[0].tenant, tenant());
    assert_eq!(figures[1].tenant, tenant());

    // The text chunks keep their own ids; the description ids follow them.
    for (i, chunk) in stored
        .iter()
        .filter(|c| c.chunk_type != kb_ports::ChunkType::Figure)
        .enumerate()
    {
        assert_eq!(chunk.unit_id.0, format!("{DOC}_{i}"));
    }

    let meta = page_images_meta(&documents).unwrap();
    assert_eq!(meta["described"], 2);
    assert_eq!(meta["descriptionsFailed"], 1);
    assert_eq!(describer.calls.load(Ordering::SeqCst), 3);
    let visual = documents.docs.read().unwrap()[&tenant()][0]
        .metadata
        .as_ref()
        .unwrap()["visualChunks"]
        .clone();
    assert_eq!(visual["written"], 2);
    assert_eq!(visual["failed"], 0);
}

#[tokio::test]
async fn a_failed_description_embedding_does_not_fail_the_document() {
    let (jobs, documents, _chunks, blobs) = parts();
    documents.insert_doc(tenant(), document());
    blobs
        .upload(BLOB_KEY, b"%PDF-1.7 fake".to_vec(), "application/pdf")
        .await
        .unwrap();
    let index = Arc::new(crate::in_memory_index::InMemoryIndex::new(4));
    let describer = Arc::new(DescribePages {
        calls: AtomicUsize::new(0),
        fail_on: None,
    });
    let (pages, _page_index) = page_ingest_with_caption(2, describer);
    let worker = worker_with_text_index(
        jobs,
        documents.clone(),
        index.clone(),
        blobs,
        extractor(),
        Arc::new(FailSecondEmbedder {
            calls: AtomicUsize::new(0),
        }),
    )
    .with_page_ingest(pages);

    let outcome = worker.run_job(job()).await;
    assert!(
        matches!(outcome, JobOutcome::Ready { .. }),
        "document failed: {outcome:?}"
    );
    let written = match outcome {
        JobOutcome::Ready { chunks } => chunks,
        JobOutcome::Failed { .. } => 0,
    };
    let stored = index
        .ordered_chunks(&tenant(), &DocumentId::new_unchecked(DOC))
        .await
        .unwrap();
    assert_eq!(stored.len(), written, "only the text chunks are indexed");
    assert!(
        stored
            .iter()
            .all(|c| c.chunk_type != kb_ports::ChunkType::Figure),
        "no description chunk survives a failed embedding"
    );
    assert_eq!(document_status_of(&documents), "ready");

    // The descriptions were generated, and their write failed visibly.
    let meta = page_images_meta(&documents).unwrap();
    assert_eq!(meta["described"], 2);
    assert_eq!(meta["descriptionsFailed"], 0);
    let visual = documents.docs.read().unwrap()[&tenant()][0]
        .metadata
        .as_ref()
        .unwrap()["visualChunks"]
        .clone();
    assert_eq!(visual["written"], 0);
    assert_eq!(visual["failed"], 2);
}
