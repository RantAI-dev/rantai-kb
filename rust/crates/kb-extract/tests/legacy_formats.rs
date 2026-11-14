#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The legacy Office formats and TIFF, end to end through the public
//! extractor, on files written by other software (LibreOffice, Microsoft
//! Word 6/95, Pillow) rather than bytes built by the parsers' own tests.

use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/legacy")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn extract_ok(name: &str) -> String {
    let mime = kb_extract::ext_to_mime(&kb_extract::extension_of(name)).unwrap_or("");
    let result = kb_extract::extract(name, mime, &fixture(name)).unwrap();
    assert_eq!(result.error, None, "{name}");
    result.content
}

#[test]
fn a_word_97_document_reads_paragraphs_punctuation_and_table_cells() {
    let text = extract_ok("legacy-sample.doc");
    assert_eq!(
        text,
        "Quarterly Water Report\n\n\
         The naïve café served “quoted” text — with an em dash and €42.\n\n\
         Laporan triwulan: kualitas air di Kabupaten contoh memenuhi baku mutu.\n\n\
         See the full report for details.\n\n\
         Region\n\nSamples\n\nNorth\n\n128\n\n\
         Closing paragraph after the table."
    );
}

#[test]
fn a_powerpoint_97_deck_reads_slides_in_order_then_notes() {
    let text = extract_ok("legacy-sample.ppt");
    assert_eq!(
        text,
        "First slide title\nBody bullet one\nBody bullet two\n\
         Second slide title\nA free text box with café\n\
         Speaker note for slide one"
    );
}

#[test]
fn an_excel_97_workbook_reads_as_a_markdown_table() {
    let text = extract_ok("table.xls");
    assert!(
        text.ends_with(
            "[Sheet: table]\n\
             | Region | Samples | Compliant |\n\
             | --- | --- | --- |\n\
             | North | 128 | yes |\n\
             | South | 96 | no |"
        ),
        "{text}"
    );
}

#[test]
fn word_6_and_word_95_documents_are_read() {
    assert_eq!(
        extract_ok("poi-Word6.doc"),
        "The quick brown fox jumps over the lazy dog"
    );
    let text = extract_ok("poi-Word95.doc");
    assert!(
        text.starts_with("The quick brown fox jumps over the lazy dog\n\nParagraph 2"),
        "{text}"
    );
}

#[test]
fn malformed_compound_files_fail_with_a_reason_not_a_panic() {
    for (name, label) in [("poi-fuzz-1.ppt", "PowerPoint"), ("poi-fuzz-2.doc", "Word")] {
        let mime = kb_extract::ext_to_mime(&kb_extract::extension_of(name)).unwrap();
        let result = kb_extract::extract(name, mime, &fixture(name)).unwrap();
        assert_eq!(result.content, "", "{name}");
        let error = result.error.expect("a reason");
        assert!(error.contains(label), "{name}: {error}");
    }
}

#[test]
fn fax_tiffs_decode_every_page_pixel_for_pixel() {
    for name in ["fax-group3.tif", "fax-group4.tif"] {
        let pages = kb_extract::tiff_pages::pages_as_png(&fixture(name)).unwrap();
        assert_eq!(pages.len(), 2, "{name}");
        for (index, png) in pages.iter().enumerate() {
            let got = image::load_from_memory(png).unwrap().to_luma8();
            let want = image::load_from_memory(&fixture(&format!("fax-page-{}.png", index + 1)))
                .unwrap()
                .to_luma8();
            assert_eq!(
                got.dimensions(),
                want.dimensions(),
                "{name} page {}",
                index + 1
            );
            assert!(
                got == want,
                "{name} page {} differs from the page drawn",
                index + 1
            );
        }
    }
}

#[test]
fn a_tiff_without_a_vision_provider_says_so() {
    let err =
        kb_extract::extract("scan.tiff", "image/tiff", &fixture("fax-group4.tif")).unwrap_err();
    assert!(err.to_string().contains("vision provider"), "{err}");
}
