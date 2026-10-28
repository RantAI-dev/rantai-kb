//! [`PageRenderer`] over pdfium: each PDF page as a JPEG, for the page-image arm.
//!
//! 96 dpi by default: the resolution the ViDoRe HR experiment measured
//! (`docs/eval-ab/vidore3-hr-en-image`); at 72 dpi chart labels are barely
//! legible, and every extra pixel is paid for by a hosted embedder.

use std::io::Cursor;

use async_trait::async_trait;
use image::codecs::jpeg::JpegEncoder;
use kb_ports::{PageImage, PageRenderer, PortError};
use pdfium_render::prelude::*;

use crate::ExtractError;
use crate::pdfium::get_pdfium;

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
            .and_then(|page| render_one(&page, &config, jpeg_quality));
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

fn render_one(
    page: &PdfPage<'_>,
    config: &PdfRenderConfig,
    jpeg_quality: u8,
) -> Result<Vec<u8>, String> {
    let rgb = page
        .render_with_config(config)
        .map_err(|e| format!("render: {e}"))?
        .as_image()
        .map_err(|e| format!("bitmap: {e}"))?
        .to_rgb8();
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut Cursor::new(&mut bytes), jpeg_quality.clamp(30, 95))
        .encode_image(&rgb)
        .map_err(|e| format!("jpeg: {e}"))?;
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
        if !is_pdf(mime_type, file_bytes) || max_pages == 0 {
            return Ok(Vec::new());
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
        if !is_pdf(mime_type, file_bytes) || count == 0 {
            return Ok(Vec::new());
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
}
