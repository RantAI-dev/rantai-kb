//! Scanned vs digital PDF detection.
//!
//! Strategy: Extract text from pages. If very little text is found
//! (<100 non-whitespace characters per page), it's likely a scanned
//! document that needs OCR.
//! Matches `isPDFScanned` in `src/lib/ocr/pdf-converter.ts:18`.\n
use pdfium_render::prelude::*;

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
