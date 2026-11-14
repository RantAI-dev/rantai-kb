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
pub fn read_page_facts(pdfium: &Pdfium, pdf_bytes: &[u8]) -> Result<Vec<PageFacts>, ExtractError> {
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

    /// One generated page: a text object, or a raster image over the whole
    /// page, or both with the text in an invisible render mode (an existing
    /// OCR layer).
    fn generated_page(kind: &str) -> Vec<u8> {
        use pdfium_render::prelude::*;
        let pdfium = get_pdfium().unwrap();
        let mut document = pdfium.create_new_pdf().unwrap();
        let font = document.fonts_mut().new_built_in(PdfFontBuiltin::Helvetica);
        let mut page = document
            .pages_mut()
            .create_page_at_end(PdfPagePaperSize::a4())
            .unwrap();
        let (width, height) = (page.width(), page.height());
        if kind != "text" {
            let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                32,
                32,
                image::Rgb([250, 250, 250]),
            ));
            page.objects_mut()
                .create_image_object(
                    PdfPoints::ZERO,
                    PdfPoints::ZERO,
                    &image,
                    Some(width),
                    Some(height),
                )
                .unwrap();
        }
        if kind != "image" {
            if kind == "invisible" {
                // Set the mode *before* the object joins the page: the page's
                // content must be regenerated for a later change to show up.
                let mut object = PdfPageTextObject::new(
                    &document,
                    "this page has a real text layer with more than fifty characters on it",
                    font,
                    PdfPoints::new(12.0),
                )
                .unwrap();
                object
                    .set_render_mode(PdfPageTextRenderMode::Invisible)
                    .unwrap();
                object
                    .translate(PdfPoints::new(40.0), PdfPoints::new(height.value - 60.0))
                    .unwrap();
                page.objects_mut().add_object(object.into()).unwrap();
            } else {
                page.objects_mut()
                    .create_text_object(
                        PdfPoints::new(40.0),
                        PdfPoints::new(height.value - 60.0),
                        "this page has a real text layer with more than fifty characters on it",
                        font,
                        PdfPoints::new(12.0),
                    )
                    .unwrap();
            }
        }
        let mut cursor = std::io::Cursor::new(Vec::new());
        document.save_to_writer(&mut cursor).unwrap();
        cursor.into_inner()
    }

    #[test]
    fn the_page_scan_test_is_per_page_not_per_document() {
        let Ok(pdfium) = get_pdfium() else {
            eprintln!("Skipping pdfium test: libpdfium not found");
            return;
        };

        // One test, three pages generated one at a time: pdfium's document
        // creation is not safe to drive from several test threads at once.
        let text = read_page_facts(pdfium, &generated_page("text")).unwrap();
        assert!(!page_is_scan(&text[0], 0.90, 50));
        assert!(text[0].image_area < 0.01, "no raster object on a text page");
        assert!(text[0].visible_chars > 50);

        let image = read_page_facts(pdfium, &generated_page("image")).unwrap();
        assert!(page_is_scan(&image[0], 0.90, 50));
        assert!(
            (image[0].image_area - 1.0).abs() < 0.01,
            "image covers the page"
        );
        assert_eq!(image[0].visible_chars, 0);

        // An invisible OCR layer still reads as a scan: pdfium-render exposes
        // `PdfPageTextObject::render_mode()`, so invisible text is excluded.
        let invisible = read_page_facts(pdfium, &generated_page("invisible")).unwrap();
        assert_eq!(
            invisible[0].visible_chars, 0,
            "invisible text must not count"
        );
        assert!(page_is_scan(&invisible[0], 0.90, 50));
        assert!(
            invisible[0].text.contains("real text layer"),
            "the local layer is still available as a fallback"
        );
    }

    #[test]
    fn overlapping_images_are_counted_once() {
        // Two identical rectangles cover 4 points of area, not 8.
        let area = union_area(&[(0.0, 0.0, 2.0, 2.0), (0.0, 0.0, 2.0, 2.0)]);
        assert!((area - 4.0).abs() < 1e-6);
        // Side by side they add up.
        let area = union_area(&[(0.0, 0.0, 1.0, 2.0), (1.0, 0.0, 3.0, 2.0)]);
        assert!((area - 6.0).abs() < 1e-6);
        // Touching in a corner adds nothing.
        let area = union_area(&[(0.0, 0.0, 1.0, 1.0), (1.0, 1.0, 2.0, 2.0)]);
        assert!((area - 2.0).abs() < 1e-6);
    }
}
