//! Ingest worker: claim a job, turn the stored file into searchable chunks.
//!
//! Mirrors `processIngestJob` in `src/service/ingest-runner.ts` and the poll
//! loop in `src/lib/ingest/worker.ts`. Job state lives entirely in Postgres, so
//! the worker is crash-safe and interchangeable with the Bun worker: both poll
//! the same table and `FOR UPDATE SKIP LOCKED` hands each row to one of them.
//!
//! Entity extraction, figure assets, and page maps are fully integrated
//! mirroring `src/lib/ingest/index-document.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kb_core::chunk::{Chunk, ChunkMetadata, SmartChunkingOptions, smart_chunk_document};
use kb_core::figure_assets::{
    LayoutBlockRef, build_page_text, context_for_page, derive_label, figure_kind_word,
    normalize_figure_type, resolve_figure_anchors,
};
use kb_core::policy::{parse_figure_mode, resolve_ingest_policy};
use kb_core::{
    IngestStep, ProgressFlags, StepProgress, compute_eta_seconds, compute_overall_progress,
};
use kb_ports::{
    BlobStore, ChunkId, ChunkType, DocumentId, DocumentStore, Embedder, EntityExtractor,
    EntityStore, EntityUpsert, ExtractedFigure, ExtractionOptions, Extractor, GraphStore,
    IndexUnit, JobRecord, JobStore, PortError, ProgressSink, RelationUpsert, RetrievalIndex,
    asset_path,
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

/// Minimum gap between two progress frames for the same step, as
/// `updateIngestJobProgress` throttles in `src/lib/ingest/job.ts:72`.
pub const PROGRESS_EMIT_MIN_GAP: Duration = Duration::from_millis(900);

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
    index: Arc<dyn RetrievalIndex>,
    blobs: Arc<dyn BlobStore>,
    extractor: Arc<dyn Extractor>,
    embedder: Arc<dyn Embedder>,
    entities: Option<Arc<dyn EntityStore>>,
    entity_extractor: Option<Arc<dyn EntityExtractor>>,
    /// The Postgres entity graph, when one is configured. Written as a second
    /// destination for the *same* extraction, never instead of it: SurrealDB
    /// keeps its rows because Bun and `/v1/documents/:id/intelligence` read
    /// them, and the graph tables exist so the graph arm has something to walk.
    graph: Option<Arc<dyn GraphStore>>,
    progress_sink: Option<Arc<dyn ProgressSink>>,
    /// Per-job throttle state for progress emits, mirroring `lastEmit` in
    /// `src/lib/ingest/job.ts`: always emit on a step change, otherwise at most
    /// one emit per `PROGRESS_EMIT_MIN_GAP`. Without it a large book produces
    /// hundreds of socket frames per second for a single document.
    emit_state: Mutex<HashMap<String, (Instant, String)>>,
    settings: IngestSettings,
}

impl IngestWorker {
    /// Assemble a worker from its primary ports.
    #[must_use]
    pub fn new(
        jobs: Arc<dyn JobStore>,
        documents: Arc<dyn DocumentStore>,
        index: Arc<dyn RetrievalIndex>,
        blobs: Arc<dyn BlobStore>,
        extractor: Arc<dyn Extractor>,
        embedder: Arc<dyn Embedder>,
        settings: IngestSettings,
    ) -> Self {
        Self {
            jobs,
            documents,
            index,
            blobs,
            extractor,
            embedder,
            entities: None,
            entity_extractor: None,
            graph: None,
            progress_sink: None,
            emit_state: Mutex::new(HashMap::new()),
            settings,
        }
    }

    /// Equip the worker with entity storage and extraction ports.
    #[must_use]
    pub fn with_entities(
        mut self,
        entities: Arc<dyn EntityStore>,
        extractor: Arc<dyn EntityExtractor>,
    ) -> Self {
        self.entities = Some(entities);
        self.entity_extractor = Some(extractor);
        self
    }

    /// Equip the worker with an entity store.
    #[must_use]
    pub fn with_entity_store(mut self, entities: Arc<dyn EntityStore>) -> Self {
        self.entities = Some(entities);
        self
    }

    /// Equip the worker with an entity extractor.
    #[must_use]
    pub fn with_entity_extractor(mut self, extractor: Arc<dyn EntityExtractor>) -> Self {
        self.entity_extractor = Some(extractor);
        self
    }

    /// Equip the worker with a graph store, so the same extraction also lands in
    /// the entity graph the graph arm walks.
    ///
    /// Independent of [`IngestWorker::with_entity_store`] on purpose: the two are
    /// different stores with different readers, and a deployment may legitimately
    /// have one without the other.
    #[must_use]
    pub fn with_graph_store(mut self, graph: Arc<dyn GraphStore>) -> Self {
        self.graph = Some(graph);
        self
    }

    /// Equip the worker with a progress sink.
    #[must_use]
    pub fn with_progress_sink(mut self, sink: Arc<dyn ProgressSink>) -> Self {
        self.progress_sink = Some(sink);
        self
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

        let mime = job
            .mime_type
            .as_deref()
            .unwrap_or("application/octet-stream");
        let params = job.params.clone().unwrap_or_else(|| json!({}));
        let figure_mode = parse_figure_mode(
            params.get("figureMode").and_then(Value::as_str),
            params.get("forceOCR").and_then(Value::as_bool),
        );
        let doc_type = params
            .get("documentType")
            .and_then(Value::as_str)
            .map(str::to_string);
        let use_combined = params
            .get("useCombined")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let policy = resolve_ingest_policy(&job.filename, figure_mode);

        let entity_extraction_enabled = std::env::var("KB_ENTITY_EXTRACTION_ENABLED")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);

        let flags = ProgressFlags {
            entities: policy.entities && entity_extraction_enabled,
            figures: policy.figures,
        };

        self.progress(
            job,
            StepProgress::of(IngestStep::Extracting),
            started,
            &flags,
        )
        .await;

        let options = ExtractionOptions {
            figures: policy.figures,
            force_layout: policy.force_layout,
            document_type: doc_type,
        };
        let extraction = self
            .extractor
            .extract_with_options(&job.filename, mime, &bytes, options)
            .await
            .map_err(|e| e.to_string())?;

        // A parser that could not read the file reports the reason and leaves
        // `content` empty (`extract.ts:192`). Indexing that would put an empty
        // document in the KB, so the job fails with the parser's own message —
        // retrying with different settings beats a poisoned document.
        if let Some(reason) = &extraction.error {
            return Err(reason.clone());
        }

        // NUL bytes survive some extractors and break Postgres text columns.
        let content: String = extraction.content.chars().filter(|c| *c != '\0').collect();

        // The title: the job's explicit one, else the document's own H1 (a
        // markdown or text file usually starts with one), else the filename
        // stem.
        let title = job_title(
            &params,
            &job.filename,
            &content,
            is_markdown_text(&extraction.file_type),
        );

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

        self.progress(job, StepProgress::of(IngestStep::Chunking), started, &flags)
            .await;
        let mut chunks = smart_chunk_document(
            &content,
            &title,
            category_of(&params),
            subcategory_of(&params).as_deref(),
            self.settings.chunking,
        );

        // Entity / relation extraction (non-fatal, mirrors index-document.ts:58-185)
        if policy.entities {
            if !entity_extraction_enabled {
                tracing::info!(
                    "[Knowledge] entity extraction disabled (KB_ENTITY_EXTRACTION_ENABLED=false)"
                );
            } else {
                self.progress(
                    job,
                    StepProgress::of(IngestStep::ExtractingEntities),
                    started,
                    &flags,
                )
                .await;
                self.extract_and_store_entities(
                    job,
                    document_id,
                    &content,
                    &chunks,
                    use_combined,
                    started,
                    &flags,
                )
                .await;
            }
        }

        // Page map assignment: tag text chunks with their source page from the layout parser
        if let Some(map) = &extraction.page_map {
            if !map.is_empty() {
                let page_blocks: Vec<kb_core::page_map::PageBlock> = map
                    .iter()
                    .map(|p| kb_core::page_map::PageBlock {
                        page: p.page,
                        text: p.text.clone(),
                    })
                    .collect();
                let annotated = kb_core::page_map::assign_chunk_pages(chunks, &page_blocks);
                chunks = annotated
                    .into_iter()
                    .map(|a| Chunk {
                        content: a.content,
                        metadata: a.metadata,
                    })
                    .collect();
            }
        }

        // Figure asset layer (multimodal RAG): upload crops + append searchable figure chunks
        if !extraction.figures.is_empty() {
            self.progress(
                job,
                StepProgress::at(
                    IngestStep::ProcessingFigures,
                    0,
                    extraction.figures.len() as u32,
                ),
                started,
                &flags,
            )
            .await;
            self.process_figures(
                job.tenant_id.as_str(),
                document_id,
                &title,
                category_of(&params),
                subcategory_of(&params).as_deref(),
                &extraction.figures,
                &mut chunks,
                extraction.pages_blocks.as_deref(),
            )
            .await;
        }

        if chunks.is_empty() {
            // The Bun worker reaches the same place: `storeChunks` returns
            // early on an empty batch, and the document is still marked ready.
            return self
                .finish_ready(job, document_id, 0, started, &flags)
                .await;
        }

        let total = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
        // The text embedded for a chunk: the document's title, the chunk's
        // heading path when the chunker found one, and the chunk's content.
        //
        // The title used to be the filename stem when the upload had no
        // title, which for a corpus of `1000580_1.md` files meant every
        // chunk was embedded as "1000580_1\n\n<text>" — the same useless
        // prefix on every passage of the collection. And a chunk after the
        // first lost the document's heading entirely, because the title was
        // only ever the file's name. Both are fixed here
        // (`docs/kf-vplane-eval.md`).
        let texts: Vec<String> = chunks
            .iter()
            .map(|c| {
                kb_core::title::embedding_text(
                    &title,
                    c.metadata.heading_path.as_deref(),
                    &c.content,
                )
            })
            .collect();
        self.progress(
            job,
            StepProgress::at(IngestStep::Embedding, 0, total),
            started,
            &flags,
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
            StepProgress::at(IngestStep::Embedding, total, total),
            started,
            &flags,
        )
        .await;

        self.progress(
            job,
            StepProgress::at(IngestStep::Storing, 0, total),
            started,
            &flags,
        )
        .await;
        let units: Vec<IndexUnit> = chunks
            .iter()
            .zip(embeddings)
            .map(|(chunk, dense)| {
                let chunk_type = match chunk.metadata.chunk_type.as_deref() {
                    Some("figure") => ChunkType::Figure,
                    Some("table") => ChunkType::Table,
                    _ => ChunkType::Text,
                };
                let sparse = kb_core::sparse::encode_document(&chunk.content);
                IndexUnit {
                    id: ChunkId(format!("{}_{}", document_id.0, chunk.metadata.chunk_index)),
                    document_id: document_id.clone(),
                    tenant: job.tenant_id.clone(),
                    knowledge_base_id: None,
                    category: Some(chunk.metadata.category.clone()),
                    chunk_index: chunk.metadata.chunk_index,
                    page: chunk.metadata.page,
                    chunk_type,
                    content: chunk.content.clone(),
                    contextual_prefix: chunk.metadata.contextual_prefix.clone(),
                    metadata: storage_metadata(&chunk.metadata),
                    dense,
                    sparse,
                }
            })
            .collect();

        // Clear first: `delete_document` removes old units for this document within tenant.
        self.index
            .delete_document(&job.tenant_id, document_id)
            .await
            .map_err(|e| format!("clearing old chunks failed: {e}"))?;
        let written = units.len();
        self.index
            .upsert(units)
            .await
            .map_err(|e| format!("storing chunks failed: {e}"))?;

        self.finish_ready(job, document_id, written, started, &flags)
            .await
    }

    /// Extract entities and relations once, then write them to every store that
    /// wants them. Non-fatal on failure: logs a warning and continues.
    ///
    /// One extraction, two destinations. The SurrealDB write is unchanged — same
    /// ids, same order, same skips — because Bun reads those rows and so does the
    /// intelligence route, and AGENTS rule 4 makes them a contract. The graph
    /// write is additive and best-effort: a graph that fails to load must not
    /// cost the document its search index.
    #[allow(clippy::too_many_arguments)]
    async fn extract_and_store_entities(
        &self,
        job: &JobRecord,
        document_id: &DocumentId,
        content: &str,
        chunks: &[Chunk],
        use_combined: bool,
        started: Instant,
        flags: &ProgressFlags,
    ) {
        let Some((entities, relations)) = self
            .extract_entities(job, content, document_id, use_combined)
            .await
        else {
            return;
        };

        self.store_entities_surreal(job, document_id, &entities, &relations, started, flags)
            .await;
        self.store_entities_graph(job, document_id, chunks, &entities, &relations)
            .await;
    }

    /// Run the configured extractor, or the pattern fallback. `None` when the
    /// extraction failed: every caller treats that as non-fatal.
    async fn extract_entities(
        &self,
        job: &JobRecord,
        content: &str,
        document_id: &DocumentId,
        use_combined: bool,
    ) -> Option<(Vec<kb_core::Entity>, Vec<kb_core::Relation>)> {
        if let Some(extractor) = &self.entity_extractor {
            if use_combined {
                match extractor
                    .extract_entities_and_relations(
                        content,
                        Some(&document_id.0),
                        job.user_id.as_deref(),
                    )
                    .await
                {
                    Ok(pair) => return Some(pair),
                    Err(e) => {
                        tracing::warn!("Entity/Relation extraction failed (non-fatal): {e}");
                        return None;
                    }
                }
            }
            match extractor
                .extract_entities(content, Some(&document_id.0), None)
                .await
            {
                Ok(ents) => return Some((ents, Vec::new())),
                Err(e) => {
                    tracing::warn!("Entity extraction failed (non-fatal): {e}");
                    return None;
                }
            }
        }

        // Pattern fallback
        let pat_extractor = kb_core::intelligence::PatternEntityExtractor::default();
        let entities = pat_extractor.extract(content, Some(&document_id.0), None);
        let relations = if use_combined {
            let pat_rel_extractor = kb_core::intelligence::PatternRelationExtractor::default();
            pat_rel_extractor.extract(
                content,
                &entities,
                Some(&document_id.0),
                job.user_id.as_deref(),
            )
        } else {
            Vec::new()
        };
        Some((entities, relations))
    }

    /// Write entities and relations to SurrealDB, exactly as before the graph
    /// store existed. Nothing here changes shape, id or ordering.
    async fn store_entities_surreal(
        &self,
        job: &JobRecord,
        document_id: &DocumentId,
        entities: &[kb_core::Entity],
        relations: &[kb_core::Relation],
        started: Instant,
        flags: &ProgressFlags,
    ) {
        let Some(store) = &self.entities else {
            return;
        };

        let total_entities = entities.len() as u32;
        let mut entity_idx: u32 = 0;
        let mut entity_id_map: HashMap<String, String> = HashMap::new();

        for entity in entities.iter() {
            entity_idx = entity_idx.saturating_add(1);
            self.progress(
                job,
                StepProgress::at(IngestStep::ExtractingEntities, entity_idx, total_entities),
                started,
                flags,
            )
            .await;

            let sanitized_name: String = entity
                .name
                .to_lowercase()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let entity_id = format!("entity:{document_id}_{sanitized_name}");

            if let Err(e) = store
                .upsert_entity(
                    document_id,
                    &sanitized_name,
                    &entity.name,
                    &entity.entity_type,
                    entity.confidence,
                    entity.metadata.as_ref(),
                )
                .await
            {
                tracing::warn!("[kb-ingest] Failed to upsert entity {entity_id}: {e}");
            }

            entity_id_map.insert(entity.name.to_lowercase(), entity_id);
        }

        if !relations.is_empty() {
            let mut stored_count = 0;
            let mut skipped_count = 0;
            for relation in relations.iter() {
                let source_name = relation
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("source_entity"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                let target_name = relation
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("target_entity"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                let source_id = entity_id_map.get(&source_name);
                let target_id = entity_id_map.get(&target_name);

                let (Some(source_id), Some(target_id)) = (source_id, target_id) else {
                    skipped_count += 1;
                    continue;
                };

                let raw_rel = if relation.relation_type.trim().is_empty() {
                    "RELATED_TO"
                } else {
                    relation.relation_type.as_str()
                };
                let rel_type: String = raw_rel
                    .to_uppercase()
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                let rel_type = rel_type.trim_matches('_');
                let rel_type = if rel_type.is_empty() {
                    "RELATED_TO"
                } else {
                    rel_type
                };

                let context = relation
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("context"))
                    .and_then(Value::as_str);

                if let Err(e) = store
                    .relate_entities(
                        source_id,
                        rel_type,
                        target_id,
                        relation.confidence,
                        document_id,
                        context,
                    )
                    .await
                {
                    tracing::warn!(
                        "[kb-ingest] Failed to create relation {source_id} ->{rel_type}-> {target_id}: {e}"
                    );
                } else {
                    stored_count += 1;
                }
            }
            tracing::info!(
                "[kb-ingest] Relations: {stored_count} stored, {skipped_count} skipped (no matching entity), {} total",
                relations.len()
            );
        }
    }

    /// Write the same extraction to the graph store: the document's entities with
    /// their chunk mentions, then its relations.
    ///
    /// **Non-fatal by design.** Every store call here logs a warning and returns.
    /// The alternative — failing the job — turns a degraded graph into an
    /// unsearchable document, which is a much worse outcome than a search that
    /// quietly has one arm fewer. AGENTS rule 6 wants that visible, and a
    /// `warn!` with the document id is how the SurrealDB write above has always
    /// reported the same class of failure.
    ///
    /// **Replace, not merge.** A re-ingest deletes this document's graph first.
    /// Without that, a document re-ingested with different content keeps every
    /// relation its previous version had, and the graph asserts things about a
    /// version of the document that no longer exists.
    async fn store_entities_graph(
        &self,
        job: &JobRecord,
        document_id: &DocumentId,
        chunks: &[Chunk],
        entities: &[kb_core::Entity],
        relations: &[kb_core::Relation],
    ) {
        let Some(graph) = &self.graph else {
            return;
        };

        if let Err(e) = graph.delete_document(&job.tenant_id, document_id).await {
            tracing::warn!(
                document_id = %document_id.0,
                error = %e,
                "[kb-ingest] graph delete failed (non-fatal): the graph may keep stale edges",
            );
            return;
        }

        let mentions = MentionIndex::build(chunks, document_id);
        let mut keys: HashMap<String, String> = HashMap::new();
        let mut upserts: Vec<EntityUpsert> = Vec::with_capacity(entities.len());
        for entity in entities {
            // `key_of` is the same normaliser the heuristic extractor uses, so an
            // entity's key is the same string whichever extractor produced it —
            // which is what makes the two extractors interchangeable on the same
            // tenant.
            let Some(key) = kb_core::key_of(&entity.name) else {
                continue;
            };
            keys.insert(entity.name.to_lowercase(), key.clone());
            upserts.push(EntityUpsert {
                key: key.clone(),
                name: entity.name.clone(),
                entity_type: entity.entity_type.clone(),
                aliases: Vec::new(),
                mentions: mentions.chunks_for(&entity.name),
            });
        }

        if let Err(e) = graph
            .upsert_entities(&job.tenant_id, document_id, upserts)
            .await
        {
            tracing::warn!(
                document_id = %document_id.0,
                error = %e,
                "[kb-ingest] graph entity upsert failed (non-fatal): {} entities lost",
                entities.len(),
            );
            return;
        }

        let relation_upserts = graph_relations(relations, &keys, &mentions);
        if let Err(e) = graph
            .relate(&job.tenant_id, document_id, relation_upserts.clone())
            .await
        {
            tracing::warn!(
                document_id = %document_id.0,
                error = %e,
                "[kb-ingest] graph relate failed (non-fatal): {} relations lost",
                relation_upserts.len(),
            );
            return;
        }
        tracing::info!(
            document_id = %document_id.0,
            entities = entities.len(),
            relations = relation_upserts.len(),
            "[kb-ingest] graph written",
        );
    }

    /// Process figure crops: upload images to BlobStore, compute captions and anchors,
    /// and build searchable figure chunks. Non-fatal: logs warning and continues.
    #[allow(clippy::too_many_arguments)]
    async fn process_figures(
        &self,
        tenant_id: &str,
        document_id: &DocumentId,
        document_title: &str,
        category: &str,
        subcategory: Option<&str>,
        figures: &[ExtractedFigure],
        chunks: &mut Vec<Chunk>,
        pages_blocks: Option<&[Vec<kb_ports::PageBlock>]>,
    ) {
        let page_text_tuples: Vec<(Option<u32>, &str)> = chunks
            .iter()
            .map(|c| (c.metadata.page, c.content.as_str()))
            .collect();
        let page_text = build_page_text(&page_text_tuples);

        // Convert pages_blocks to LayoutBlockRef if present
        let core_pages_blocks: Option<Vec<Vec<LayoutBlockRef<'_>>>> = pages_blocks.map(|pbs| {
            pbs.iter()
                .map(|page| {
                    page.iter()
                        .map(|b| LayoutBlockRef {
                            kind: match b.kind {
                                kb_ports::PageBlockKind::Text => "text",
                                kb_ports::PageBlockKind::Caption => "caption",
                                kb_ports::PageBlockKind::Figure => "figure",
                            },
                            id: b.id.as_deref(),
                            text: b.text.as_deref(),
                        })
                        .collect()
                })
                .collect()
        });

        let text_chunk_tuples: Vec<(u32, &str)> = chunks
            .iter()
            .filter(|c| c.metadata.chunk_type.as_deref() != Some("figure"))
            .map(|c| (c.metadata.chunk_index, c.content.as_str()))
            .collect();

        let anchors = if let Some(pbs) = &core_pages_blocks {
            resolve_figure_anchors(pbs, &text_chunk_tuples)
        } else {
            HashMap::new()
        };

        let mut fig_chunks = Vec::new();
        let mut assets = Vec::new();

        for (i, fig) in figures.iter().enumerate() {
            let n = i + 1;
            let filename = format!("fig-p{}-{n}.png", fig.page);
            let key = asset_path(Some(tenant_id), &document_id.0, &filename);

            if let Err(e) = self
                .blobs
                .upload(&key, fig.image_bytes.clone(), "image/png")
                .await
            {
                tracing::warn!("[figure-assets] upload failed for {key}, skipping figure: {e}");
                continue;
            }

            let context = context_for_page(&page_text, fig.page);
            let kind_word = figure_kind_word(&fig.kind);
            let derived = derive_label(&context);
            let caption = if let Some(cap) = fig
                .caption
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                cap.to_string()
            } else {
                let page_label = fig.page + 1;
                if let Some(der) = derived {
                    format!("{kind_word} halaman {page_label}: {der}")
                } else {
                    format!("{kind_word} halaman {page_label}")
                }
            };

            assets.push(json!({
                "assetKey": key,
                "page": fig.page,
                "caption": caption,
                "bbox": fig.bbox,
                "type": normalize_figure_type(&fig.kind),
            }));

            let context_snippet = if !context.is_empty() {
                let snippet: String = context.chars().take(500).collect();
                format!(" — Konteks: {snippet}")
            } else {
                String::new()
            };

            let anchor_chunk_index = fig.id.as_deref().and_then(|id| anchors.get(id).copied());

            fig_chunks.push(Chunk {
                content: format!("[{kind_word}] {caption}{context_snippet}"),
                metadata: ChunkMetadata {
                    document_title: document_title.to_string(),
                    category: category.to_string(),
                    subcategory: subcategory.map(str::to_string),
                    section: Some(caption),
                    heading_path: None,
                    chunk_index: 0, // will be reassigned below
                    chunk_type: Some("figure".to_string()),
                    page: Some(fig.page),
                    asset_key: Some(key),
                    anchor_chunk_index,
                    contextual_prefix: None,
                },
            });
        }

        if !fig_chunks.is_empty() {
            chunks.extend(fig_chunks);
            for (idx, c) in chunks.iter_mut().enumerate() {
                c.metadata.chunk_index = idx as u32;
            }
        }

        if !assets.is_empty() {
            let patch = json!({ "figures": assets });
            if let Err(e) = self.documents.update_metadata(document_id, &patch).await {
                tracing::warn!("[kb-ingest] Failed to update document metadata with figures: {e}");
            }
            tracing::info!(
                "[kb-ingest] Figures: {} stored for document {}",
                assets.len(),
                document_id.0
            );
        }
    }

    /// Flip the document to `ready`, emit `done` progress, and return chunk count.
    async fn finish_ready(
        &self,
        job: &JobRecord,
        document_id: &DocumentId,
        chunks: usize,
        started: Instant,
        flags: &ProgressFlags,
    ) -> Result<(usize, DocumentId), String> {
        self.documents
            .set_status(document_id, "ready")
            .await
            .map_err(|e| format!("marking document ready failed: {e}"))?;

        self.progress(job, StepProgress::of(IngestStep::Done), started, flags)
            .await;

        // A ready job carries no error text.
        self.emit_terminal(job, "ready", Some(&document_id.0), "")
            .await;

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
        self.emit_terminal(
            job,
            "failed",
            job.document_id.as_ref().map(|d| d.0.as_str()),
            reason,
        )
        .await;
    }

    /// Record progress. A failed progress write is logged and dropped: it must
    /// not abort an ingest that is otherwise fine.
    async fn progress(
        &self,
        job: &JobRecord,
        step: StepProgress,
        started: Instant,
        flags: &ProgressFlags,
    ) {
        let overall = compute_overall_progress(step, *flags);
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
        let step_name = step.step.as_str();
        if self.should_emit(&job.id, step_name) {
            let payload = json!({
                "jobId": job.id,
                "documentId": job.document_id.as_ref().map(|d| &d.0),
                "status": "processing",
                "step": step_name,
                "progress": overall,
                "stepCurrent": step.current,
                "stepTotal": step.total,
                "etaSeconds": eta,
            });
            self.emit(job, payload).await;
        }
    }

    /// Emit the one frame that ends the job's stream (`emitIngestTerminal` in
    /// `src/lib/ingest/job.ts`). Always sent, never throttled, and it also
    /// clears the throttle state so a retried job starts clean.
    async fn emit_terminal(
        &self,
        job: &JobRecord,
        status: &str,
        document_id: Option<&str>,
        error: &str,
    ) {
        if let Ok(mut state) = self.emit_state.lock() {
            state.remove(&job.id);
        }
        let ready = status == "ready";
        let payload = json!({
            "jobId": job.id,
            "documentId": document_id,
            "status": status,
            "step": if ready { Value::String("done".into()) } else { Value::Null },
            "progress": if ready { 100 } else { 0 },
            "stepCurrent": Value::Null,
            "stepTotal": Value::Null,
            "etaSeconds": 0,
            "error": if ready { Value::Null } else { Value::String(error.to_string()) },
        });
        self.emit(job, payload).await;
    }

    /// Throttle gate for progress frames: always emit on a step change,
    /// otherwise at most one per [`PROGRESS_EMIT_MIN_GAP`].
    fn should_emit(&self, job_id: &str, step: &str) -> bool {
        let Ok(mut state) = self.emit_state.lock() else {
            return true;
        };
        let now = Instant::now();
        let prev = state.insert(job_id.to_string(), (now, step.to_string()));
        !matches!(
            prev,
            Some((at, prev_step))
                if prev_step == step && now.duration_since(at) < PROGRESS_EMIT_MIN_GAP
        )
    }

    /// Push one frame to the sink, if the process wired one.
    async fn emit(&self, job: &JobRecord, payload: serde_json::Value) {
        let Some(sink) = &self.progress_sink else {
            return;
        };
        if let Err(err) = sink
            .emit(&job.tenant_id, "ingest:job:update", payload)
            .await
        {
            tracing::warn!(error = %err, "progress emit failed");
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

/// Reap stored files of failed jobs nobody retried.
pub async fn reap_failed_job_uploads(
    jobs: &dyn JobStore,
    blobs: &dyn BlobStore,
    max_age_days: u32,
    limit: u32,
) -> Result<usize, PortError> {
    let reapable = jobs.list_reapable(max_age_days, limit).await?;
    let mut reaped = 0;
    for job in reapable {
        if let Err(e) = blobs.delete(&job.s3_key).await {
            tracing::warn!(error = %e, job_id = %job.id, "reap blob delete failed");
            continue;
        }
        if let Err(e) = jobs.clear_s3_key(&job.id).await {
            tracing::warn!(error = %e, job_id = %job.id, "reap clear s3 key failed");
            continue;
        }
        reaped += 1;
    }
    Ok(reaped)
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

/// Which chunks mention which entity, for the graph write.
///
/// The extraction ran over the whole document, so it says nothing about *where*
/// in the document a name appears — and the graph needs that, because a mention is
/// the edge from an entity to the chunk a reader has to be shown. Matching is a
/// substring search of a normalised name inside a normalised chunk.
///
/// The naive shape of that is O(entities x chunks) substring searches over the
/// whole document, which on a 3,000-chunk book is 1.5 million full-text scans. So
/// there is a token index: an entity's *rarest* token narrows the search to the
/// handful of chunks that could possibly contain it, and the substring check then
/// decides. The result is identical to the naive scan and costs about a
/// millisecond.
struct MentionIndex {
    /// Chunk id and normalised text, in chunk order.
    chunks: Vec<(ChunkId, String)>,
    /// Normalised token -> the chunks it appears in.
    by_token: HashMap<String, Vec<usize>>,
}

impl MentionIndex {
    /// Index a document's chunks. `document_id` is used to mint the same chunk ids
    /// the index write will mint a few lines later, because a mention has to point
    /// at the chunk id that will actually exist in the store.
    fn build(chunks: &[Chunk], document_id: &DocumentId) -> Self {
        let mut indexed: Vec<(ChunkId, String)> = Vec::with_capacity(chunks.len());
        let mut by_token: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let id = ChunkId(format!("{}_{}", document_id.0, chunk.metadata.chunk_index));
            let normalised = normalise_for_match(&chunk.content);
            for token in normalised.split(' ') {
                if token.is_empty() {
                    continue;
                }
                let bucket = by_token.entry(token.to_string()).or_default();
                if bucket.last() != Some(&i) {
                    bucket.push(i);
                }
            }
            indexed.push((id, normalised));
        }
        Self {
            chunks: indexed,
            by_token,
        }
    }

    /// Chunk ids whose text contains `surface`, ascending by chunk order.
    ///
    /// Empty when the name cannot be normalised, when no token of it appears
    /// anywhere, or when the name is a substring of some other word. That last one
    /// is deliberate: "Bank" would otherwise match "Bankruptcy" and attach a
    /// mention to every chunk that happens to discuss bankruptcy.
    fn chunks_for(&self, surface: &str) -> Vec<ChunkId> {
        let needle = normalise_for_match(surface);
        let needle = needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        self.search(needle)
    }

    /// The first chunk that mentions both names, if one does.
    ///
    /// A relation's provenance is the chunk that says it, so both endpoints have
    /// to be in that chunk. A relation extracted from a summary with no single
    /// supporting chunk gets `None`, which the port allows: it is a real edge with
    /// no one place to point at.
    fn chunk_for_both(&self, a: &str, b: &str) -> Option<ChunkId> {
        let left = normalise_for_match(a);
        let left = left.trim();
        let right = normalise_for_match(b);
        let right = right.trim();
        if left.is_empty() || right.is_empty() {
            return None;
        }
        let mut both = self.search(left);
        both.retain(|id| {
            self.chunks
                .iter()
                .any(|(other, text)| other == id && text.contains(right))
        });
        both.into_iter().next()
    }

    /// Chunk ids whose text contains `needle`, in chunk order.
    fn search(&self, needle: &str) -> Vec<ChunkId> {
        let tokens: Vec<&str> = needle.split(' ').filter(|t| !t.is_empty()).collect();
        let Some(anchor) = tokens
            .iter()
            .filter_map(|t| self.by_token.get(*t))
            .min_by_key(|bucket| bucket.len())
        else {
            return Vec::new();
        };
        let mut out: Vec<(usize, ChunkId)> = Vec::new();
        for i in anchor {
            if self.chunks[*i].1.contains(needle) {
                out.push((*i, self.chunks[*i].0.clone()));
            }
        }
        out.sort();
        out.dedup_by(|a, b| a.0 == b.0);
        out.into_iter().map(|(_, id)| id).collect()
    }
}

/// Case- and diacritic-insensitive text, for the mention substring match.
fn normalise_for_match(text: &str) -> String {
    kb_core::key_of(text).unwrap_or_default()
}

/// Map extracted relations onto the graph store's shape.
///
/// Two filters, both load-bearing:
///
/// * a relation whose endpoints were not written as entities is dropped, because
///   `relate` rejects a batch containing an unknown endpoint — keeping it would
///   cost every other relation in the batch;
/// * a relation with a non-positive weight is dropped, because in
///   [`kb_core::graph`] a zero weight is an absent edge and storing it would be a
///   row that walks over.
fn graph_relations(
    relations: &[kb_core::Relation],
    keys: &HashMap<String, String>,
    mentions: &MentionIndex,
) -> Vec<RelationUpsert> {
    relations
        .iter()
        .filter_map(|relation| {
            let src = relation_name(relation, "source_entity")?;
            let dst = relation_name(relation, "target_entity")?;
            let src_key = keys.get(&src.to_lowercase())?.clone();
            let dst_key = keys.get(&dst.to_lowercase())?.clone();
            // `is_sign_positive` rather than `> 0.0`: a NaN confidence is not a
            // positive confidence, and `NaN > 0.0` is false anyway, so both
            // spellings agree — this one says which of them is doing the work.
            // Zero is dropped and so is anything not finite. `is_sign_positive`
            // is the trap here: it reads the IEEE sign *bit*, so `+0.0` is
            // "positive" and a zero-weight relation would be stored as an edge
            // that `kb_core::graph` then treats as absent.
            if src_key == dst_key || !relation.confidence.is_finite() || relation.confidence <= 0.0
            {
                return None;
            }
            let label = relation.relation_type.trim();
            let relation_type = if label.is_empty() {
                "RELATED_TO".to_string()
            } else {
                label.to_string()
            };
            Some(RelationUpsert {
                src_key,
                dst_key,
                relation_type,
                weight: relation.confidence,
                chunk_id: mentions.chunk_for_both(&src, &dst),
            })
        })
        .collect()
}

/// An endpoint name from the extractor's own fields, falling back to the metadata
/// the pattern and LLM extractors both write.
fn relation_name(relation: &kb_core::Relation, field: &str) -> Option<String> {
    let direct = match field {
        "source_entity" => relation.source_entity.as_str(),
        _ => relation.target_entity.as_str(),
    };
    let _ = field;
    if !direct.trim().is_empty() {
        return Some(direct.trim().to_string());
    }
    relation
        .metadata
        .as_ref()
        .and_then(|m| m.get(field))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Title for the embedding prefix and the chunk metadata: the job's explicit
/// title, else the document's own H1 (markdown and text files usually start
/// with one), else the filename without its extension.
fn job_title(params: &Value, filename: &str, content: &str, markdown: bool) -> String {
    kb_core::title::document_title(
        params.get("title").and_then(Value::as_str),
        filename,
        content,
        markdown,
    )
}

/// Whether an extracted document's text is markdown (or plain text read as
/// markdown), which is what may carry an H1 to use as the title.
///
/// The extractors report `"markdown"` for `.md`, `.txt` and the other text
/// formats, and `"pdf"`/`"image"` for the rest.
fn is_markdown_text(file_type: &str) -> bool {
    file_type.eq_ignore_ascii_case("markdown")
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
    if let Some(page) = meta.page {
        map.insert("page".to_string(), json!(page));
    }
    if let Some(asset_key) = &meta.asset_key {
        map.insert("assetKey".to_string(), json!(asset_key));
    }
    if let Some(anchor) = meta.anchor_chunk_index {
        map.insert("anchorChunkIndex".to_string(), json!(anchor));
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
            heading_path: None,
            chunk_index: 4,
            contextual_prefix: None,
            chunk_type: Some("text".to_string()),
            page: None,
            asset_key: None,
            anchor_chunk_index: None,
        }
    }

    #[test]
    fn title_falls_back_to_the_filename_stem() {
        assert_eq!(job_title(&json!({}), "report.pdf", "", false), "report");
        assert_eq!(
            job_title(&json!({}), "archive.tar.gz", "", false),
            "archive.tar"
        );
        assert_eq!(
            job_title(&json!({}), "noextension", "", false),
            "noextension"
        );
        assert_eq!(
            job_title(&json!({"title": "Given"}), "report.pdf", "", false),
            "Given"
        );
        assert_eq!(
            job_title(&json!({"title": "  "}), "report.pdf", "", false),
            "report"
        );
    }

    #[test]
    fn a_markdown_document_without_a_title_lends_its_h1_to_the_title() {
        // The case that made every MIRACL passage embed as "1000580_1\n\n…".
        assert_eq!(
            job_title(&json!({}), "1000580_1.md", "# DPTools\n\nbody", true),
            "DPTools"
        );
        // A PDF's extracted text is not markdown, so it keeps the stem.
        assert_eq!(
            job_title(&json!({}), "report.pdf", "# Heading\n\nbody", false),
            "report"
        );
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
        assert!(stored.get("assetKey").is_none());
        assert!(stored.get("anchorChunkIndex").is_none());
    }

    #[test]
    fn stored_metadata_includes_page_and_figures_when_present() {
        let mut m = meta();
        m.page = Some(7);
        m.asset_key = Some("docs/asset.png".to_string());
        m.anchor_chunk_index = Some(2);
        let stored = storage_metadata(&m);
        assert_eq!(stored["page"], json!(7));
        assert_eq!(stored["assetKey"], json!("docs/asset.png"));
        assert_eq!(stored["anchorChunkIndex"], json!(2));
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
