//! Ingest worker: claim a job, turn the stored file into searchable chunks.
//!
//! Mirrors `processIngestJob` in `src/service/ingest-runner.ts` and the poll
//! loop in `src/lib/ingest/worker.ts`. Job state lives entirely in Postgres, so
//! the worker is crash-safe and interchangeable with the Bun worker: both poll
//! the same table and `FOR UPDATE SKIP LOCKED` hands each row to one of them.
//!
//! Everything the pipeline does not have a port for yet — entity extraction,
//! figure assets, page maps — is skipped, so a document indexed by this worker
//! carries text chunks only. Those steps are additive and land in a later slice.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kb_core::chunk::{ChunkMetadata, SmartChunkingOptions, smart_chunk_document};
use kb_core::{
    IngestStep, ProgressFlags, StepProgress, compute_eta_seconds, compute_overall_progress,
};
use kb_ports::{
    BlobStore, ChunkStore, ChunkToStore, DocumentId, DocumentStore, Embedder, Extractor, JobRecord,
    JobStore, PortError,
};
use serde_json::{Map, Value, json};
use tokio::sync::watch;

/// Chunking settings, copied from the call the Bun worker makes
/// (`src/lib/ingest/index-document.ts`, `smartChunkDocument` options).
pub const INGEST_CHUNKING: SmartChunkingOptions = SmartChunkingOptions {
    max_chunk_size: Some(800),
    overlap_size: Some(200),
    preserve_code_blocks: Some(true),
    respect_heading_boundaries: Some(true),
    respect_section_boundaries: None,
    strategy: None,
};

/// How often a running job is touched, as the Bun worker does
/// (`src/lib/ingest/worker.ts:49`).
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// How a claimed job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    /// The document is ready and its chunks are stored.
    Ready {
        /// How many chunks were written.
        chunks: usize,
    },
    /// The job is terminal-failed; `reason` is what landed on the job row.
    Failed { reason: String },
}

/// Tunables, one per `KB_INGEST_*` environment variable.
#[derive(Debug, Clone)]
pub struct IngestSettings {
    /// Jobs claimed per poll. This slice runs a claimed job to completion
    /// before claiming the next one, so a value above 1 only widens the batch
    /// a single tick takes.
    pub claims_per_tick: usize,
    /// How often the queue is polled for work.
    pub poll_interval: Duration,
    /// How long a `processing` job may go without an update before it is
    /// considered stranded.
    pub stale_after: Duration,
    /// Attempts before a stranded job is failed instead of retried.
    pub max_attempts: i32,
    /// How often a running job is touched so the stranded-job sweep leaves it
    /// alone. Must stay well under `stale_after`.
    pub heartbeat_interval: Duration,
    /// How often the stranded-job sweep runs.
    pub reclaim_interval: Duration,
    /// Stored on each chunk so a later query can tell which model indexed it.
    pub embedding_model: Option<String>,
    /// Chunking settings.
    pub chunking: SmartChunkingOptions,
}

impl Default for IngestSettings {
    fn default() -> Self {
        Self {
            claims_per_tick: 1,
            poll_interval: Duration::from_secs(3),
            stale_after: Duration::from_secs(300),
            max_attempts: 3,
            heartbeat_interval: HEARTBEAT_INTERVAL,
            reclaim_interval: Duration::from_secs(60),
            embedding_model: None,
            chunking: INGEST_CHUNKING,
        }
    }
}

/// Cancellation handle for a running worker. Cloning shares the flag, so the
/// process can hold one copy while a signal handler holds another.
#[derive(Clone, Debug)]
pub struct CancelToken {
    tx: watch::Sender<bool>,
}

impl CancelToken {
    /// A token that has not been cancelled.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx }
    }

    /// Ask the worker to stop after the job it is running finishes.
    ///
    /// `send_replace` rather than `send`: the token keeps no receiver of its
    /// own, and `send` drops the value when there are none, which would make
    /// cancelling before the worker subscribes a silent no-op.
    pub fn cancel(&self) {
        self.tx.send_replace(true);
    }

    /// Watch for cancellation. The returned receiver resolves on every change.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// The ingest pipeline. Holds ports, names no adapter.
pub struct IngestWorker {
    jobs: Arc<dyn JobStore>,
    documents: Arc<dyn DocumentStore>,
    chunks: Arc<dyn ChunkStore>,
    blobs: Arc<dyn BlobStore>,
    extractor: Arc<dyn Extractor>,
    embedder: Arc<dyn Embedder>,
    settings: IngestSettings,
}

impl IngestWorker {
    /// Assemble a worker from its ports.
    #[must_use]
    pub fn new(
        jobs: Arc<dyn JobStore>,
        documents: Arc<dyn DocumentStore>,
        chunks: Arc<dyn ChunkStore>,
        blobs: Arc<dyn BlobStore>,
        extractor: Arc<dyn Extractor>,
        embedder: Arc<dyn Embedder>,
        settings: IngestSettings,
    ) -> Self {
        Self {
            jobs,
            documents,
            chunks,
            blobs,
            extractor,
            embedder,
            settings,
        }
    }

    /// Poll until cancelled, sweeping for stranded jobs on `reclaim_interval`.
    ///
    /// The job in flight is never abandoned: cancellation is only observed
    /// between claims, because a half-indexed document is exactly the state the
    /// stale reclaim exists to clean up.
    pub async fn run(&self, cancel: CancelToken) {
        let mut cancelled = cancel.subscribe();
        let mut since_reclaim = Duration::ZERO;
        self.reclaim().await;
        loop {
            if *cancelled.borrow() {
                return;
            }
            self.tick().await;
            since_reclaim = since_reclaim.saturating_add(self.settings.poll_interval);
            if since_reclaim >= self.settings.reclaim_interval {
                self.reclaim().await;
                since_reclaim = Duration::ZERO;
            }
            tokio::select! {
                () = tokio::time::sleep(self.settings.poll_interval) => {}
                _ = cancelled.changed() => {
                    if *cancelled.borrow() {
                        // One last sweep, so a job the departing worker was
                        // holding does not sit stranded until the next boot.
                        self.reclaim().await;
                        return;
                    }
                }
            }
        }
    }

    /// Claim up to `claims_per_tick` jobs and run them.
    async fn tick(&self) {
        for _ in 0..self.settings.claims_per_tick.max(1) {
            match self.jobs.claim_next_pending().await {
                Ok(Some(job)) => {
                    self.run_job(job).await;
                }
                Ok(None) => return,
                Err(err) => {
                    tracing::warn!(error = %err, "ingest claim failed");
                    return;
                }
            }
        }
    }

    /// Claim and run one job. Returns `false` when the queue was empty.
    ///
    /// This is the unit the tests drive: it is the whole claim-to-finish
    /// transition without the polling loop around it.
    ///
    /// # Errors
    ///
    /// Only a failed claim propagates. A job that runs and fails is recorded
    /// on the job row, exactly as the Bun worker records it.
    pub async fn run_once(&self) -> Result<bool, PortError> {
        let Some(job) = self.jobs.claim_next_pending().await? else {
            return Ok(false);
        };
        self.run_job(job).await;
        Ok(true)
    }

    /// Run a claimed job and record its terminal state.
    ///
    /// The heartbeat guard is held for the whole call, so it stops on every
    /// exit path — success, failure, or a `run_job` the caller dropped.
    pub async fn run_job(&self, job: JobRecord) -> JobOutcome {
        let _heartbeat = self.start_heartbeat(&job.id);
        let started = Instant::now();
        tracing::info!(job_id = %job.id, filename = %job.filename, "ingest claimed");
        let outcome = match self.index(&job, started).await {
            Ok((chunks, document_id)) => {
                if let Err(err) = self.jobs.finish_success(&job.id, &document_id).await {
                    // The chunks are already stored and the document is ready;
                    // the stale reclaim replays the job, which is safe because
                    // chunk ids are deterministic.
                    tracing::error!(error = %err, job_id = %job.id, "could not mark job successful");
                }
                JobOutcome::Ready { chunks }
            }
            Err(reason) => {
                self.mark_failed(&job, &reason).await;
                JobOutcome::Failed { reason }
            }
        };
        tracing::info!(job_id = %job.id, outcome = ?outcome, "ingest finished");
        outcome
    }

    /// Start touching `job_id` every `heartbeat_interval`.
    ///
    /// Steps that emit no progress (a long extraction, a big embedding batch)
    /// would otherwise leave `updatedAt` untouched for longer than
    /// `stale_after`, and the reclaim sweep would requeue a job that is still
    /// running — the Bun worker heartbeats for exactly this reason
    /// (`src/lib/ingest/worker.ts:49`).
    fn start_heartbeat(&self, job_id: &str) -> Heartbeat {
        let jobs = Arc::clone(&self.jobs);
        let job_id = job_id.to_string();
        let every = self.settings.heartbeat_interval;
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            // A stalled runtime should not then fire a burst of catch-up ticks.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // `setInterval` waits a full period before its first call, and the
            // claim already wrote `updatedAt`, so skip the immediate tick.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if let Err(err) = jobs.touch(&job_id).await {
                    tracing::warn!(error = %err, job_id = %job_id, "heartbeat touch failed");
                }
            }
        });
        Heartbeat(handle)
    }

    /// The pipeline proper. Every failure is a `reason` string rather than an
    /// error type, because the reason is what lands on the job row.
    async fn index(
        &self,
        job: &JobRecord,
        started: Instant,
    ) -> Result<(usize, DocumentId), String> {
        if job.tenant_id.as_str().trim().is_empty() {
            return Err("job has no tenant".to_string());
        }
        let (Some(document_id), Some(s3_key)) = (&job.document_id, &job.s3_key) else {
            return Err("job missing documentId or s3Key".to_string());
        };

        let bytes = self
            .blobs
            .download(s3_key)
            .await
            .map_err(|e| format!("blob download failed: {e}"))?;

        self.progress(job, StepProgress::of(IngestStep::Extracting), started)
            .await;

        let mime = job
            .mime_type
            .as_deref()
            .unwrap_or("application/octet-stream");
        let extraction = self
            .extractor
            .extract(&job.filename, mime, &bytes)
            .await
            .map_err(|e| e.to_string())?;
        // A parser that could not read the file reports the reason and leaves
        // `content` empty (`extract.ts:192`). Indexing that would put an empty
        // document in the KB, so the job fails with the parser's own message —
        // retrying with different settings beats a poisoned document.
        if let Some(reason) = &extraction.error {
            return Err(reason.clone());
        }

        let params = job.params.clone().unwrap_or_else(|| json!({}));
        let title = job_title(&params, &job.filename);
        // NUL bytes survive some extractors and break Postgres text columns.
        let content: String = extraction.content.chars().filter(|c| *c != '\0').collect();

        let file_size = i32::try_from(bytes.len()).unwrap_or(i32::MAX);
        self.documents
            .update_extracted_content(
                document_id,
                &content,
                Some(&extraction.file_type),
                Some(file_size),
            )
            .await
            .map_err(|e| format!("document update failed: {e}"))?;

        self.progress(job, StepProgress::of(IngestStep::Chunking), started)
            .await;
        let chunks = smart_chunk_document(
            &content,
            &title,
            category_of(&params),
            subcategory_of(&params).as_deref(),
            self.settings.chunking,
        );

        if chunks.is_empty() {
            // The Bun worker reaches the same place: `storeChunks` returns
            // early on an empty batch, and the document is still marked ready.
            return self.finish_ready(document_id, 0).await;
        }

        let total = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
        let texts: Vec<String> = chunks
            .iter()
            .map(|c| format!("{title}\n\n{}", c.content))
            .collect();
        self.progress(
            job,
            StepProgress::at(IngestStep::Embedding, 0, total),
            started,
        )
        .await;
        let embeddings = self
            .embedder
            .embed_passages(&texts)
            .await
            .map_err(|e| format!("embedding failed: {e}"))?;
        if embeddings.len() != chunks.len() {
            // `generateEmbeddings` degrades to a short list rather than
            // throwing, and a short list would index some chunks with nothing.
            return Err(format!(
                "embedding batch returned {} vectors for {} chunks",
                embeddings.len(),
                chunks.len()
            ));
        }

        self.progress(
            job,
            StepProgress::at(IngestStep::Storing, 0, total),
            started,
        )
        .await;
        let to_store: Vec<ChunkToStore> = chunks
            .iter()
            .zip(embeddings)
            .map(|(chunk, embedding)| ChunkToStore {
                content: chunk.content.clone(),
                chunk_index: chunk.metadata.chunk_index,
                embedding,
                metadata: storage_metadata(&chunk.metadata),
                contextual_prefix: chunk.metadata.contextual_prefix.clone(),
            })
            .collect();

        // Clear first: `store_chunks` writes deterministic ids, so stale chunks
        // from a killed attempt would otherwise sit next to the new ones.
        self.chunks
            .delete_by_document(document_id)
            .await
            .map_err(|e| format!("clearing old chunks failed: {e}"))?;
        self.chunks
            .store_chunks(
                document_id,
                &to_store,
                self.settings.embedding_model.as_deref(),
            )
            .await
            .map_err(|e| format!("storing chunks failed: {e}"))?;

        self.finish_ready(document_id, to_store.len()).await
    }

    /// Flip the document to `ready`. A document with no chunks is still ready:
    /// the file is readable, there is simply nothing for RAG to match yet.
    async fn finish_ready(
        &self,
        document_id: &DocumentId,
        chunks: usize,
    ) -> Result<(usize, DocumentId), String> {
        self.documents
            .set_status(document_id, "ready")
            .await
            .map_err(|e| format!("marking document ready failed: {e}"))?;
        Ok((chunks, document_id.clone()))
    }

    /// Terminal failure: job row plus, when known, the document.
    async fn mark_failed(&self, job: &JobRecord, reason: &str) {
        if let Err(err) = self.jobs.finish_failure(&job.id, reason).await {
            tracing::error!(error = %err, job_id = %job.id, "could not mark job failed");
        }
        if let Some(document_id) = &job.document_id {
            if let Err(err) = self.documents.set_status(document_id, "failed").await {
                tracing::error!(error = %err, "could not mark document failed");
            }
        }
    }

    /// Record progress. A failed progress write is logged and dropped: it must
    /// not abort an ingest that is otherwise fine.
    async fn progress(&self, job: &JobRecord, step: StepProgress, started: Instant) {
        let flags = ProgressFlags {
            entities: false,
            figures: false,
        };
        let overall = compute_overall_progress(step, flags);
        let eta = compute_eta_seconds(overall, Some(started), Instant::now());
        if let Err(err) = self
            .jobs
            .update_progress(
                &job.id,
                step.step.as_str(),
                overall,
                step.current,
                step.total,
                eta,
            )
            .await
        {
            tracing::warn!(error = %err, job_id = %job.id, "progress update failed");
        }
    }

    /// Sweep jobs whose worker died mid-run.
    pub async fn reclaim(&self) {
        match self
            .jobs
            .reclaim_stale(self.settings.stale_after, self.settings.max_attempts)
            .await
        {
            Ok(n) if n > 0 => tracing::info!(count = n, "reclaimed stale ingest jobs"),
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "stale job reclaim failed"),
        }
    }
}

/// Owns a job's heartbeat task and stops it when dropped.
///
/// `Drop` rather than an explicit call at each exit: the task must also stop
/// when a caller drops a `run_job` future mid-job, or the job would keep being
/// touched long after the worker stopped working on it.
struct Heartbeat(tokio::task::JoinHandle<()>);

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Title for the embedding prefix and the chunk metadata: the job's explicit
/// title, else the filename without its extension.
fn job_title(params: &Value, filename: &str) -> String {
    if let Some(t) = params.get("title").and_then(Value::as_str) {
        if !t.trim().is_empty() {
            return t.to_string();
        }
    }
    match filename.rfind('.') {
        Some(0) | None => filename.to_string(),
        Some(i) => filename[..i].to_string(),
    }
}

fn category_of(params: &Value) -> &str {
    params
        .get("categories")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn subcategory_of(params: &Value) -> Option<String> {
    params
        .get("subcategory")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The metadata object stored on each chunk, with the field names the Bun
/// chunker writes. An absent value is left out rather than written as null,
/// matching how an `undefined` property drops out of the TS object.
fn storage_metadata(meta: &ChunkMetadata) -> Value {
    let mut map = Map::new();
    map.insert("documentTitle".to_string(), json!(meta.document_title));
    map.insert("category".to_string(), json!(meta.category));
    if let Some(sub) = &meta.subcategory {
        map.insert("subcategory".to_string(), json!(sub));
    }
    if let Some(section) = &meta.section {
        map.insert("section".to_string(), json!(section));
    }
    map.insert("chunkIndex".to_string(), json!(meta.chunk_index));
    if let Some(chunk_type) = &meta.chunk_type {
        map.insert("chunkType".to_string(), json!(chunk_type));
    }
    if let Some(prefix) = &meta.contextual_prefix {
        map.insert("contextualPrefix".to_string(), json!(prefix));
    }
    Value::Object(map)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn meta() -> ChunkMetadata {
        ChunkMetadata {
            document_title: "Report".to_string(),
            category: "finance".to_string(),
            subcategory: Some("q3".to_string()),
            section: Some("Intro".to_string()),
            chunk_index: 4,
            contextual_prefix: None,
            chunk_type: Some("text".to_string()),
        }
    }

    #[test]
    fn title_falls_back_to_the_filename_stem() {
        assert_eq!(job_title(&json!({}), "report.pdf"), "report");
        assert_eq!(job_title(&json!({}), "archive.tar.gz"), "archive.tar");
        assert_eq!(job_title(&json!({}), "noextension"), "noextension");
        assert_eq!(job_title(&json!({"title": "Given"}), "report.pdf"), "Given");
        assert_eq!(job_title(&json!({"title": "  "}), "report.pdf"), "report");
    }

    #[test]
    fn category_defaults_to_empty_when_absent() {
        assert_eq!(category_of(&json!({})), "");
        assert_eq!(category_of(&json!({"categories": ["ops"]})), "ops");
        assert_eq!(category_of(&json!({"categories": []})), "");
        assert_eq!(subcategory_of(&json!({})), None);
        assert_eq!(
            subcategory_of(&json!({"subcategory": "x"})),
            Some("x".to_string())
        );
    }

    #[test]
    fn stored_metadata_uses_the_typescript_field_names() {
        let stored = storage_metadata(&meta());
        assert_eq!(stored["documentTitle"], json!("Report"));
        assert_eq!(stored["category"], json!("finance"));
        assert_eq!(stored["subcategory"], json!("q3"));
        assert_eq!(stored["section"], json!("Intro"));
        assert_eq!(stored["chunkIndex"], json!(4));
        assert_eq!(stored["chunkType"], json!("text"));
        assert!(stored.get("contextualPrefix").is_none());
        assert!(stored.get("page").is_none());
    }

    #[test]
    fn chunking_matches_the_bun_worker_call() {
        assert_eq!(INGEST_CHUNKING.max_chunk_size, Some(800));
        assert_eq!(INGEST_CHUNKING.overlap_size, Some(200));
        assert_eq!(INGEST_CHUNKING.preserve_code_blocks, Some(true));
        assert_eq!(INGEST_CHUNKING.respect_heading_boundaries, Some(true));
    }

    #[tokio::test]
    async fn cancel_token_is_observed() {
        let token = CancelToken::new();
        let mut rx = token.subscribe();
        assert!(!*rx.borrow());
        token.cancel();
        rx.changed().await.unwrap();
        assert!(*rx.borrow());
    }
}
