//! Worker tests. Every port is a fake, so nothing here touches a network or a
//! database; the Postgres `FOR UPDATE SKIP LOCKED` claim is verified against a
//! live database in a later phase.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
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
    extractor: Arc<FakeExtractor>,
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
        extractor,
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

    let finished = jobs.finished.read().unwrap();
    let mut ids: Vec<String> = finished.iter().map(|f| f.job_id.clone()).collect();
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
            text: "Section two follows with the breakdown by region and by product line.".to_string(),
        },
    ];

    let pages_blocks = vec![vec![
        PageBlock {
            kind: PageBlockKind::Text,
            id: None,
            text: Some("Section one explains the quarter in detail with all financial figures.".to_string()),
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

Alice works at Acme Corp.".to_string(),
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

    let fig_chunk = doc_chunks.iter().find(|c| {
        c.metadata.get("chunkType").and_then(|v| v.as_str()) == Some("figure")
    }).expect("figure chunk should be stored");
    assert_eq!(
        fig_chunk.metadata.get("assetKey").and_then(|v| v.as_str()),
        Some("documents/tenant-1/doc_1/assets/fig-p0-1.png")
    );
    assert_eq!(
        fig_chunk.metadata.get("anchorChunkIndex").and_then(|v| v.as_u64()),
        Some(0)
    );

    // Verify chunkIndex monotonically reindexed across set
    for (i, c) in doc_chunks.iter().enumerate() {
        assert_eq!(c.chunk_index, i as u32);
        assert_eq!(c.metadata.get("chunkIndex").and_then(|v| v.as_u64()), Some(i as u64));
    }

    // 3. Check document metadata figures updated
    let doc_meta = documents.docs.read().unwrap()[&tenant()][0].metadata.clone().unwrap();
    let figs = doc_meta.get("figures").and_then(|v| v.as_array()).unwrap();
    assert_eq!(figs.len(), 1);
    assert_eq!(figs[0]["assetKey"], "documents/tenant-1/doc_1/assets/fig-p0-1.png");
    assert_eq!(figs[0]["type"], "chart");

    // 4. Check entity upserts and relations
    let upserts = entity_store.upserted_entities.read().unwrap();
    assert_eq!(upserts.len(), 2);
    assert!(upserts.iter().any(|(_, san, name, typ, ..)| san == "alice" && name == "Alice" && typ == "PERSON"));
    assert!(upserts.iter().any(|(_, san, name, typ, ..)| san == "acme_corp" && name == "Acme Corp" && typ == "ORGANIZATION"));

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
    entity_extractor.fail.store(true, std::sync::atomic::Ordering::Relaxed);

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
    embedder.fail.store(true, std::sync::atomic::Ordering::Relaxed);

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
