//! The visual stage of ingest (`ING-5`): every picture a reader of the
//! document can see becomes searchable by what it shows.
//!
//! 1. **Gather.** The [`VisualCollector`] finds the file's own pictures (the
//!    upload itself, Office/EPUB media, opt-in PDF image objects) and
//!    normalises them; a layout extractor's figure crops are normalised through
//!    the same collector.
//! 2. **Select** with [`kb_core::visuals::select`]: icons, strips, a logo on
//!    every slide and duplicates are counted and dropped; uploads are kept.
//! 3. **Read** each kept picture with the [`ImageDescriber`] (bounded
//!    concurrency, a timeout per picture). A failure is counted, never
//!    papered over with an invented description.
//! 4. **Index.** A picture the describer called a logo or decoration is not
//!    indexed (unless it is the upload); every other picture with something
//!    known about it is stored as an asset with its true content type and
//!    becomes a `figure` chunk carrying its `assetKey`, so the dense, BM25 and
//!    figure arms find it and the caller can fetch the picture.
//!
//! What happened is returned as a [`VisualOutcome`] for the document's
//! metadata (`metadata.visuals`).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use kb_core::chunk::{Chunk, ChunkMetadata};
use kb_core::visuals::{Candidate, SelectRules, SkipCounts, VisualText};
use kb_ports::{
    BlobStore, DocumentId, ExtractedFigure, ImageDescriber, ImageReading, Visual, VisualCollector,
    VisualOptions, VisualOrigin, asset_path,
};
use serde::Serialize;
use serde_json::{Value, json};

/// Settings for the visual stage (`KB_IMAGE_*`).
#[derive(Debug, Clone)]
pub struct VisualSettings {
    pub rules: SelectRules,
    /// Pictures read at once.
    pub describe_concurrency: usize,
    /// Per-picture time limit for one reading.
    pub describe_timeout: Duration,
    /// Lift image objects out of PDFs (`KB_IMAGE_PDF`).
    pub pdf_images: bool,
    /// Page text, in characters, added to a picture's chunk for grounding.
    pub context_chars: usize,
}

impl Default for VisualSettings {
    fn default() -> Self {
        Self {
            rules: SelectRules::default(),
            describe_concurrency: 2,
            describe_timeout: Duration::from_secs(120),
            pdf_images: false,
            context_chars: 300,
        }
    }
}

/// The ports and settings of the visual stage.
pub struct VisualPipeline {
    collector: Arc<dyn VisualCollector>,
    describer: Option<Arc<dyn ImageDescriber>>,
    settings: VisualSettings,
}

impl VisualPipeline {
    #[must_use]
    pub fn new(
        collector: Arc<dyn VisualCollector>,
        describer: Option<Arc<dyn ImageDescriber>>,
        settings: VisualSettings,
    ) -> Self {
        Self {
            collector,
            describer,
            settings,
        }
    }

    /// Whether pictures can be read (an image describer is configured).
    #[must_use]
    pub fn can_describe(&self) -> bool {
        self.describer.is_some()
    }
}

/// What the visual stage did with one document, for `metadata.visuals`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualOutcome {
    /// Pictures found (before selection).
    pub found: u32,
    /// Pictures that became a chunk.
    pub indexed: u32,
    /// Pictures the describer read.
    pub described: u32,
    /// Readings that failed or timed out.
    pub describe_failed: u32,
    /// Embedded pictures the describer called a logo or decoration.
    pub ornaments: u32,
    /// Kept pictures with nothing known about them (no describer, no alt
    /// text, no caption): not indexed.
    pub undescribed: u32,
    pub skipped: SkipCounts,
    pub unsupported_format: u32,
    pub undecodable: u32,
    /// The last reading or collection error, so an operator sees why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether the file itself is a picture (a standalone upload).
    #[serde(skip)]
    pub is_upload: bool,
    /// `metadata.figures` entries for the indexed pictures.
    #[serde(skip)]
    pub figures: Vec<Value>,
}

impl VisualOutcome {
    /// Whether there is anything worth recording.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.found == 0 && self.unsupported_format == 0 && self.undecodable == 0
    }
}

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Everything about the document the stage needs, borrowed from ingest.
pub struct VisualInput<'a> {
    pub tenant_id: &'a str,
    pub document_id: &'a DocumentId,
    pub document_title: &'a str,
    pub category: &'a str,
    pub subcategory: Option<&'a str>,
    pub filename: &'a str,
    pub mime: &'a str,
    pub bytes: &'a [u8],
    /// The layout extractor's figure crops.
    pub layout_figures: &'a [ExtractedFigure],
    /// Text of each page, for grounding.
    pub page_text: &'a HashMap<u32, String>,
    /// Layout figure id → the text chunk it belongs to.
    pub anchors: &'a HashMap<String, u32>,
}

/// Run the stage: append the pictures' chunks to `chunks` and report.
pub async fn index_visuals(
    pipeline: &VisualPipeline,
    blobs: &dyn BlobStore,
    input: VisualInput<'_>,
    chunks: &mut Vec<Chunk>,
) -> VisualOutcome {
    let mut outcome = VisualOutcome::default();

    // 1. Gather: the file's own pictures, then the layout crops.
    let options = VisualOptions {
        pdf_images: pipeline.settings.pdf_images,
        // Collection stops a little after the cap: selection drops icons and
        // duplicates first, and the cap applies to what survives.
        max_visuals: pipeline.settings.rules.max_per_document.saturating_mul(4),
    };
    let mut visuals: Vec<Visual> = match pipeline
        .collector
        .collect(input.filename, input.mime, input.bytes, options)
        .await
    {
        Ok(scan) => {
            outcome.unsupported_format += scan.skipped.unsupported_format;
            outcome.undecodable += scan.skipped.undecodable;
            scan.visuals
        }
        Err(e) => {
            outcome.error = Some(format!("collect: {e}"));
            Vec::new()
        }
    };
    for fig in input.layout_figures {
        match pipeline.collector.normalize(&fig.image_bytes).await {
            Ok(Some(img)) => visuals.push(Visual {
                origin: VisualOrigin::LayoutCrop,
                page: fig.page,
                mime: img.mime,
                bytes: img.bytes,
                width: img.width,
                height: img.height,
                alt_text: None,
                caption: fig.caption.clone(),
                bbox: Some(fig.bbox),
                figure_id: fig.id.clone(),
                kind_hint: Some(fig.kind.clone()),
            }),
            Ok(None) => outcome.undecodable += 1,
            Err(e) => {
                outcome.undecodable += 1;
                outcome.error = Some(format!("normalize: {e}"));
            }
        }
    }
    outcome.found = u32::try_from(visuals.len()).unwrap_or(u32::MAX);
    outcome.is_upload = visuals.iter().any(|v| v.origin == VisualOrigin::Upload);
    if visuals.is_empty() {
        return outcome;
    }

    // 2. Select.
    let candidates: Vec<Candidate> = visuals
        .iter()
        .map(|v| Candidate {
            upload: v.origin == VisualOrigin::Upload,
            width: v.width,
            height: v.height,
            fingerprint: fingerprint(&v.bytes),
        })
        .collect();
    let selection = kb_core::visuals::select(&candidates, &pipeline.settings.rules);
    outcome.skipped = selection.skipped.clone();
    let kept: Vec<&Visual> = selection.keep.iter().map(|&i| &visuals[i]).collect();

    // 3. Read, in document order, a few at a time.
    let readings: Vec<Option<Result<ImageReading, String>>> = match &pipeline.describer {
        None => kept.iter().map(|_| None).collect(),
        Some(describer) => {
            let timeout = pipeline.settings.describe_timeout;
            // Owned inputs: a stream over borrowed visuals makes the whole
            // ingest future's `Send` bound not general enough for `spawn`.
            let jobs: Vec<(String, Vec<u8>)> = kept
                .iter()
                .map(|v| (v.mime.clone(), v.bytes.clone()))
                .collect();
            futures::stream::iter(jobs)
                .map(|(mime, bytes)| {
                    let describer = Arc::clone(describer);
                    async move {
                        let r = tokio::time::timeout(timeout, describer.read(&mime, &bytes)).await;
                        Some(match r {
                            Ok(Ok(reading)) => Ok(reading),
                            Ok(Err(e)) => Err(e.to_string()),
                            Err(_) => Err(format!("timed out after {} s", timeout.as_secs())),
                        })
                    }
                })
                .buffered(pipeline.settings.describe_concurrency.max(1))
                .collect()
                .await
        }
    };

    // 4. Index.
    let mut fig_chunks = Vec::new();
    for (n, (visual, reading)) in kept.iter().zip(readings).enumerate() {
        let reading = match reading {
            Some(Ok(r)) => {
                outcome.described += 1;
                Some(r)
            }
            Some(Err(e)) => {
                outcome.describe_failed += 1;
                outcome.error = Some(format!("describe: {e}"));
                None
            }
            None => None,
        };
        let is_upload = visual.origin == VisualOrigin::Upload;
        if let Some(r) = &reading
            && r.kind.is_ornament()
            && !is_upload
        {
            outcome.ornaments += 1;
            continue;
        }
        let context = if is_upload {
            None
        } else {
            input.page_text.get(&visual.page).map(String::as_str)
        };
        let kind = reading
            .as_ref()
            .map(|r| r.kind.as_str())
            .or(visual.kind_hint.as_deref().map(layout_kind));
        let text = VisualText {
            kind,
            description: reading.as_ref().map(|r| r.description.as_str()),
            text: reading.as_ref().map(|r| r.text.as_str()),
            alt_text: visual.alt_text.as_deref(),
            caption: visual.caption.as_deref(),
            context,
        };
        let Some(content) =
            kb_core::visuals::visual_chunk_text(&text, pipeline.settings.context_chars)
        else {
            outcome.undescribed += 1;
            continue;
        };
        let ext = if visual.mime == "image/png" {
            "png"
        } else {
            "jpg"
        };
        let key = asset_path(
            Some(input.tenant_id),
            &input.document_id.0,
            &format!("fig-p{}-{}.{ext}", visual.page, n + 1),
        );
        if let Err(e) = blobs.upload(&key, visual.bytes.clone(), &visual.mime).await {
            tracing::warn!(key = %key, error = %e, "picture upload failed; not indexed");
            outcome.error = Some(format!("upload: {e}"));
            continue;
        }
        let section = kb_core::visuals::visual_section(&text);
        outcome.figures.push(json!({
            "assetKey": key,
            "page": visual.page,
            "caption": section,
            "bbox": visual.bbox,
            "type": kind.unwrap_or("image"),
            "origin": visual.origin.as_str(),
            "description": reading.as_ref().map(|r| r.description.clone()),
            "text": reading.as_ref().map(|r| r.text.clone()).filter(|t| !t.is_empty()),
        }));
        outcome.indexed += 1;
        fig_chunks.push(Chunk {
            content,
            metadata: ChunkMetadata {
                document_title: input.document_title.to_string(),
                category: input.category.to_string(),
                subcategory: input.subcategory.map(str::to_string),
                section,
                heading_path: None,
                chunk_index: 0,
                chunk_type: Some("figure".to_string()),
                page: Some(visual.page),
                asset_key: Some(key),
                anchor_chunk_index: visual
                    .figure_id
                    .as_deref()
                    .and_then(|id| input.anchors.get(id).copied()),
                contextual_prefix: None,
            },
        });
    }
    if !fig_chunks.is_empty() {
        chunks.extend(fig_chunks);
        for (idx, c) in chunks.iter_mut().enumerate() {
            c.metadata.chunk_index = u32::try_from(idx).unwrap_or(u32::MAX);
        }
    }
    outcome
}

/// A layout extractor's figure kind → the describer's vocabulary.
fn layout_kind(kind: &str) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "table" => "table",
        "chart" | "graph" => "chart",
        "diagram" => "diagram",
        _ => "image",
    }
}

/// Why an image upload has nothing searchable, for the job's failure reason.
#[must_use]
pub fn nothing_searchable_reason(
    filename: &str,
    outcome: &VisualOutcome,
    can_describe: bool,
) -> String {
    let why = if !can_describe {
        "OCR found no text and no image describer is configured (set KB_IMAGE_DESCRIBE_MODEL)"
            .to_string()
    } else if let Some(e) = &outcome.error {
        format!("OCR found no text and the image could not be described: {e}")
    } else {
        "OCR found no text and the image describer returned nothing".to_string()
    };
    format!("nothing searchable in \"{filename}\": {why}")
}

/// `metadata.visuals` and `metadata.figures` for the document.
#[must_use]
pub fn metadata_patch(outcome: &VisualOutcome) -> Value {
    let mut patch = json!({ "visuals": outcome });
    if !outcome.figures.is_empty() {
        patch["figures"] = Value::Array(outcome.figures.clone());
    }
    patch
}
