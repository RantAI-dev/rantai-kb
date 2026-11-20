//! The concurrent worker (`ING-6`): several documents at once, the seats that
//! bound each stage, and the head-of-line behaviour the serial worker had.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kb_ports::{
    BlobStore, DocumentId, DocumentMeta, Embedder, ExtractionOptions, ExtractionResult, Extractor,
    JobRecord, PortError, TenantId,
};

use crate::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeExtractor, FakeJobStore,
};
use crate::ingest::{CancelToken, IngestSettings, IngestWorker};
use crate::seats::SeatCapacity;

const TENANT: &str = "tenant-1";

/// Long enough that several jobs are in the stage at the same time, short
/// enough that a test does not drag.
const STAGE_DELAY: Duration = Duration::from_millis(60);

fn tenant() -> TenantId {
    TenantId::new_unchecked(TENANT)
}

fn doc_id(n: u32) -> DocumentId {
    DocumentId::new_unchecked(format!("doc_{n}"))
}

fn blob_key(n: u32) -> String {
    format!("documents/{TENANT}/doc_{n}/file.md")
}

/// A document row for job `n`.
fn document(n: u32) -> DocumentMeta {
    DocumentMeta {
        sha256: None,
        id: doc_id(n),
        title: format!("Document {n}"),
        categories: vec!["finance".to_string()],
        subcategory: None,
        status: "processing".to_string(),
        external_ref: None,
        chunk_count: 0,
        file_type: None,
        file_size: None,
        mime_type: Some("text/markdown".to_string()),
        s3_key: Some(blob_key(n)),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        knowledge_base_ids: vec![],
        metadata: None,
    }
}

/// A job for document `n`, uploaded at `created_at` (epoch milliseconds).
fn job(n: u32, created_at: i64) -> JobRecord {
    JobRecord {
        id: format!("job_{n}"),
        tenant_id: tenant(),
        user_id: None,
        document_id: Some(doc_id(n)),
        s3_key: Some(blob_key(n)),
        filename: format!("file-{n}.md"),
        mime_type: Some("text/markdown".to_string()),
        attempt: 1,
        params: Some(serde_json::json!({"title": format!("Document {n}")})),
        created_at: Some(created_at),
    }
}

/// Enough text for the chunker to make more than one chunk.
fn long_text() -> String {
    (0..6)
        .map(|_| "Section one explains the quarter in detail. It continues across several sentences so the chunker has real work to do. Section two follows with the breakdown by region and by product line.")
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// An extractor that takes `delay` and records how many extractions ran at
/// the same time.
struct CountingExtractor {
    inner: Arc<FakeExtractor>,
    delay: Duration,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Extractor for CountingExtractor {
    async fn extract(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
    ) -> Result<ExtractionResult, PortError> {
        self.extract_with_options(
            filename,
            mime_type,
            file_bytes,
            ExtractionOptions::default(),
        )
        .await
    }

    async fn extract_with_options(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        let result = self
            .inner
            .extract_with_options(filename, mime_type, file_bytes, options)
            .await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

/// An embedder that takes `delay` and records how many embedding calls ran at
/// the same time.
struct CountingEmbedder {
    inner: Arc<FakeEmbedder>,
    delay: Duration,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Embedder for CountingEmbedder {
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        self.inner.embed_query(text).await
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        let out = self.inner.embed_passages(texts).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        out
    }

    fn dimension(&self) -> u32 {
        self.inner.dimension()
    }
}

/// An extractor that blocks on `gate` for one filename, and returns at once
/// for the others. This is the "big scan" of the head-of-line test.
struct BlockingExtractor {
    inner: Arc<FakeExtractor>,
    blocked_file: String,
    gate: Arc<tokio::sync::Notify>,
    entered: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Extractor for BlockingExtractor {
    async fn extract(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
    ) -> Result<ExtractionResult, PortError> {
        self.extract_with_options(
            filename,
            mime_type,
            file_bytes,
            ExtractionOptions::default(),
        )
        .await
    }

    async fn extract_with_options(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        if filename == self.blocked_file {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.gate.notified().await;
        }
        self.inner
            .extract_with_options(filename, mime_type, file_bytes, options)
            .await
    }
}

/// The pieces of a worker over fakes.
struct Harness {
    worker: IngestWorker,
    jobs: Arc<FakeJobStore>,
    documents: Arc<FakeDocumentStore>,
}

fn worker_with(
    jobs: Arc<FakeJobStore>,
    documents: Arc<FakeDocumentStore>,
    chunks: Arc<FakeChunkStore>,
    blobs: Arc<FakeBlobStore>,
    extractor: Arc<dyn Extractor>,
    embedder: Arc<dyn Embedder>,
    settings: IngestSettings,
) -> IngestWorker {
    IngestWorker::new(
        jobs, documents, chunks, blobs, extractor, embedder, settings,
    )
}

/// A harness with `count` documents and their blobs stored.
async fn harness(
    count: u32,
    extractor: Arc<dyn Extractor>,
    embedder: Arc<dyn Embedder>,
    settings: IngestSettings,
) -> Harness {
    let jobs = Arc::new(FakeJobStore::default());
    let documents = Arc::new(FakeDocumentStore::default());
    let chunks = Arc::new(FakeChunkStore::default());
    let blobs = Arc::new(FakeBlobStore::default());
    for n in 0..count {
        documents.insert_doc(tenant(), document(n));
        blobs
            .upload(&blob_key(n), b"%PDF-1.7 fake".to_vec(), "text/markdown")
            .await
            .unwrap();
    }
    let worker = worker_with(
        jobs.clone(),
        documents.clone(),
        chunks,
        blobs,
        extractor,
        embedder,
        settings,
    );
    Harness {
        worker,
        jobs,
        documents,
    }
}

/// The status of document `n`.
fn status(documents: &FakeDocumentStore, n: u32) -> Option<String> {
    documents.docs.read().unwrap()[&tenant()]
        .iter()
        .find(|d| d.id == doc_id(n))
        .map(|d| d.status.clone())
}

/// Wait until `condition` holds, or give up after two seconds.
async fn wait_for<F: Fn() -> bool>(condition: F) -> bool {
    for _ in 0..400 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

/// Three documents are extracted at the same time when the extract stage has
/// three seats.
#[tokio::test]
async fn three_documents_are_extracted_at_once_when_there_are_three_seats() {
    let peak = Arc::new(AtomicUsize::new(0));
    let extractor = Arc::new(CountingExtractor {
        inner: Arc::new(FakeExtractor::returning(&long_text(), "markdown")),
        delay: STAGE_DELAY,
        active: Arc::new(AtomicUsize::new(0)),
        peak: Arc::clone(&peak),
    });
    let h = harness(
        3,
        extractor,
        Arc::new(FakeEmbedder::default()),
        IngestSettings {
            job_workers: 3,
            seats: SeatCapacity {
                extract: 3,
                embed: 4,
                store: 2,
            },
            ..IngestSettings::default()
        },
    )
    .await;
    for n in 0..3 {
        h.jobs.push(job(n, 1_000 + i64::from(n)));
    }

    let cancel = CancelToken::new();
    let token = cancel.clone();
    let handle = tokio::spawn(async move { h.worker.run(token).await });

    assert!(
        wait_for(|| h.jobs.finished.read().unwrap().len() == 3).await,
        "the three documents never finished"
    );
    cancel.cancel();
    handle.await.unwrap();

    assert_eq!(
        peak.load(Ordering::SeqCst),
        3,
        "the three documents were not extracted at the same time"
    );
    for n in 0..3 {
        assert_eq!(status(&h.documents, n).as_deref(), Some("ready"));
    }
}

/// With one extract seat the extraction is serial — but the rest of the
/// pipeline still overlaps, because a document does not hold a seat while it
/// is not in that stage.
#[tokio::test]
async fn one_extract_seat_serialises_the_extraction_and_nothing_else() {
    let extract_peak = Arc::new(AtomicUsize::new(0));
    let embed_peak = Arc::new(AtomicUsize::new(0));
    let extractor = Arc::new(CountingExtractor {
        inner: Arc::new(FakeExtractor::returning(&long_text(), "markdown")),
        delay: STAGE_DELAY,
        active: Arc::new(AtomicUsize::new(0)),
        peak: Arc::clone(&extract_peak),
    });
    let embedder = Arc::new(CountingEmbedder {
        inner: Arc::new(FakeEmbedder::default()),
        // Longer than the extraction, so the second document is still
        // embedding when the first one is: the stage overlap has to be
        // visible, not just possible.
        delay: Duration::from_millis(150),
        active: Arc::new(AtomicUsize::new(0)),
        peak: Arc::clone(&embed_peak),
    });
    let h = harness(
        3,
        extractor,
        embedder,
        IngestSettings {
            job_workers: 3,
            seats: SeatCapacity {
                extract: 1,
                embed: 4,
                store: 2,
            },
            ..IngestSettings::default()
        },
    )
    .await;
    for n in 0..3 {
        h.jobs.push(job(n, 1_000 + i64::from(n)));
    }

    let cancel = CancelToken::new();
    let token = cancel.clone();
    let handle = tokio::spawn(async move { h.worker.run(token).await });

    assert!(
        wait_for(|| h.jobs.finished.read().unwrap().len() == 3).await,
        "the three documents never finished"
    );
    cancel.cancel();
    handle.await.unwrap();

    assert_eq!(
        extract_peak.load(Ordering::SeqCst),
        1,
        "the extraction ran with more than one document in it"
    );
    assert!(
        embed_peak.load(Ordering::SeqCst) > 1,
        "the embedding did not overlap: one seat for extraction must not \
         serialise the whole pipeline (peak {})",
        embed_peak.load(Ordering::SeqCst)
    );
}

/// The head-of-line test: a big document that blocks in the extractor must
/// not hold a small document that was queued behind it.
#[tokio::test]
async fn a_small_document_is_ready_while_a_big_one_is_still_extracting() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(AtomicUsize::new(0));
    let extractor = Arc::new(BlockingExtractor {
        inner: Arc::new(FakeExtractor::returning(&long_text(), "markdown")),
        blocked_file: "file-0.md".to_string(),
        gate: Arc::clone(&gate),
        entered: Arc::clone(&entered),
    });
    let h = harness(
        2,
        extractor,
        Arc::new(FakeEmbedder::default()),
        IngestSettings {
            job_workers: 2,
            seats: SeatCapacity {
                extract: 2,
                embed: 4,
                store: 2,
            },
            ..IngestSettings::default()
        },
    )
    .await;
    // The big document is queued first; the small one arrives behind it.
    h.jobs.push(job(0, 1_000));
    h.jobs.push(job(1, 1_001));

    let cancel = CancelToken::new();
    let token = cancel.clone();
    let handle = tokio::spawn(async move { h.worker.run(token).await });

    // The big document is in the extractor and stays there.
    assert!(
        wait_for(|| entered.load(Ordering::SeqCst) == 1).await,
        "the big document never reached the extractor"
    );
    // The small one is finished while the big one is still blocked.
    assert!(
        wait_for(|| status(&h.documents, 1).as_deref() == Some("ready")).await,
        "the small document waited behind the big one"
    );
    assert_eq!(
        status(&h.documents, 0).as_deref(),
        Some("processing"),
        "the big document was not still running"
    );

    gate.notify_one();
    assert!(
        wait_for(|| h.jobs.finished.read().unwrap().len() == 2).await,
        "the documents never finished"
    );
    cancel.cancel();
    handle.await.unwrap();
    assert_eq!(status(&h.documents, 0).as_deref(), Some("ready"));
}

/// The same test with one extract seat: the small document waits for the
/// seat, because the seat — not the queue — is what bounds the stage.
#[tokio::test]
async fn with_one_extract_seat_the_small_document_waits_for_the_seat() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(AtomicUsize::new(0));
    let extractor = Arc::new(BlockingExtractor {
        inner: Arc::new(FakeExtractor::returning(&long_text(), "markdown")),
        blocked_file: "file-0.md".to_string(),
        gate: Arc::clone(&gate),
        entered: Arc::clone(&entered),
    });
    let h = harness(
        2,
        extractor,
        Arc::new(FakeEmbedder::default()),
        IngestSettings {
            job_workers: 2,
            seats: SeatCapacity {
                extract: 1,
                embed: 4,
                store: 2,
            },
            ..IngestSettings::default()
        },
    )
    .await;
    h.jobs.push(job(0, 1_000));
    h.jobs.push(job(1, 1_001));

    let cancel = CancelToken::new();
    let token = cancel.clone();
    let handle = tokio::spawn(async move { h.worker.run(token).await });

    assert!(
        wait_for(|| entered.load(Ordering::SeqCst) == 1).await,
        "the big document never reached the extractor"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_ne!(
        status(&h.documents, 1).as_deref(),
        Some("ready"),
        "the small document finished while the only extract seat was taken"
    );

    gate.notify_one();
    assert!(
        wait_for(|| h.jobs.finished.read().unwrap().len() == 2).await,
        "the documents never finished"
    );
    cancel.cancel();
    handle.await.unwrap();
    assert_eq!(status(&h.documents, 1).as_deref(), Some("ready"));
}

/// The worker stops claiming when it is cancelled, and the jobs it already
/// admitted are finished rather than abandoned.
#[tokio::test]
async fn cancellation_stops_the_claiming_and_the_admitted_jobs_finish() {
    let extractor = Arc::new(FakeExtractor::returning(&long_text(), "markdown"));
    let h = harness(
        2,
        extractor,
        Arc::new(FakeEmbedder::default()),
        IngestSettings {
            job_workers: 2,
            poll_interval: Duration::from_millis(20),
            ..IngestSettings::default()
        },
    )
    .await;
    h.jobs.push(job(0, 1_000));
    h.jobs.push(job(1, 1_001));

    let cancel = CancelToken::new();
    let token = cancel.clone();
    let handle = tokio::spawn(async move { h.worker.run(token).await });

    assert!(
        wait_for(|| h.jobs.finished.read().unwrap().len() == 2).await,
        "the documents never finished"
    );
    cancel.cancel();
    handle.await.unwrap();
    assert_eq!(h.jobs.pending_len(), 0);
    for n in 0..2 {
        assert_eq!(status(&h.documents, n).as_deref(), Some("ready"));
    }
}
