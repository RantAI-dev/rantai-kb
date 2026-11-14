//! Scanned vs digital PDF detection.
//!
//! Strategy: Extract text from pages. If very little text is found
//! (<100 non-whitespace characters per page), it's likely a scanned
//! document that needs OCR.
//! Matches `isPDFScanned` in `src/lib/ocr/pdf-converter.ts:18`.\n
use pdfium_render::prelude::*;

use crate::ExtractError;

/// The facts the per-page scan test needs, read from one page.
#[derive(Debug, Clone)]
pub struct PageFacts {
    /// The page's full local text layer (zero-based page index in [`Self::page`]).
    pub page: u32,
    pub text: String,
    /// The share of the page covered by the union of its raster image objects,
    /// clipped to the page box. Overlapping images are not counted twice.
    pub image_area: f32,
    /// Visible characters: text objects whose render mode is not invisible.
    /// An existing invisible OCR layer therefore does not make a scan look
    /// digital, which is the whole point of the test.
    pub visible_chars: usize,
}

/// Whether a page is a scan: raster images cover at least `area` of it and it
/// carries at most `max_text` visible characters.
///
/// The document-level [`is_pdf_scanned`] average stays for the legacy path (a
/// PDF whose text layer the chain should skip); this is the per-page decision
/// the OCR router makes (`ING-6`).
#[must_use]
pub fn page_is_scan(facts: &PageFacts, area: f32, max_text: u32) -> bool {
    facts.image_area >= area && facts.visible_chars <= max_text as usize
}

/// Read every page's facts: text, raster coverage and visible characters.
///
/// # Errors
///
/// When the PDF cannot be loaded or a page's objects cannot be read; an
/// unreadable page is not silently treated as digital.
pub fn read_page_facts(
    pdfium: &Pdfium,
    pdf_bytes: &[u8],
) -> Result<Vec<PageFacts>, ExtractError> {
    let document = pdfium
        .load_pdf_from_byte_slice(pdf_bytes, None)
        .map_err(|e| ExtractError::Container(format!("failed to load PDF: {e}")))?;

    let mut out = Vec::new();
    for (index, page) in document.pages().iter().enumerate() {
        let text = page
            .text()
            .map_err(|e| ExtractError::Container(format!("failed to read page text: {e}")))?
            .all();

        let mut rects: Vec<(f32, f32, f32, f32)> = Vec::new();
        let mut visible_chars = 0usize;
        for object in page.objects().iter() {
            if let Some(image) = object.as_image_object() {
                if let Ok(bounds) = image.bounds() {
                    let rect = bounds.to_rect();
                    rects.push((
                        rect.left().value,
                        rect.bottom().value,
                        rect.right().value,
                        rect.top().value,
                    ));
                }
            }
            if let Some(text_object) = object.as_text_object() {
                // pdfium-render exposes the render mode
                // (`PdfPageTextObject::render_mode`), so an invisible OCR layer
                // is excluded as the spec asks.
                if text_object.is_visible() {
                    visible_chars += text_object
                        .text()
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .count();
                }
            }
        }

        let page_area = (page.width().value * page.height().value).abs();
        let image_area = if page_area > 0.0 {
            (union_area(&rects) / page_area).clamp(0.0, 1.0)
        } else {
            0.0
        };

        #[allow(clippy::cast_possible_truncation)]
        out.push(PageFacts {
            page: index as u32,
            text,
            image_area,
            visible_chars,
        });
    }
    Ok(out)
}

/// Area of the union of axis-aligned rectangles. A sweep over the x
/// coordinates, merging the y intervals of each strip, so two identical image
/// objects count once.
fn union_area(rects: &[(f32, f32, f32, f32)]) -> f32 {
    let mut xs: Vec<f32> = Vec::with_capacity(rects.len() * 2);
    let mut cleaned: Vec<(f32, f32, f32, f32)> = Vec::with_capacity(rects.len());
    for &(x0, y0, x1, y1) in rects {
        let (x0, x1) = (x0.min(x1), x0.max(x1));
        let (y0, y1) = (y0.min(y1), y0.max(y1));
        if x1 > x0 && y1 > y0 {
            cleaned.push((x0, y0, x1, y1));
            xs.push(x0);
            xs.push(x1);
        }
    }
    if cleaned.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs.dedup();

    let mut total = 0.0f32;
    for strip in xs.windows(2) {
        let (left, right) = (strip[0], strip[1]);
        if right <= left {
            continue;
        }
        let mid = (left + right) / 2.0;
        let mut spans: Vec<(f32, f32)> = cleaned
            .iter()
            .filter(|(x0, _, x1, _)| *x0 <= mid && mid <= *x1)
            .map(|(_, y0, _, y1)| (*y0, *y1))
            .collect();
        if spans.is_empty() {
            continue;
        }
        spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut covered = 0.0f32;
        let (mut start, mut end) = spans[0];
        for &(y0, y1) in &spans[1..] {
            if y0 > end {
                covered += end - start;
                start = y0;
                end = y1;
            } else if y1 > end {
                end = y1;
            }
        }
        covered += end - start;
        total += covered * (right - left);
    }
    total
}

/// Returns `true` if the PDF is scanned (image-based) or unreadable, `false`
/// if digital text layer is sufficient.
#[must_use]
pub fn is_pdf_scanned(pdfium: &Pdfium, pdf_bytes: &[u8]) -> bool {
    let document = match pdfium.load_pdf_from_byte_slice(pdf_bytes, None) {
        Ok(doc) => doc,
        Err(_) => return true,
    };

    let page_count = document.pages().len();
    if page_count == 0 {
        return true;
    }

    let mut non_ws_chars: usize = 0;
    for page in document.pages().iter() {
        let text_obj = match page.text() {
            Ok(t) => t,
            Err(_) => return true,
        };
        let page_text = text_obj.all();
        non_ws_chars += page_text.chars().filter(|c| !c.is_whitespace()).count();
    }

    let avg = (non_ws_chars as f64) / (page_count as f64);
    avg < 100.0
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::pdfium::{MINIMAL_BLANK_PDF, MINIMAL_PDF_HELLO_WORLD, get_pdfium};

    #[test]
    fn detects_blank_pdf_as_scanned() {
        let pdfium = match get_pdfium() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Skipping pdfium test: {e}");
                return;
            }
        };

        // MINIMAL_BLANK_PDF has 0 text chars -> avg 0 < 100 -> scanned
        assert!(is_pdf_scanned(pdfium, MINIMAL_BLANK_PDF));
    }

    #[test]
    fn detects_hello_world_below_threshold_as_scanned() {
        let pdfium = match get_pdfium() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Skipping pdfium test: {e}");
                return;
            }
        };

        // "Hello World" has 10 non-ws chars -> 10 / 1 < 100 -> scanned (needs >=100 chars/page)
        assert!(is_pdf_scanned(pdfium, MINIMAL_PDF_HELLO_WORLD));
    }
}
