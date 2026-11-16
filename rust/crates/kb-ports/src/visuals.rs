//! Visuals: the images a reader of a document can see, and reading them.
//!
//! A *visual* is one picture: a standalone image upload (one per TIFF page),
//! a picture embedded in an Office or ebook file, an image object in a PDF, or
//! a figure crop a layout extractor cut out. Two ports serve them (`ING-5`):
//!
//! - [`VisualCollector`] finds a file's visuals and normalises their pixels
//!   (decoded, bounded in size, re-encoded as JPEG or PNG), so the rest of the
//!   pipeline never decodes an image. It is local and free, so it is not part
//!   of the cached [`Extractor`](crate::Extractor): the text extraction cache
//!   and its paid OCR results are untouched by it.
//! - [`ImageDescriber`] reads one normalised image with a vision model: what
//!   kind of picture it is, what it shows, and the text written in it.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::PortError;

/// Where a visual came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VisualOrigin {
    /// The uploaded file is the image (or one page of a TIFF).
    Upload,
    /// A picture embedded in the document (Office media, EPUB `<img>`, a PDF
    /// image object).
    Embedded,
    /// A crop a layout extractor cut out of a page.
    LayoutCrop,
}

impl VisualOrigin {
    /// Wire form, as stored in document metadata.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Embedded => "embedded",
            Self::LayoutCrop => "layoutCrop",
        }
    }
}

/// One normalised picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Visual {
    pub origin: VisualOrigin,
    /// 0-based page, slide or sheet the picture sits on; 0 when the format has
    /// no pages.
    pub page: u32,
    /// `image/jpeg` or `image/png`.
    pub mime: String,
    /// The normalised bytes.
    pub bytes: Vec<u8>,
    /// Pixel size of the *original* picture, before normalisation, so a filter
    /// can drop an icon that was upscaled by nobody.
    pub width: u32,
    pub height: u32,
    /// The author's alternative text (`descr`, `alt`, `svg:desc`), when any.
    pub alt_text: Option<String>,
    /// A caption the extractor found next to it, when any.
    pub caption: Option<String>,
    /// `[x0, y0, x1, y1]` in page points, when the format places it on a page.
    pub bbox: Option<[f32; 4]>,
    /// The layout extractor's figure id, for reading-order anchoring.
    pub figure_id: Option<String>,
    /// The layout extractor's kind (`image`, `table`, `chart`), when any.
    pub kind_hint: Option<String>,
}

/// A picture after normalisation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedImage {
    /// `image/jpeg` or `image/png`.
    pub mime: String,
    pub bytes: Vec<u8>,
    /// Pixel size of the original.
    pub width: u32,
    pub height: u32,
}

/// Visuals a collector saw but could not hand over, by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualScanSkips {
    /// A format with no decoder here (EMF, WMF, SVG, HEIC).
    pub unsupported_format: u32,
    /// Bytes that would not decode.
    pub undecodable: u32,
}

/// What a collector found in one file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VisualScan {
    pub visuals: Vec<Visual>,
    pub skipped: VisualScanSkips,
}

/// What to look for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VisualOptions {
    /// Also lift image objects out of PDFs. Off by default: a text-layer PDF
    /// can hold hundreds of images, and page captions already cover PDFs on
    /// the tiers that have them.
    pub pdf_images: bool,
    /// Stop after this many visuals (0 = no limit), so a hostile file cannot
    /// make the collector decode a million images.
    pub max_visuals: u32,
}

/// Finds a file's visuals and normalises pictures.
#[async_trait]
pub trait VisualCollector: Send + Sync {
    /// Every visual in the file, in document order. A format with no visuals
    /// returns an empty scan, never an error; an error means the file could not
    /// be read at all.
    async fn collect(
        &self,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        options: VisualOptions,
    ) -> Result<VisualScan, PortError>;

    /// Normalise one picture handed over by someone else (a layout crop).
    /// `Ok(None)` when the bytes are not a picture this collector decodes.
    async fn normalize(&self, bytes: &[u8]) -> Result<Option<NormalizedImage>, PortError>;
}

/// What kind of picture an image is, as the describing model judged it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisualKind {
    Photo,
    Diagram,
    Chart,
    Screenshot,
    Table,
    Document,
    Map,
    Logo,
    Decorative,
    Other,
}

impl VisualKind {
    /// Parse the model's word; anything unknown is [`VisualKind::Other`].
    #[must_use]
    pub fn parse(word: &str) -> Self {
        match word.trim().to_ascii_lowercase().as_str() {
            "photo" | "photograph" | "picture" => Self::Photo,
            "diagram" | "illustration" | "drawing" | "flowchart" => Self::Diagram,
            "chart" | "graph" | "plot" => Self::Chart,
            "screenshot" | "screen" | "ui" => Self::Screenshot,
            "table" => Self::Table,
            "document" | "scan" | "text" | "page" => Self::Document,
            "map" => Self::Map,
            "logo" | "icon" => Self::Logo,
            "decorative" | "decoration" | "background" | "border" => Self::Decorative,
            _ => Self::Other,
        }
    }

    /// Wire form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Diagram => "diagram",
            Self::Chart => "chart",
            Self::Screenshot => "screenshot",
            Self::Table => "table",
            Self::Document => "document",
            Self::Map => "map",
            Self::Logo => "logo",
            Self::Decorative => "decorative",
            Self::Other => "other",
        }
    }

    /// Whether a picture of this kind carries no content worth a search hit
    /// when it is embedded in a document (a company logo on every slide, a
    /// divider). A standalone upload is always indexed: the user chose it.
    #[must_use]
    pub fn is_ornament(self) -> bool {
        matches!(self, Self::Logo | Self::Decorative)
    }
}

/// A vision model's reading of one image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageReading {
    pub kind: VisualKind,
    /// What the picture shows, in sentences a searcher would use.
    pub description: String,
    /// Text legible in the picture, verbatim; empty when none.
    pub text: String,
}

/// Reads images with a vision model.
///
/// A failed call is a [`PortError`]: the caller counts it and leaves the
/// visual undescribed; it never invents a description.
#[async_trait]
pub trait ImageDescriber: Send + Sync {
    /// Read one normalised image (`mime` is `image/jpeg` or `image/png`).
    async fn read(&self, mime: &str, bytes: &[u8]) -> Result<ImageReading, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_words_map_to_kinds_and_unknown_is_other() {
        assert_eq!(VisualKind::parse(" Photograph "), VisualKind::Photo);
        assert_eq!(VisualKind::parse("bar graph"), VisualKind::Other);
        assert_eq!(VisualKind::parse("GRAPH"), VisualKind::Chart);
        assert_eq!(VisualKind::parse("icon"), VisualKind::Logo);
        assert!(VisualKind::Logo.is_ornament());
        assert!(!VisualKind::Chart.is_ornament());
        for kind in [VisualKind::Photo, VisualKind::Map, VisualKind::Other] {
            assert_eq!(VisualKind::parse(kind.as_str()), kind);
        }
    }
}
