//! Dynamic libpdfium loader and text-layer/image-rendering helpers.
//!
//! Uses `pdfium-render` with late dynamic runtime binding. If libpdfium is
//! absent at runtime, functions return `ExtractError::Unsupported` naming the
//! library and instructions rather than panicking.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::OnceLock;

use kb_ports::{ExtractionResult, PageText};
use pdfium_render::prelude::*;

use crate::ExtractError;

static PDFIUM: OnceLock<Result<Pdfium, String>> = OnceLock::new();

/// Minimal valid 1-page PDF fixture containing text "Hello World".
pub const MINIMAL_PDF_HELLO_WORLD: &[u8] = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /MediaBox [0 0 612 792] /Contents 5 0 R >> endobj
4 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
5 0 obj << /Length 44 >> stream
BT /F1 24 Tf 100 700 Td (Hello World) Tj ET
endstream endobj
xref
0 6
0000000000 65535 f 
0000000009 00000 n 
0000000058 00000 n 
0000000115 00000 n 
0000000244 00000 n 
0000000325 00000 n 
trailer << /Size 6 /Root 1 0 R >>
startxref
419
%%EOF
";

/// Minimal valid 1-page scanned/blank PDF fixture containing no text.
pub const MINIMAL_BLANK_PDF: &[u8] = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >> endobj
xref
0 4
0000000000 65535 f 
0000000009 00000 n 
0000000058 00000 n 
0000000115 00000 n 
trailer << /Size 4 /Root 1 0 R >>
startxref
190
%%EOF
";

/// Resolve candidate paths for libpdfium.
#[must_use]
pub fn find_pdfium_library_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PDFIUM_DYNAMIC_LIB_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if p.exists() {
                return Some(p);
            }
        }
    }

    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let manifest_path = PathBuf::from(manifest);
        for ancestor in manifest_path.ancestors() {
            let candidate = ancestor.join(".vendor/pdfium/lib/libpdfium.so");
            if candidate.exists() {
                return Some(candidate);
            }
            let candidate_rust = ancestor.join("rust/.vendor/pdfium/lib/libpdfium.so");
            if candidate_rust.exists() {
                return Some(candidate_rust);
            }
        }
    }

    let candidates = [
        "rust/.vendor/pdfium/lib/libpdfium.so",
        ".vendor/pdfium/lib/libpdfium.so",
        "../.vendor/pdfium/lib/libpdfium.so",
        "../../.vendor/pdfium/lib/libpdfium.so",
        "/usr/lib/libpdfium.so",
        "/usr/local/lib/libpdfium.so",
        "/opt/pdfium/lib/libpdfium.so",
    ];

    for c in candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }

    None
}

/// Initialize a new `Pdfium` binding.
pub fn init_pdfium() -> Result<Pdfium, ExtractError> {
    if let Some(path) = find_pdfium_library_path() {
        let bindings = Pdfium::bind_to_library(path).map_err(|e| {
            ExtractError::Unsupported(format!("Failed to bind to libpdfium at found path: {e}"))
        })?;
        Ok(Pdfium::new(bindings))
    } else {
        match Pdfium::bind_to_system_library() {
            Ok(bindings) => Ok(Pdfium::new(bindings)),
            Err(e) => Err(ExtractError::Unsupported(format!(
                "libpdfium library could not be loaded: {e}. Set PDFIUM_DYNAMIC_LIB_PATH or run rust/scripts/fetch-pdfium.sh."
            ))),
        }
    }
}

/// Get or initialize the shared `Pdfium` runtime.
pub fn get_pdfium() -> Result<&'static Pdfium, ExtractError> {
    let res = PDFIUM.get_or_init(|| init_pdfium().map_err(|e| e.to_string()));
    match res {
        Ok(pdfium) => Ok(pdfium),
        Err(err) => Err(ExtractError::Unsupported(err.clone())),
    }
}

/// Extract text layer and page map from a PDF document.
pub fn extract_pdf_text_layer(
    pdfium: &Pdfium,
    pdf_bytes: &[u8],
) -> Result<ExtractionResult, ExtractError> {
    let document = pdfium
        .load_pdf_from_byte_slice(pdf_bytes, None)
        .map_err(|e| ExtractError::Container(format!("failed to load PDF: {e}")))?;

    let mut pages = Vec::new();
    let mut page_map = Vec::new();

    for (i, page) in document.pages().iter().enumerate() {
        let text_obj = page
            .text()
            .map_err(|e| ExtractError::Container(format!("failed to read page text: {e}")))?;
        let page_text = text_obj.all();
        let trimmed = page_text.trim();
        if !trimmed.is_empty() {
            page_map.push(PageText {
                page: i as u32,
                text: trimmed.to_string(),
            });
            pages.push(trimmed.to_string());
        }
    }

    let content = pages.join("\n\n");
    let map_opt = if page_map.is_empty() {
        None
    } else {
        Some(page_map)
    };

    Ok(ExtractionResult {
        content,
        file_type: "pdf".to_string(),
        used_ocr: false,
        figures: Vec::new(),
        pages_blocks: None,
        page_map: map_opt,
        error: None,
    })
}

/// Get the number of pages in a PDF document.
pub fn get_pdf_page_count(pdfium: &Pdfium, pdf_bytes: &[u8]) -> Result<u32, ExtractError> {
    let document = pdfium
        .load_pdf_from_byte_slice(pdf_bytes, None)
        .map_err(|e| ExtractError::Container(format!("failed to load PDF: {e}")))?;
    Ok(document.pages().len() as u32)
}

/// Split a PDF document into multiple PDF byte segments by page count.
pub fn split_pdf_by_page_count(
    pdfium: &Pdfium,
    pdf_bytes: &[u8],
    segment_pages: u32,
) -> Result<Vec<Vec<u8>>, ExtractError> {
    let document = pdfium
        .load_pdf_from_byte_slice(pdf_bytes, None)
        .map_err(|e| ExtractError::Container(format!("failed to load PDF: {e}")))?;

    let total_pages = document.pages().len() as u32;
    if total_pages == 0 {
        return Ok(Vec::new());
    }

    if total_pages <= segment_pages {
        return Ok(vec![pdf_bytes.to_vec()]);
    }

    let mut segments = Vec::new();
    let mut start = 0u32;

    while start < total_pages {
        let end = (start + segment_pages).min(total_pages);
        let mut new_doc = pdfium
            .create_new_pdf()
            .map_err(|e| ExtractError::Container(format!("failed to create new PDF: {e}")))?;

        // Pages in pdfium-render copy_page_range_from_document: RangeInclusive<i32>
        let range = (start as i32)..=((end - 1) as i32);
        new_doc
            .pages_mut()
            .copy_page_range_from_document(&document, range, 0)
            .map_err(|e| ExtractError::Container(format!("failed to copy page range: {e}")))?;

        let mut cursor = Cursor::new(Vec::new());
        new_doc
            .save_to_writer(&mut cursor)
            .map_err(|e| ExtractError::Container(format!("failed to save PDF segment: {e}")))?;

        segments.push(cursor.into_inner());
        start = end;
    }

    Ok(segments)
}

/// Render a 0-based page of a PDF document to PNG image bytes.
pub fn render_pdf_page_to_png(
    pdfium: &Pdfium,
    pdf_bytes: &[u8],
    page_index: u32,
) -> Result<Vec<u8>, ExtractError> {
    let document = pdfium
        .load_pdf_from_byte_slice(pdf_bytes, None)
        .map_err(|e| ExtractError::Container(format!("failed to load PDF: {e}")))?;

    let pages = document.pages();
    let page = pages
        .get(page_index as u16 as i32)
        .map_err(|e| ExtractError::Container(format!("failed to get page {page_index}: {e}")))?;

    // 150 DPI render config for OCR
    let render_config = PdfRenderConfig::new().set_target_width(1200);
    let bitmap = page
        .render_with_config(&render_config)
        .map_err(|e| ExtractError::Container(format!("failed to render page: {e}")))?;

    let dyn_img = bitmap
        .as_image()
        .map_err(|e| ExtractError::Container(format!("failed to convert bitmap to image: {e}")))?;

    let mut png_bytes = Vec::new();
    let mut cursor = Cursor::new(&mut png_bytes);
    dyn_img
        .write_to(&mut cursor, image::ImageFormat::Png)
        .map_err(|e| ExtractError::Container(format!("failed to encode PNG: {e}")))?;

    Ok(png_bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn can_find_pdfium_library_or_skip() {
        let path = find_pdfium_library_path();
        if let Some(p) = path {
            println!("Found libpdfium at {}", p.display());
            assert!(p.exists());
        }
    }

    #[test]
    fn extract_hello_world_text_layer() {
        let Ok(pdfium) = get_pdfium() else {
            eprintln!("Skipping extract_hello_world_text_layer: libpdfium not found");
            return;
        };

        let res = extract_pdf_text_layer(pdfium, MINIMAL_PDF_HELLO_WORLD).unwrap();
        assert!(res.content.contains("Hello World"));
        assert_eq!(res.file_type, "pdf");
        assert!(!res.used_ocr);
        assert_eq!(res.page_map.as_ref().unwrap().len(), 1);
        assert_eq!(res.page_map.as_ref().unwrap()[0].page, 0);
        assert_eq!(res.page_map.as_ref().unwrap()[0].text, "Hello World");
    }

    #[test]
    fn extract_blank_pdf_yields_empty_content_and_none_page_map() {
        let Ok(pdfium) = get_pdfium() else {
            eprintln!("Skipping test: libpdfium not found");
            return;
        };

        let res = extract_pdf_text_layer(pdfium, MINIMAL_BLANK_PDF).unwrap();
        assert!(res.content.is_empty());
        assert!(res.page_map.is_none());
    }
}
