//! [`PageRenderer`] over pdfium: each PDF page as a JPEG, for the page-image arm.
//!
//! An image upload is a document of its own pages too (one, or one per TIFF
//! page): it is decoded and normalised exactly as a visual is
//! ([`crate::visuals::normalize_image`]), so the page-image arm, its captions
//! and the vision rerank see a picture the user uploaded the same way they see
//! a PDF page.
//!
//! 96 dpi by default: the resolution the ViDoRe HR experiment measured
//! (`docs/eval-ab/vidore3-hr-en-image`); at 72 dpi chart labels are barely
//! legible, and every extra pixel is paid for by a hosted embedder.

use std::io::Cursor;

use async_trait::async_trait;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use kb_ports::{PageImage, PageRenderer, PortError};
use pdfium_render::prelude::*;

use crate::ExtractError;
use crate::pdfium::get_pdfium;

/// How a page image is encoded for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageImageFormat {
    /// JPEG at the given quality.
    Jpeg { quality: u8 },
    /// PNG, lossless.
    Png,
}

impl PageImageFormat {
    /// The mime the encoded bytes carry.
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            Self::Jpeg { .. } => "image/jpeg",
            Self::Png => "image/png",
        }
    }
}

/// How a provider wants a page rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRenderSpec {
    /// Resolution.
    pub dpi: u32,
    /// The encoding.
    pub format: PageImageFormat,
    /// The longest edge the provider accepts; `0` means no limit.
    ///
    /// A page longer than this is scaled down, keeping its aspect ratio.
    pub max_edge: u32,
}

impl PageRenderSpec {
    /// The spec the page router used before the xiaomi recipe: a JPEG of the
    /// whole page, at the given resolution.
    #[must_use]
    pub fn jpeg(dpi: u32, quality: u8) -> Self {
        Self {
            dpi,
            format: PageImageFormat::Jpeg { quality },
            max_edge: 0,
        }
    }
}

/// The longest edge of an image, in pixels.
fn longest_edge(width: u32, height: u32) -> u32 {
    width.max(height)
}

/// The size an image is scaled to when it must fit a square of `max_edge`.
///
/// This is Pillow's `thumbnail`: the aspect ratio is kept, and the size that
/// is not rounded to a whole number of pixels is rounded the way Python rounds
/// (half to even). One pixel of difference changes nothing for a model that
/// reads a page, but the same rule keeps the two renderers comparable.
fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if max_edge == 0 || longest_edge(width, height) <= max_edge {
        return (width, height);
    }
    let width_f = f64::from(width);
    let height_f = f64::from(height);
    let edge_f = f64::from(max_edge);
    let (new_width, new_height) = if edge_f * height_f <= edge_f * width_f {
        (edge_f, round_aspect(edge_f * height_f / width_f))
    } else {
        (round_aspect(edge_f * width_f / height_f), edge_f)
    };
    ((new_width.max(1.0)) as u32, (new_height.max(1.0)) as u32)
}

/// Round like Python's `round()`: halfway cases go to the even neighbour.
fn python_round(value: f64) -> f64 {
    let fraction = value - value.trunc();
    if fraction.abs() == 0.5 {
        let floor = value.floor();
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        value.round()
    }
}

/// The pixel count Pillow's `thumbnail` would pick for one side.
fn round_aspect(value: f64) -> f64 {
    let floor = value.floor();
    let ceil = value.ceil();
    let picked = if python_round(floor) <= python_round(ceil) {
        floor
    } else {
        ceil
    };
    picked.max(1.0)
}

/// Renders PDF pages with the process-wide pdfium.
#[derive(Debug, Clone, Copy)]
pub struct PdfiumPageRenderer {
    pub dpi: u32,
    pub jpeg_quality: u8,
}

impl Default for PdfiumPageRenderer {
    fn default() -> Self {
        Self {
            dpi: 96,
            jpeg_quality: 80,
        }
    }
}

fn is_pdf(mime_type: &str, bytes: &[u8]) -> bool {
    mime_type.eq_ignore_ascii_case("application/pdf") || bytes.starts_with(b"%PDF-")
}

/// Whether the bytes are a picture rather than a PDF: by mime, else by magic.
fn is_image(mime_type: &str, bytes: &[u8]) -> bool {
    if mime_type.to_ascii_lowercase().starts_with("image/") {
        return true;
    }
    bytes.starts_with(b"\x89PNG")
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"GIF8")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
        || bytes.starts_with(b"II*\x00")
        || bytes.starts_with(b"MM\x00*")
}

/// Pages `first .. first + count` of an image upload as normalised JPEGs: one
/// page for most images, one per page for a TIFF. Bytes that do not decode are
/// no pages, not an error: OCR reports the unreadable upload.
#[must_use]
pub fn render_image_pages(bytes: &[u8], first: u32, count: u32) -> Vec<PageImage> {
    let pages: Vec<Vec<u8>> = if bytes.starts_with(b"II*\x00") || bytes.starts_with(b"MM\x00*") {
        crate::tiff_pages::pages_as_png(bytes).unwrap_or_default()
    } else {
        vec![bytes.to_vec()]
    };
    pages
        .iter()
        .enumerate()
        .skip(first as usize)
        .take(count as usize)
        .filter_map(|(index, page)| {
            let normalized = crate::visuals::normalize_image(page)?;
            #[allow(clippy::cast_possible_truncation)]
            Some(PageImage {
                page: index as u32,
                mime: normalized.mime,
                bytes: normalized.bytes,
                text: String::new(),
            })
        })
        .collect()
}

/// Render the first `max_pages` pages of a PDF to JPEG.
///
/// # Errors
///
/// When pdfium is missing or the PDF cannot be loaded. A page that fails to
/// render comes back with empty `bytes`.
pub fn render_pdf_pages(
    pdf_bytes: &[u8],
    max_pages: u32,
    dpi: u32,
    jpeg_quality: u8,
) -> Result<Vec<PageImage>, ExtractError> {
    render_pdf_page_range(pdf_bytes, 0, max_pages, dpi, jpeg_quality)
}

/// Render pages `first .. first + count` of a PDF to JPEG (fewer at the end).
///
/// # Errors
///
/// When pdfium is missing or the PDF cannot be loaded. A page that fails to
/// render comes back with empty `bytes`.
pub fn render_pdf_page_range(
    pdf_bytes: &[u8],
    first: u32,
    count: u32,
    dpi: u32,
    jpeg_quality: u8,
) -> Result<Vec<PageImage>, ExtractError> {
    let pdfium = get_pdfium()?;
    let document = pdfium
        .load_pdf_from_byte_slice(pdf_bytes, None)
        .map_err(|e| ExtractError::Container(format!("failed to load PDF: {e}")))?;
    #[allow(clippy::cast_precision_loss)]
    let scale = dpi.clamp(36, 300) as f32 / 72.0;
    let config = PdfRenderConfig::new().scale_page_by_factor(scale);
    let mut out = Vec::new();
    let pages = document.pages();
    #[allow(clippy::cast_sign_loss)]
    let end = first.saturating_add(count).min(pages.len() as u32);
    for index in first..end {
        // One bad page must not cost the document its other pages: it comes
        // back with no bytes, and the page arm counts it as failed.
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let rendered = pages
            .get(index as _)
            .map_err(|e| format!("load: {e}"))
            .and_then(|page| render_one(&page, &config, PageRenderSpec::jpeg(dpi, jpeg_quality)));
        let bytes = rendered.unwrap_or_else(|e| {
            tracing::warn!(page = index, error = %e, "page not rendered");
            Vec::new()
        });
        out.push(PageImage {
            page: index,
            mime: "image/jpeg".to_string(),
            bytes,
            text: String::new(),
        });
    }
    Ok(out)
}

/// Render the given page indices (0-based, in the order given) to JPEG.
///
/// Unlike [`render_pdf_page_range`], the pages need not be contiguous: the OCR
/// router renders only the pages it will send, and a batch of them may skip a
/// page already in the cache.
///
/// # Errors
///
/// When pdfium is missing or the PDF cannot be loaded. A page that fails to
/// render comes back with empty `bytes`.
pub fn render_pdf_page_indices(
    pdf_bytes: &[u8],
    indices: &[u32],
    dpi: u32,
    jpeg_quality: u8,
) -> Result<Vec<PageImage>, ExtractError> {
    render_pdf_page_indices_spec(pdf_bytes, indices, PageRenderSpec::jpeg(dpi, jpeg_quality))
}

/// Render the given page indices with a provider's spec: its resolution, its
/// encoding, and the longest edge it accepts.
///
/// # Errors
///
/// When pdfium is missing or the PDF cannot be loaded. A page that fails to
/// render comes back with empty `bytes`.
pub fn render_pdf_page_indices_spec(
    pdf_bytes: &[u8],
    indices: &[u32],
    spec: PageRenderSpec,
) -> Result<Vec<PageImage>, ExtractError> {
    let pdfium = get_pdfium()?;
    let document = pdfium
        .load_pdf_from_byte_slice(pdf_bytes, None)
        .map_err(|e| ExtractError::Container(format!("failed to load PDF: {e}")))?;
    #[allow(clippy::cast_precision_loss)]
    let scale = spec.dpi.clamp(36, 300) as f32 / 72.0;
    let config = PdfRenderConfig::new().scale_page_by_factor(scale);
    let pages = document.pages();
    let mut out = Vec::with_capacity(indices.len());
    for &index in indices {
        // One bad page must not cost the document its other pages: it comes
        // back with no bytes, and the router marks that page failed.
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let rendered = pages
            .get(index as _)
            .map_err(|e| format!("load: {e}"))
            .and_then(|page| render_one(&page, &config, spec));
        let bytes = rendered.unwrap_or_else(|e| {
            tracing::warn!(page = index, error = %e, "page not rendered");
            Vec::new()
        });
        out.push(PageImage {
            page: index,
            mime: spec.format.mime().to_string(),
            bytes,
            text: String::new(),
        });
    }
    Ok(out)
}

fn render_one(
    page: &PdfPage<'_>,
    config: &PdfRenderConfig,
    spec: PageRenderSpec,
) -> Result<Vec<u8>, String> {
    let bitmap = page
        .render_with_config(config)
        .map_err(|e| format!("render: {e}"))?
        .as_image()
        .map_err(|e| format!("bitmap: {e}"))?;
    let rgb = bitmap.to_rgb8();
    let image = image::DynamicImage::ImageRgb8(match spec.max_edge {
        0 => rgb,
        edge => {
            let (width, height) = fit_within(rgb.width(), rgb.height(), edge);
            if width == rgb.width() && height == rgb.height() {
                rgb
            } else {
                image::imageops::resize(&rgb, width, height, FilterType::Lanczos3)
            }
        }
    });
    let mut bytes = Vec::new();
    match spec.format {
        PageImageFormat::Jpeg { quality } => {
            JpegEncoder::new_with_quality(&mut Cursor::new(&mut bytes), quality.clamp(30, 95))
                .encode_image(&image.to_rgb8())
                .map_err(|e| format!("jpeg: {e}"))?;
        }
        PageImageFormat::Png => {
            image
                .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
                .map_err(|e| format!("png: {e}"))?;
        }
    }
    Ok(bytes)
}

#[async_trait]
impl PageRenderer for PdfiumPageRenderer {
    async fn render(
        &self,
        mime_type: &str,
        file_bytes: &[u8],
        max_pages: u32,
    ) -> Result<Vec<PageImage>, PortError> {
        if max_pages == 0 {
            return Ok(Vec::new());
        }
        if !is_pdf(mime_type, file_bytes) {
            if !is_image(mime_type, file_bytes) {
                return Ok(Vec::new());
            }
            let bytes = file_bytes.to_vec();
            return tokio::task::spawn_blocking(move || render_image_pages(&bytes, 0, max_pages))
                .await
                .map_err(|e| PortError::Unavailable(format!("page render task: {e}")));
        }
        let bytes = file_bytes.to_vec();
        let (dpi, quality) = (self.dpi, self.jpeg_quality);
        tokio::task::spawn_blocking(move || render_pdf_pages(&bytes, max_pages, dpi, quality))
            .await
            .map_err(|e| PortError::Unavailable(format!("page render task: {e}")))?
            .map_err(|e| PortError::Unavailable(format!("page render: {e}")))
    }

    async fn render_range(
        &self,
        mime_type: &str,
        file_bytes: &[u8],
        first: u32,
        count: u32,
    ) -> Result<Vec<PageImage>, PortError> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if !is_pdf(mime_type, file_bytes) {
            if !is_image(mime_type, file_bytes) {
                return Ok(Vec::new());
            }
            let bytes = file_bytes.to_vec();
            return tokio::task::spawn_blocking(move || render_image_pages(&bytes, first, count))
                .await
                .map_err(|e| PortError::Unavailable(format!("page render task: {e}")));
        }
        let bytes = file_bytes.to_vec();
        let (dpi, quality) = (self.dpi, self.jpeg_quality);
        tokio::task::spawn_blocking(move || {
            render_pdf_page_range(&bytes, first, count, dpi, quality)
        })
        .await
        .map_err(|e| PortError::Unavailable(format!("page render task: {e}")))?
        .map_err(|e| PortError::Unavailable(format!("page render: {e}")))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::pdfium::MINIMAL_PDF_HELLO_WORLD;

    #[tokio::test]
    async fn a_range_past_the_last_page_is_empty_not_an_error() {
        if get_pdfium().is_err() {
            eprintln!("libpdfium not found; skipping");
            return;
        }
        let r = PdfiumPageRenderer::default();
        let first = r
            .render_range("application/pdf", MINIMAL_PDF_HELLO_WORLD, 0, 64)
            .await
            .unwrap();
        assert_eq!(first.len(), 1, "a 1-page PDF has one page in 0..64");
        assert!(!first[0].bytes.is_empty());
        let past = r
            .render_range("application/pdf", MINIMAL_PDF_HELLO_WORLD, 1, 64)
            .await
            .unwrap();
        assert!(past.is_empty());
    }

    #[tokio::test]
    async fn renders_a_pdf_page_as_jpeg_at_the_requested_dpi() {
        if get_pdfium().is_err() {
            eprintln!("libpdfium not found; skipping");
            return;
        }
        let r = PdfiumPageRenderer::default();
        let pages = r
            .render("application/pdf", MINIMAL_PDF_HELLO_WORLD, 10)
            .await
            .unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].page, 0);
        assert_eq!(pages[0].mime, "image/jpeg");
        let img = image::load_from_memory(&pages[0].bytes).unwrap();
        // 612 x 792 points at 96 dpi.
        assert_eq!((img.width(), img.height()), (816, 1056));
    }

    #[tokio::test]
    async fn non_pdf_files_and_a_zero_page_budget_render_nothing() {
        let r = PdfiumPageRenderer::default();
        assert!(
            r.render("text/plain", b"hello", 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            r.render("application/pdf", MINIMAL_PDF_HELLO_WORLD, 0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([9, 9, 9])))
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[tokio::test]
    async fn an_image_upload_is_one_normalised_jpeg_page() {
        let r = PdfiumPageRenderer::default();
        let pages = r.render("image/png", &png(3000, 1500), 10).await.unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].mime, "image/jpeg");
        let img = image::load_from_memory(&pages[0].bytes).unwrap();
        assert_eq!((img.width(), img.height()), (1568, 784));
        // Recognised by its bytes when the mime is missing.
        assert_eq!(r.render("", &png(4, 4), 10).await.unwrap().len(), 1);
        // Garbage that claims to be an image is no pages, not an error.
        assert!(r.render("image/png", b"nope", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn every_tiff_page_is_a_page_and_the_range_is_honoured() {
        let mut tif = Cursor::new(Vec::new());
        let mut encoder = tiff::encoder::TiffEncoder::new(&mut tif).unwrap();
        for shade in [10u8, 120, 250] {
            encoder
                .write_image::<tiff::encoder::colortype::Gray8>(2, 2, &[shade; 4])
                .unwrap();
        }
        let bytes = tif.into_inner();
        let r = PdfiumPageRenderer::default();
        let all = r.render("image/tiff", &bytes, 10).await.unwrap();
        assert_eq!(
            all.iter().map(|p| p.page).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let window = r.render_range("image/tiff", &bytes, 1, 1).await.unwrap();
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].page, 1);
    }
}
