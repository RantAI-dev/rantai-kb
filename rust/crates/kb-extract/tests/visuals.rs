#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `LocalVisualCollector` on files written by other software (python-pptx,
//! LibreOffice Writer) and by hand to the published layouts (EPUB, XLSX, ODS).
//! `tests/fixtures/visuals/make.py` rebuilds every fixture.

use std::path::PathBuf;

use kb_extract::LocalVisualCollector;
use kb_ports::{Visual, VisualCollector, VisualOptions, VisualOrigin, VisualScan};

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/visuals")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

async fn scan(name: &str, options: VisualOptions) -> VisualScan {
    LocalVisualCollector
        .collect(name, "", &fixture(name), options)
        .await
        .unwrap()
}

/// `(page, original size, alt text)` per visual, after checking each is a
/// bounded JPEG that decodes.
fn summary(scan: &VisualScan) -> Vec<(u32, (u32, u32), Option<&str>)> {
    scan.visuals
        .iter()
        .map(|v: &Visual| {
            assert_eq!(v.mime, "image/jpeg");
            let img = image::load_from_memory(&v.bytes).unwrap();
            assert!(
                img.width().max(img.height()) <= 1568,
                "{}x{}",
                img.width(),
                img.height()
            );
            (v.page, (v.width, v.height), v.alt_text.as_deref())
        })
        .collect()
}

#[tokio::test]
async fn pptx_pictures_carry_their_slide_and_alt_text() {
    let scan = scan("deck.pptx", VisualOptions::default()).await;
    assert!(
        scan.visuals
            .iter()
            .all(|v| v.origin == VisualOrigin::Embedded)
    );
    assert_eq!(
        summary(&scan),
        vec![
            (0, (800, 600), Some("A red square on slide one")),
            (1, (2400, 1200), None),
            (1, (300, 200), None),
        ]
    );
}

#[tokio::test]
async fn docx_from_libreoffice_reads_both_pictures_and_the_one_alt_text() {
    let scan = scan("report.docx", VisualOptions::default()).await;
    assert_eq!(
        summary(&scan),
        vec![
            (0, (800, 600), Some("A red square in the report")),
            (0, (2400, 1200), None),
        ]
    );
    // The big one was shrunk to the longest side, keeping its aspect.
    let big = image::load_from_memory(&scan.visuals[1].bytes).unwrap();
    assert_eq!((big.width(), big.height()), (1568, 784));
}

#[tokio::test]
async fn odt_from_libreoffice_reads_svg_title_as_alt_text() {
    let scan = scan("report.odt", VisualOptions::default()).await;
    assert_eq!(
        summary(&scan),
        vec![
            (0, (800, 600), Some("A red square in the report")),
            (0, (2400, 1200), None),
        ]
    );
}

#[tokio::test]
async fn ods_pictures_sit_on_their_sheet_with_title_and_description() {
    let scan = scan("sheets.ods", VisualOptions::default()).await;
    assert_eq!(
        summary(&scan),
        vec![(
            1,
            (800, 600),
            Some("Red tile. A red tile on the second sheet")
        )]
    );
}

#[tokio::test]
async fn xlsx_pictures_sit_on_their_sheet_and_emf_is_counted_not_decoded() {
    let scan = scan("sheets.xlsx", VisualOptions::default()).await;
    assert_eq!(
        summary(&scan),
        vec![(1, (300, 200), Some("Green field photo"))]
    );
    assert_eq!(scan.skipped.unsupported_format, 1);
}

#[tokio::test]
async fn epub_images_come_in_spine_order_and_remote_ones_are_skipped() {
    let scan = scan("book.epub", VisualOptions::default()).await;
    assert_eq!(
        summary(&scan),
        vec![
            (0, (800, 600), Some("Red & bold cover")),
            (0, (300, 200), None),
        ]
    );
}

#[tokio::test]
async fn pdf_image_objects_only_when_asked() {
    assert!(
        scan("report.pdf", VisualOptions::default())
            .await
            .visuals
            .is_empty()
    );
    if kb_extract::get_pdfium().is_err() {
        eprintln!("libpdfium not found; skipping the PDF image-object half");
        return;
    }
    let opts = VisualOptions {
        pdf_images: true,
        max_visuals: 0,
    };
    let scan = scan("report.pdf", opts).await;
    let sizes: Vec<(u32, u32)> = scan.visuals.iter().map(|v| (v.width, v.height)).collect();
    // The raw image objects, at the resolution the PDF stores them: Writer's
    // PDF export downsampled the 2400 x 1200 picture to 1250 x 625.
    assert_eq!(sizes, vec![(800, 600), (1250, 625)]);
    assert!(scan.visuals.iter().all(|v| v.page == 0 && v.bbox.is_some()));
    summary(&scan);
    // The cap stops collecting.
    let capped = LocalVisualCollector
        .collect(
            "report.pdf",
            "",
            &fixture("report.pdf"),
            VisualOptions {
                pdf_images: true,
                max_visuals: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(capped.visuals.len(), 1);
}

#[tokio::test]
async fn a_standalone_image_is_one_upload_and_normalize_reads_crops() {
    let scan = scan("big.png", VisualOptions::default()).await;
    assert_eq!(scan.visuals.len(), 1);
    assert_eq!(scan.visuals[0].origin, VisualOrigin::Upload);
    assert_eq!(summary(&scan), vec![(0, (2400, 1200), None)]);

    let n = LocalVisualCollector
        .normalize(&fixture("big.png"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (n.width, n.height, n.mime.as_str()),
        (2400, 1200, "image/jpeg")
    );
    assert!(
        LocalVisualCollector
            .normalize(b"no")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_package_that_is_not_a_zip_has_no_visuals_not_an_error() {
    let scan = LocalVisualCollector
        .collect("broken.docx", "", b"not a zip", VisualOptions::default())
        .await
        .unwrap();
    assert_eq!(scan, VisualScan::default());
}
