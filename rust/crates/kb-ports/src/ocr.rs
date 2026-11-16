//! The OCR offload contract: what a document's OCR pass reports, and where a
//! page result is cached.
//!
//! The report is the visible half of ING-6: a document whose pages went to an
//! OCR service says how many pages it sent, how many came back from the cache,
//! how many were read locally, and — the part that matters — which pages could
//! not be read at all. It is stored in the document's `metadata` JSONB under
//! `ocr`, so an operator sees a lost page without reading the logs.
//!
//! The cache is a port, not the disk store: `kb-extract` (an inner crate) may
//! not name `kb-cache` (an adapter), so the composition root hands the
//! extractor a cache implementation and the engine never learns one exists.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::PortError;

/// What one document's OCR offload did.
///
/// Field names are camelCase because they land in the document's `metadata`
/// JSONB, where the calling apps read them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrReport {
    /// `off`, `hybrid` or `full`.
    pub mode: String,
    /// The provider that served the pages (`"mistral"`, `"chat"`, …); `None`
    /// when no provider was configured or no page was sent.
    pub provider: Option<String>,
    /// How many pages the document has.
    pub pages: u32,
    /// How many pages were sent to the provider.
    pub ocr_pages: u32,
    /// How many pages came from the cache, so no request was paid for.
    pub cached_pages: u32,
    /// How many pages were read from the local text layer.
    pub local_pages: u32,
    /// The pages that could not be read (1-based, as a PDF viewer counts).
    pub failed_pages: Vec<u32>,
    /// The pages past the page cap (1-based).
    pub skipped_pages: Vec<u32>,
    /// Whether the outage breaker was open while this document ran.
    pub breaker_opened: bool,
    /// How many pages a layout detector answered for.
    ///
    /// Zero when no layout detector is configured: the page was read whole,
    /// without regions.
    #[serde(default)]
    pub layout_pages: u32,
    /// How many pages a layout detector failed on.
    ///
    /// A page in this count kept the whole-page answer: the layout path could
    /// not be applied, and the report says so rather than pretending the page
    /// had no regions.
    #[serde(default)]
    pub layout_failed: u32,
}

impl OcrReport {
    /// A report for a document that sent nothing to a provider.
    #[must_use]
    pub fn local(mode: &str, pages: u32) -> Self {
        Self {
            mode: mode.to_string(),
            pages,
            local_pages: pages,
            ..Self::default()
        }
    }
}

/// A cache of OCR page results, keyed by the page's content hash.
///
/// The key is the hash the caller computes over the provider id, the model,
/// the prompt, the resolution and the page's bytes, so a cache is never asked
/// whether two different pages are the same page. Implementations must treat
/// every I/O error as a miss: a broken cache makes ingest slower, never wrong.
#[async_trait]
pub trait OcrPageCache: Send + Sync {
    /// The text a page was read as before, if it was.
    async fn get(&self, key: &str) -> Option<String>;

    /// Remember a page's text. Only successful reads are stored.
    async fn put(&self, key: &str, text: &str);
}

/// One region of a page, as a layout detector found it.
///
/// The box is normalised to 0–1000 so it travels with the page's own
/// resolution: a detector that sees a 150 dpi page and one that sees a 300
/// dpi page describe the same region with the same numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutBox {
    /// The label the detector gave the region (`"text"`, `"table"`, …).
    pub label: String,
    /// The detector's confidence, 0–1.
    #[serde(default)]
    pub score: f32,
    /// The region's box, normalised to 0–1000: `[x1, y1, x2, y2]`.
    pub bbox_2d: [f64; 4],
    /// What to do with the region: `text`, `table`, `formula` or `skip`.
    #[serde(default)]
    pub task_type: String,
}

/// A page's layout: the regions to read, in reading order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PageLayout {
    /// The regions, in the order the detector says to read them.
    #[serde(default)]
    pub regions: Vec<LayoutBox>,
}

/// A layout detector: where a page's text, tables and formulas are.
///
/// The detector is a model, so it is a service like an OCR provider: it can be
/// down, and the caller must degrade visibly rather than pretend the page had
/// no regions.
#[async_trait]
pub trait LayoutDetector: Send + Sync {
    /// Detect the layout of one page image.
    async fn detect(&self, image_bytes: &[u8], mime: &str) -> Result<PageLayout, PortError>;
}
