//! Stateless document processing pipeline.
//!
//! Runs the same extraction, furniture stripping, smart chunking, quality
//! assessment, table structuring, figure anchoring, BM25 sparse encoding,
//! optional entity/relation extraction, and optional dense embedding as
//! [`crate::IngestWorker`] — without touching Postgres, Qdrant, or S3.
//!
//! Designed for callers that own their own database and Qdrant server and want
//! `rantai-kb` strictly as a document-processing microservice.

use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use kb_core::chunk::{Chunk, ChunkMetadata, SmartChunkingOptions, smart_chunk_document};
use kb_core::figure_assets::{
    build_page_text, context_for_page, derive_label, figure_kind_word, normalize_figure_type,
};
use kb_core::policy::{FigureMode, resolve_ingest_policy};
use kb_ports::{
    ChunkId, Embedder, Entity, EntityExtractor, ExtractionOptions, Extractor, OcrReport, PageText,
    PortError, Relation, RerankCandidate, Reranker, SeatRelease,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ingest::{
    INGEST_CHUNKING, TABLE_ROW_UNIT_MAX_ROWS, assess_document_quality, figure_anchors,
    is_markdown_text, table_row_content, table_summary_content,
};
use crate::seats::{SeatCapacity, SeatPriority, StageSeats};

/// Tunables for [`DocumentProcessor`].
#[derive(Debug, Clone)]
pub struct ProcessorSettings {
    /// Default chunking settings (overridable per request).
    pub chunking: SmartChunkingOptions,
    /// Strip running headers, footers and page numbers before chunking.
    pub strip_page_furniture: bool,
    /// Model identifier echoed on the response when dense embeddings run.
    pub embedding_model: Option<String>,
    /// Characters of each chunk's embedding text sent to the embedder; 0 = all.
    pub embedding_max_chars: usize,
    /// Stage concurrency seats (`KB_SEATS`), shared across concurrent requests.
    pub seats: SeatCapacity,
    /// Optional extraction quality gate (`KB_EXTRACT_QUALITY_GATE`).
    pub quality: Option<kb_core::quality::QualityOptions>,
}

impl Default for ProcessorSettings {
    fn default() -> Self {
        Self {
            chunking: INGEST_CHUNKING,
            strip_page_furniture: false,
            embedding_model: None,
            embedding_max_chars: 0,
            seats: SeatCapacity::default(),
            quality: None,
        }
    }
}

/// Input to one stateless document processing call.
#[derive(Debug, Clone)]
pub struct ProcessRequest {
    pub filename: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub title: Option<String>,
    pub category: Option<String>,
    pub subcategory: Option<String>,
    pub figure_mode: FigureMode,
    pub document_type: Option<String>,
    /// Compute dense embeddings via the configured embedder.
    /// `None` defaults to `true` when an embedder is configured, `false` otherwise.
    pub embed: Option<bool>,
    /// Compute Qdrant-compatible BM25 sparse vectors for every chunk (default `true`).
    pub sparse: bool,
    /// Emit structured tables and `table_summary` / `table_row` chunks (default `true`).
    pub tables: bool,
    /// Run entity and relation extraction (default `false` — opt-in to avoid unexpected spend).
    pub entities: bool,
    /// Include base64-encoded PNG bytes on each extracted figure (default `false`).
    pub include_figure_bytes: bool,
    /// Optional per-request chunk size override (characters, floor 100).
    pub chunk_size: Option<u32>,
    /// Optional per-request chunk overlap override (characters).
    pub chunk_overlap: Option<u32>,
}

impl ProcessRequest {
    /// Minimal request for a file with sensible defaults.
    #[must_use]
    pub fn new(filename: impl Into<String>, mime_type: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            filename: filename.into(),
            mime_type: mime_type.into(),
            bytes,
            title: None,
            category: None,
            subcategory: None,
            figure_mode: FigureMode::Auto,
            document_type: None,
            embed: None,
            sparse: true,
            tables: true,
            entities: false,
            include_figure_bytes: false,
            chunk_size: None,
            chunk_overlap: None,
        }
    }
}

/// Failures from [`DocumentProcessor`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProcessError {
    /// Caller asked for something the request or configuration cannot satisfy (HTTP 400).
    #[error("{0}")]
    BadRequest(String),
    /// The extractor could not read this file (HTTP 422).
    #[error("{0}")]
    ExtractionFailed(String),
    /// The extraction quality gate flagged the document as needing OCR (HTTP 422).
    #[error("{0}")]
    NeedsOcr(String),
    /// An upstream model/OCR backend is currently unavailable (HTTP 503).
    #[error("{0}")]
    Unavailable(String),
    /// Unexpected internal processing failure (HTTP 500).
    #[error("{0}")]
    Internal(String),
}

/// Qdrant-compatible sparse vector (sorted ascending by FNV-1a term id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SparseVectorData {
    pub indices: Vec<u32>,
    pub values: Vec<f32>,
}

impl From<kb_core::sparse::SparseVector> for SparseVectorData {
    fn from(v: kb_core::sparse::SparseVector) -> Self {
        Self {
            indices: v.indices,
            values: v.values,
        }
    }
}

/// One processed chunk ready for external storage and Qdrant upsert.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessedChunk {
    pub chunk_index: u32,
    pub content: String,
    pub chunk_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcategory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_chunk_index: Option<u32>,
    /// Index into [`ProcessOutput::figures`] when `chunk_type == "figure"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub figure_index: Option<u32>,
    /// Index into [`ProcessOutput::tables`] when `chunk_type` is `table_summary` or `table_row`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sparse_vector: Option<SparseVectorData>,
}

/// One extracted structured table with its inferred typed manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessedTable {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<kb_core::tables::TableSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_span: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub header: Vec<String>,
    pub rows: Vec<Vec<kb_core::tables::Cell>>,
    pub manifest: kb_core::tables::TableManifest,
}

/// One extracted figure with its caption, bounding box, text anchor, and optional PNG bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessedFigure {
    pub index: u32,
    pub page: u32,
    pub kind: String,
    pub caption: String,
    pub bbox: [f32; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_chunk_index: Option<u32>,
    /// Base64-encoded PNG image, present only when `include_figure_bytes` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub png_base64: Option<String>,
}

/// Wall-clock stage breakdown in milliseconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessTimings {
    pub extract_ms: u64,
    pub chunk_ms: u64,
    pub tables_ms: u64,
    pub figures_ms: u64,
    pub entities_ms: u64,
    pub embed_ms: u64,
    pub total_ms: u64,
}

/// Full output of stateless document processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessOutput {
    pub title: String,
    pub file_type: String,
    pub sha256: String,
    pub file_size: usize,
    pub content: String,
    pub page_map: Vec<PageText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<kb_core::tree::Outline>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr: Option<OcrReport>,
    pub quality: serde_json::Value,
    pub degrades: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    pub chunks: Vec<ProcessedChunk>,
    pub tables: Vec<ProcessedTable>,
    pub figures: Vec<ProcessedFigure>,
    pub entities: Vec<Entity>,
    pub relations: Vec<Relation>,
    pub timings: ProcessTimings,
}

/// How [`DocumentProcessor::encode`] encodes input strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncodeMode {
    #[default]
    Query,
    Passage,
}

/// Output of [`DocumentProcessor::encode`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeOutput {
    pub mode: EncodeMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeddings: Option<Vec<Vec<f32>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sparse_vectors: Option<Vec<SparseVectorData>>,
}

/// One scored document returned by [`DocumentProcessor::rerank`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankItem {
    /// Zero-based index into the caller's input `documents` array.
    pub index: u32,
    /// Relevance score returned by the reranker (higher is more relevant).
    pub score: f32,
    /// Zero-based rank in the returned ordering.
    pub rank: u32,
    /// Document text, echoed back only when `return_documents` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
}

/// Output of [`DocumentProcessor::rerank`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerankOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub results: Vec<RerankItem>,
}

/// Capabilities currently wired on a [`DocumentProcessor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessorCapabilities {
    pub embed: bool,
    pub rerank: bool,
    pub entities: bool,
    pub sparse: bool,
    pub tables: bool,
    pub figures: bool,
}

/// Stateless document processor. Holds ports, names no adapter, writes to no store.
#[derive(Clone)]
pub struct DocumentProcessor {
    extractor: Arc<dyn Extractor>,
    embedder: Option<Arc<dyn Embedder>>,
    reranker: Option<Arc<dyn Reranker>>,
    rerank_model: Option<String>,
    entity_extractor: Option<Arc<dyn EntityExtractor>>,
    seats: StageSeats,
    settings: ProcessorSettings,
}

impl DocumentProcessor {
    /// Build a processor backed only by an extractor (no embedder, reranker, or entity extractor).
    #[must_use]
    pub fn new(extractor: Arc<dyn Extractor>, settings: ProcessorSettings) -> Self {
        let seats = settings.seats.seats();
        Self {
            extractor,
            embedder: None,
            reranker: None,
            rerank_model: None,
            entity_extractor: None,
            seats,
            settings,
        }
    }

    /// Attach a dense embedder.
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Attach a reranker and optional model identifier echoed on responses.
    #[must_use]
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>, model: Option<String>) -> Self {
        self.reranker = Some(reranker);
        self.rerank_model = model;
        self
    }

    /// Attach an entity/relation extractor.
    #[must_use]
    pub fn with_entity_extractor(mut self, entity_extractor: Arc<dyn EntityExtractor>) -> Self {
        self.entity_extractor = Some(entity_extractor);
        self
    }

    /// Which optional stages are wired on this processor.
    #[must_use]
    pub fn capabilities(&self) -> ProcessorCapabilities {
        ProcessorCapabilities {
            embed: self.embedder.is_some(),
            rerank: self.reranker.is_some(),
            entities: self.entity_extractor.is_some(),
            sparse: true,
            tables: true,
            figures: true,
        }
    }

    /// The configured dense embedding model name, when an embedder is wired.
    #[must_use]
    pub fn embedding_model(&self) -> Option<&str> {
        self.embedder
            .as_ref()
            .and(self.settings.embedding_model.as_deref())
    }

    /// The configured reranker model name, when a reranker is wired.
    #[must_use]
    pub fn rerank_model(&self) -> Option<&str> {
        self.reranker.as_ref().and(self.rerank_model.as_deref())
    }

    /// Process one uploaded document end-to-end in memory and return structured results.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] for invalid options, extraction/OCR failures,
    /// quality-gate refusal, or unavailable upstream backends.
    pub async fn process(&self, req: ProcessRequest) -> Result<ProcessOutput, ProcessError> {
        let total_started = Instant::now();
        let should_embed = match req.embed {
            Some(true) if self.embedder.is_none() => {
                return Err(ProcessError::BadRequest(
                    "embed=true was requested, but no embedder is configured \
                     (set KB_EMBEDDING_BASE_URL / KB_EMBEDDING_API_KEY or KB_TIER)"
                        .to_string(),
                ));
            }
            Some(flag) => flag,
            None => self.embedder.is_some(),
        };

        let priority = next_priority();
        let policy = resolve_ingest_policy(&req.filename, req.figure_mode);
        let sha256 = hex::encode(Sha256::digest(&req.bytes));
        let file_size = req.bytes.len();

        // ── 1. Extract ───────────────────────────────────────────────────────
        let extract_started = Instant::now();
        let seat = SeatRelease::holding(self.seats.extract.acquire(priority.clone()).await);
        let options = ExtractionOptions {
            figures: policy.figures,
            force_layout: policy.force_layout,
            document_type: req.document_type.clone(),
            seat: seat.clone(),
        };
        let extraction = self
            .extractor
            .extract_with_options(&req.filename, &req.mime_type, &req.bytes, options)
            .await;
        seat.release();

        let extraction = match extraction {
            Ok(result) => result,
            Err(PortError::Unavailable(reason)) => return Err(ProcessError::Unavailable(reason)),
            Err(other) => return Err(ProcessError::ExtractionFailed(other.to_string())),
        };
        if let Some(reason) = &extraction.error {
            return Err(ProcessError::ExtractionFailed(reason.clone()));
        }

        let content: String = extraction.content.chars().filter(|c| *c != '\0').collect();
        let title = kb_core::title::document_title(
            req.title.as_deref(),
            &req.filename,
            &content,
            is_markdown_text(&extraction.file_type),
        );

        let mut outline = extraction.outline.clone();
        if outline.is_none() {
            let pages: Vec<String> = extraction
                .page_map
                .as_ref()
                .map(|map| {
                    let len = map.iter().map(|p| p.page as usize + 1).max().unwrap_or(0);
                    let mut pages = vec![String::new(); len];
                    for p in map {
                        pages[p.page as usize].clone_from(&p.text);
                    }
                    pages
                })
                .unwrap_or_default();
            let outline_mime = if extraction.file_type.eq_ignore_ascii_case("pdf") {
                "application/pdf"
            } else {
                &req.mime_type
            };
            outline = self
                .extractor
                .outline(outline_mime, &req.bytes, &pages)
                .await;
        }
        if let Some(ref mut o) = outline {
            o.document_title = Some(title.clone());
        }
        let extract_ms = elapsed_ms(extract_started);

        // ── 2. Chunk & Quality Gate ──────────────────────────────────────────
        let chunk_started = Instant::now();
        let category = req.category.as_deref().unwrap_or_default();
        let subcategory = req.subcategory.as_deref();

        let mut chunking = self.settings.chunking;
        if let Some(size) = req.chunk_size {
            chunking.max_chunk_size = Some(size.max(100));
        }
        if let Some(overlap) = req.chunk_overlap {
            chunking.overlap_size = Some(overlap);
        }

        let page_texts: Vec<(u32, String)> = extraction
            .page_map
            .iter()
            .flatten()
            .map(|p| (p.page, p.text.chars().filter(|c| *c != '\0').collect()))
            .collect();

        let (content_for_chunks, cleaned_pages) = if self.settings.strip_page_furniture
            && kb_core::chunk::content_is_pages(&content, &page_texts)
        {
            let (cleaned, _report) = kb_core::furniture::strip_page_furniture(
                &page_texts,
                kb_core::furniture::FurnitureOptions::default(),
            );
            let joined = cleaned
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            (std::borrow::Cow::Owned(joined), cleaned)
        } else {
            (std::borrow::Cow::Borrowed(content.as_str()), page_texts)
        };

        let mut chunks = kb_core::chunk::smart_chunk_pages(
            &content_for_chunks,
            &cleaned_pages,
            &title,
            category,
            subcategory,
            chunking,
        )
        .unwrap_or_else(|| {
            smart_chunk_document(&content_for_chunks, &title, category, subcategory, chunking)
        });

        let quality = assess_document_quality(
            self.settings.quality,
            &extraction,
            &content_for_chunks,
            chunks.len(),
        );
        if self.settings.quality.is_some() && quality.verdict.needs_ocr {
            return Err(ProcessError::NeedsOcr(format!(
                "the document needs OCR: {}",
                quality.verdict.reasons.join("; ")
            )));
        }

        // Page map assignment for non-paged chunks
        if let Some(map) = &extraction.page_map
            && !map.is_empty()
        {
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
        let chunk_ms = elapsed_ms(chunk_started);

        // Track chunk -> table_index and chunk -> figure_index without abusing storage keys.
        let mut chunk_table_indices: Vec<Option<u32>> = vec![None; chunks.len()];
        let mut chunk_figure_indices: Vec<Option<u32>> = vec![None; chunks.len()];

        // ── 3. Structured tables ─────────────────────────────────────────────
        let tables_started = Instant::now();
        let mut tables_out: Vec<ProcessedTable> = Vec::new();
        if req.tables && !extraction.tables.is_empty() {
            let mut next_index = chunks
                .iter()
                .map(|c| c.metadata.chunk_index)
                .max()
                .map_or(0, |last| last.saturating_add(1));

            for (i, grid) in extraction.tables.iter().enumerate() {
                let table_idx = u32::try_from(i).unwrap_or(u32::MAX);
                let manifest = kb_core::tables::build_manifest(grid);
                let section = grid
                    .title
                    .clone()
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| format!("Table {}", table_idx + 1));

                chunks.push(Chunk {
                    content: table_summary_content(grid, &manifest),
                    metadata: ChunkMetadata {
                        document_title: title.clone(),
                        category: category.to_string(),
                        subcategory: subcategory.map(str::to_string),
                        section: Some(section.clone()),
                        heading_path: None,
                        chunk_index: next_index,
                        contextual_prefix: None,
                        chunk_type: Some("table_summary".to_string()),
                        page: grid.page_span.map(|(first, _)| first),
                        asset_key: None,
                        anchor_chunk_index: None,
                        table_id: None,
                        ..ChunkMetadata::default()
                    },
                });
                chunk_table_indices.push(Some(table_idx));
                chunk_figure_indices.push(None);
                next_index = next_index.saturating_add(1);

                if grid.rows.len() <= TABLE_ROW_UNIT_MAX_ROWS {
                    for (row_index, row) in grid.rows.iter().enumerate() {
                        let row_text = table_row_content(&grid.header, row);
                        if row_text.is_empty() {
                            continue;
                        }
                        chunks.push(Chunk {
                            content: row_text,
                            metadata: ChunkMetadata {
                                document_title: title.clone(),
                                category: category.to_string(),
                                subcategory: subcategory.map(str::to_string),
                                section: Some(format!("{section} — row {}", row_index + 1)),
                                heading_path: None,
                                chunk_index: next_index,
                                contextual_prefix: None,
                                chunk_type: Some("table_row".to_string()),
                                page: grid.page_span.map(|(first, _)| first),
                                asset_key: None,
                                anchor_chunk_index: None,
                                table_id: None,
                                ..ChunkMetadata::default()
                            },
                        });
                        chunk_table_indices.push(Some(table_idx));
                        chunk_figure_indices.push(None);
                        next_index = next_index.saturating_add(1);
                    }
                }

                tables_out.push(ProcessedTable {
                    index: table_idx,
                    source: grid.source,
                    sheet: grid.sheet.clone(),
                    page_span: grid.page_span,
                    title: grid.title.clone(),
                    caption: grid.caption.clone(),
                    header: grid.header.clone(),
                    rows: grid.rows.clone(),
                    manifest,
                });
            }
        }
        let tables_ms = elapsed_ms(tables_started);

        // ── 4. Figures (stateless: anchor + caption + optional base64 PNG) ──
        let figures_started = Instant::now();
        let mut figures_out: Vec<ProcessedFigure> = Vec::new();
        if !extraction.figures.is_empty() {
            let page_text_tuples: Vec<(Option<u32>, &str)> = chunks
                .iter()
                .map(|c| (c.metadata.page, c.content.as_str()))
                .collect();
            let page_text = build_page_text(&page_text_tuples);
            let anchors = figure_anchors(extraction.pages_blocks.as_deref(), &chunks);

            for (i, fig) in extraction.figures.iter().enumerate() {
                let fig_idx = u32::try_from(i).unwrap_or(u32::MAX);
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
                let context_snippet = if !context.is_empty() {
                    let snippet: String = context.chars().take(500).collect();
                    format!(" — Konteks: {snippet}")
                } else {
                    String::new()
                };
                let anchor_chunk_index = fig.id.as_deref().and_then(|id| anchors.get(id).copied());

                figures_out.push(ProcessedFigure {
                    index: fig_idx,
                    page: fig.page,
                    kind: normalize_figure_type(&fig.kind).to_string(),
                    caption: caption.clone(),
                    bbox: fig.bbox,
                    anchor_chunk_index,
                    png_base64: if req.include_figure_bytes {
                        Some(base64::engine::general_purpose::STANDARD.encode(&fig.image_bytes))
                    } else {
                        None
                    },
                });

                chunks.push(Chunk {
                    content: format!("[{kind_word}] {caption}{context_snippet}"),
                    metadata: ChunkMetadata {
                        document_title: title.clone(),
                        category: category.to_string(),
                        subcategory: subcategory.map(str::to_string),
                        section: Some(caption),
                        heading_path: None,
                        chunk_index: 0,
                        chunk_type: Some("figure".to_string()),
                        page: Some(fig.page),
                        asset_key: None,
                        anchor_chunk_index,
                        contextual_prefix: None,
                        table_id: None,
                        ..ChunkMetadata::default()
                    },
                });
                chunk_table_indices.push(None);
                chunk_figure_indices.push(Some(fig_idx));
            }
        }
        // Normalise chunk_index to contiguous 0..N across text, table, and figure chunks.
        for (idx, chunk) in chunks.iter_mut().enumerate() {
            chunk.metadata.chunk_index = u32::try_from(idx).unwrap_or(u32::MAX);
        }
        let figures_ms = elapsed_ms(figures_started);

        // ── 5. Optional entity / relation extraction ─────────────────────────
        let entities_started = Instant::now();
        let mut degrades = quality.degrades;
        let (entities, relations) = if req.entities && policy.entities {
            if let Some(extractor) = &self.entity_extractor {
                match extractor
                    .extract_entities_and_relations(&content, None, None)
                    .await
                {
                    Ok(pair) => pair,
                    Err(err) => {
                        tracing::warn!(error = %err, "stateless entity extraction failed (non-fatal)");
                        degrades.push(format!("entities: {err}"));
                        (Vec::new(), Vec::new())
                    }
                }
            } else {
                let pat = kb_core::intelligence::PatternEntityExtractor::default();
                let ents = pat.extract(&content, None, None);
                let rel_pat = kb_core::intelligence::PatternRelationExtractor::default();
                let rels = rel_pat.extract(&content, &ents, None, None);
                (ents, rels)
            }
        } else {
            (Vec::new(), Vec::new())
        };
        let entities_ms = elapsed_ms(entities_started);

        // ── 6. Optional dense embeddings ─────────────────────────────────────
        let embed_started = Instant::now();
        let mut dense_vectors: Option<Vec<Vec<f32>>> = None;
        let mut embedding_model = None;
        if should_embed && !chunks.is_empty() {
            let Some(embedder) = &self.embedder else {
                return Err(ProcessError::BadRequest(
                    "no embedder is configured".to_string(),
                ));
            };
            let max_chars = self.settings.embedding_max_chars;
            let texts: Vec<String> = chunks
                .iter()
                .map(|c| {
                    let text = kb_core::title::embedding_text(
                        &title,
                        c.metadata.heading_path.as_deref(),
                        &c.content,
                    );
                    if max_chars > 0 && text.chars().count() > max_chars {
                        text.chars().take(max_chars).collect()
                    } else {
                        text
                    }
                })
                .collect();

            let vectors = {
                let _seat = self.seats.embed.acquire(priority.clone()).await;
                embedder.embed_passages(&texts).await
            };
            let vectors = match vectors {
                Ok(v) => v,
                Err(PortError::Unavailable(reason)) => {
                    return Err(ProcessError::Unavailable(format!(
                        "embedding failed: {reason}"
                    )));
                }
                Err(other) => {
                    return Err(ProcessError::Internal(format!("embedding failed: {other}")));
                }
            };
            if vectors.len() != chunks.len() {
                return Err(ProcessError::Internal(format!(
                    "embedding batch returned {} vectors for {} chunks",
                    vectors.len(),
                    chunks.len()
                )));
            }
            dense_vectors = Some(vectors);
            embedding_model = self.settings.embedding_model.clone();
        }
        let embed_ms = elapsed_ms(embed_started);

        // ── 7. Assemble chunks (+ BM25 sparse vectors) ───────────────────────
        let mut dense_iter = dense_vectors.map(Vec::into_iter);
        let processed_chunks: Vec<ProcessedChunk> = chunks
            .into_iter()
            .zip(chunk_table_indices)
            .zip(chunk_figure_indices)
            .map(|((chunk, table_index), figure_index)| {
                let sparse_vector = if req.sparse {
                    Some(SparseVectorData::from(kb_core::sparse::encode_document(
                        &chunk.content,
                    )))
                } else {
                    None
                };
                let embedding = dense_iter.as_mut().and_then(Iterator::next);
                let chunk_type = chunk
                    .metadata
                    .chunk_type
                    .unwrap_or_else(|| "text".to_string());
                ProcessedChunk {
                    chunk_index: chunk.metadata.chunk_index,
                    content: chunk.content,
                    chunk_type,
                    section: chunk.metadata.section,
                    heading_path: chunk.metadata.heading_path,
                    page: chunk.metadata.page,
                    category: chunk.metadata.category,
                    subcategory: chunk.metadata.subcategory,
                    anchor_chunk_index: chunk.metadata.anchor_chunk_index,
                    figure_index,
                    table_index,
                    embedding,
                    sparse_vector,
                }
            })
            .collect();

        let page_map = extraction.page_map.unwrap_or_default();

        Ok(ProcessOutput {
            title,
            file_type: extraction.file_type,
            sha256,
            file_size,
            content,
            page_map,
            outline,
            ocr: extraction.ocr,
            quality: quality.verdict.metadata(),
            degrades,
            embedding_model,
            chunks: processed_chunks,
            tables: tables_out,
            figures: figures_out,
            entities,
            relations,
            timings: ProcessTimings {
                extract_ms,
                chunk_ms,
                tables_ms,
                figures_ms,
                entities_ms,
                embed_ms,
                total_ms: elapsed_ms(total_started),
            },
        })
    }

    /// Encode text strings for search queries or passages (dense and/or BM25 sparse).
    ///
    /// Allows an external service that owns Qdrant to encode queries using the exact
    /// same dense prefixes and FNV-1a BM25 tokenizer that [`Self::process`] uses.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::BadRequest`] if dense encoding is requested without a
    /// configured embedder, or [`ProcessError::Unavailable`] / [`ProcessError::Internal`]
    /// if the embedder fails.
    pub async fn encode(
        &self,
        texts: &[String],
        mode: EncodeMode,
        dense: Option<bool>,
        sparse: bool,
    ) -> Result<EncodeOutput, ProcessError> {
        let should_dense = match dense {
            Some(true) if self.embedder.is_none() => {
                return Err(ProcessError::BadRequest(
                    "dense=true was requested, but no embedder is configured \
                     (set KB_EMBEDDING_BASE_URL / KB_EMBEDDING_API_KEY or KB_TIER)"
                        .to_string(),
                ));
            }
            Some(flag) => flag,
            None => self.embedder.is_some(),
        };

        let sparse_vectors = if sparse {
            Some(
                texts
                    .iter()
                    .map(|text| match mode {
                        EncodeMode::Query => {
                            SparseVectorData::from(kb_core::sparse::encode_query(text))
                        }
                        EncodeMode::Passage => {
                            SparseVectorData::from(kb_core::sparse::encode_document(text))
                        }
                    })
                    .collect(),
            )
        } else {
            None
        };

        let (embeddings, embedding_model) = if should_dense && !texts.is_empty() {
            let Some(embedder) = &self.embedder else {
                return Err(ProcessError::BadRequest(
                    "no embedder is configured".to_string(),
                ));
            };
            let vectors = match mode {
                // Live search queries take a fast lane and do not wait behind bulk
                // document ingestion holding `seats.embed` (same invariant as stateful
                // `SearchService`).
                EncodeMode::Query => {
                    let mut out = Vec::with_capacity(texts.len());
                    for text in texts {
                        match embedder.embed_query(text).await {
                            Ok(vec) => out.push(vec),
                            Err(PortError::Unavailable(reason)) => {
                                return Err(ProcessError::Unavailable(format!(
                                    "embedding failed: {reason}"
                                )));
                            }
                            Err(other) => {
                                return Err(ProcessError::Internal(format!(
                                    "embedding failed: {other}"
                                )));
                            }
                        }
                    }
                    out
                }
                EncodeMode::Passage => {
                    let priority = next_priority();
                    let _seat = self.seats.embed.acquire(priority).await;
                    match embedder.embed_passages(texts).await {
                        Ok(v) => v,
                        Err(PortError::Unavailable(reason)) => {
                            return Err(ProcessError::Unavailable(format!(
                                "embedding failed: {reason}"
                            )));
                        }
                        Err(other) => {
                            return Err(ProcessError::Internal(format!(
                                "embedding failed: {other}"
                            )));
                        }
                    }
                }
            };
            (Some(vectors), self.settings.embedding_model.clone())
        } else if should_dense {
            (Some(Vec::new()), self.settings.embedding_model.clone())
        } else {
            (None, None)
        };

        Ok(EncodeOutput {
            mode,
            embedding_model,
            embeddings,
            sparse_vectors,
        })
    }

    /// Rerank candidate passages against a query using the configured reranker
    /// (local TEI/vLLM/llama.cpp or cloud Cohere/Voyage/OpenRouter).
    ///
    /// Runs on the search fast lane without waiting behind bulk ingest seats.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::BadRequest`] when no reranker is configured or inputs
    /// are empty/invalid, [`ProcessError::Unavailable`] when the reranker backend is
    /// unreachable, or [`ProcessError::Internal`] on unexpected reranker failures.
    pub async fn rerank(
        &self,
        query: &str,
        documents: &[String],
        top_k: Option<u32>,
        return_documents: bool,
    ) -> Result<RerankOutput, ProcessError> {
        let Some(reranker) = &self.reranker else {
            return Err(ProcessError::BadRequest(
                "no reranker is configured (set KB_RERANK_ENABLED=true and KB_RERANK_PROVIDER, \
                 or KB_TIER=cloud/desk/studio/station)"
                    .to_string(),
            ));
        };
        if query.trim().is_empty() {
            return Err(ProcessError::BadRequest(
                "query must not be empty".to_string(),
            ));
        }
        if documents.is_empty() {
            return Err(ProcessError::BadRequest(
                "documents must not be empty".to_string(),
            ));
        }
        if matches!(top_k, Some(0)) {
            return Err(ProcessError::BadRequest(
                "topK must be at least 1".to_string(),
            ));
        }
        let doc_count = u32::try_from(documents.len()).unwrap_or(u32::MAX);
        let final_k = top_k.map_or(doc_count, |k| k.min(doc_count)).max(1);

        let candidates: Vec<RerankCandidate> = documents
            .iter()
            .enumerate()
            .map(|(idx, text)| RerankCandidate {
                id: ChunkId(idx.to_string()),
                text: text.clone(),
                original_rank: u32::try_from(idx).unwrap_or(u32::MAX),
                original_score: 0.0,
            })
            .collect();

        let ranked = match reranker.rerank(query, candidates, final_k).await {
            Ok(results) => results,
            Err(PortError::Unavailable(reason)) => {
                return Err(ProcessError::Unavailable(format!(
                    "rerank failed: {reason}"
                )));
            }
            Err(other) => {
                return Err(ProcessError::Internal(format!("rerank failed: {other}")));
            }
        };

        let mut results = Vec::with_capacity(ranked.len());
        for (rank_idx, item) in ranked.into_iter().enumerate() {
            let Ok(doc_idx) = item.id.as_str().parse::<usize>() else {
                continue;
            };
            let Some(text) = documents.get(doc_idx) else {
                continue;
            };
            results.push(RerankItem {
                index: u32::try_from(doc_idx).unwrap_or(u32::MAX),
                score: item.score,
                rank: u32::try_from(rank_idx).unwrap_or(u32::MAX),
                document: return_documents.then(|| text.clone()),
            });
        }

        Ok(RerankOutput {
            model: self.rerank_model.clone(),
            results,
        })
    }
}

fn next_priority() -> SeatPriority {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok());
    SeatPriority::new(now_ms, format!("stateless-{seq:016x}"))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
