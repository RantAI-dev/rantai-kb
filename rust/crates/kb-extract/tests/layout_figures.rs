//! The layout figure pass (`ING-15`) against a real PDF.
//!
//! The PDF is built here with pdfium. Its figure is a **vector** drawing (path
//! objects, no image object), the kind the PDF image-object scan cannot see,
//! with a caption in the text layer below it. The layout detector is fake: it
//! answers with the boxes a real one gives for that page.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use kb_extract::LayoutFigurePass;
use kb_extract::builder::ProductionExtractor;
use kb_extract::chain::LayoutExtractorChain;
use kb_ports::{
    ExtractionOptions, Extractor, LayoutBox, LayoutDetector, PageLayout, PortError, SeatRelease,
};
use pdfium_render::prelude::*;

/// libpdfium is driven from one test at a time (see `tests/ocr_router.rs`).
static PDFIUM: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// An A4 page: a dark filled rectangle and a diagonal line drawn as vector
/// paths between y = 400 and y = 600 pt, and "Gambar 1.1 Grafik fungsi" below
/// it at y = 380 pt. A second page holds only text.
fn vector_figure_pdf() -> Vec<u8> {
    let pdfium = kb_extract::get_pdfium().expect("libpdfium is needed for these tests");
    let mut document = pdfium.create_new_pdf().unwrap();
    let font = document.fonts_mut().new_built_in(PdfFontBuiltin::Helvetica);

    let mut page = document
        .pages_mut()
        .create_page_at_end(PdfPagePaperSize::a4())
        .unwrap();
    let rect = PdfPagePathObject::new_rect(
        &document,
        PdfRect::new_from_values(400.0, 100.0, 600.0, 400.0),
        Some(PdfColor::new(0, 0, 0, 255)),
        Some(PdfPoints::new(2.0)),
        Some(PdfColor::new(30, 60, 200, 255)),
    )
    .unwrap();
    page.objects_mut().add_path_object(rect).unwrap();
    let line = PdfPagePathObject::new_line(
        &document,
        PdfPoints::new(100.0),
        PdfPoints::new(400.0),
        PdfPoints::new(400.0),
        PdfPoints::new(600.0),
        PdfColor::new(220, 20, 20, 255),
        PdfPoints::new(4.0),
    )
    .unwrap();
    page.objects_mut().add_path_object(line).unwrap();
    write_body(&mut page, font);
    page.objects_mut()
        .create_text_object(
            PdfPoints::new(100.0),
            PdfPoints::new(380.0),
            "Gambar 1.1 Grafik fungsi",
            font,
            PdfPoints::new(12.0),
        )
        .unwrap();

    let mut second = document
        .pages_mut()
        .create_page_at_end(PdfPagePaperSize::a4())
        .unwrap();
    write_body(&mut second, font);

    let mut cursor = std::io::Cursor::new(Vec::new());
    document.save_to_writer(&mut cursor).unwrap();
    cursor.into_inner()
}

/// A line of body text: a page needs about a hundred characters, or the PDF
/// reads as a scan.
const BODY: [&str; 3] = [
    "Bab 1 Fungsi kuadrat. Grafik fungsi kuadrat",
    "berbentuk parabola pada bidang koordinat Kartesius,",
    "dengan sumbu simetri dan titik puncak yang dapat dihitung.",
];

fn write_body(page: &mut PdfPage<'_>, font: PdfFontToken) {
    for (line, text) in BODY.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let y = 780.0 - 16.0 * line as f32;
        page.objects_mut()
            .create_text_object(
                PdfPoints::new(40.0),
                PdfPoints::new(y),
                *text,
                font,
                PdfPoints::new(12.0),
            )
            .unwrap();
    }
}

/// A4 is 595 × 842 pt; the detector's boxes are 0–1000 from the top left.
fn per_mille(x: f64, y_from_bottom: f64) -> (f64, f64) {
    (x / 595.0 * 1000.0, (842.0 - y_from_bottom) / 842.0 * 1000.0)
}

/// Answers like PP-DocLayoutV3: an image and its caption for the page that
/// shows the blue figure, one text region for any other page. Fails every
/// call when `fail`.
struct FakeDetector {
    calls: AtomicU32,
    fail: bool,
}

impl FakeDetector {
    fn new(fail: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicU32::new(0),
            fail,
        })
    }
}

#[async_trait]
impl LayoutDetector for FakeDetector {
    async fn detect(&self, image_bytes: &[u8], mime: &str) -> Result<PageLayout, PortError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(mime, "image/png");
        assert!(!image_bytes.is_empty());
        if self.fail {
            return Err(PortError::Unavailable("layout http 503".to_string()));
        }
        let _ = call;
        if drawn_pixels(image_bytes) == 0 {
            return Ok(PageLayout {
                regions: vec![LayoutBox {
                    label: "text".to_string(),
                    score: 0.9,
                    bbox_2d: [60.0, 50.0, 900.0, 90.0],
                    task_type: "text".to_string(),
                }],
            });
        }
        let (fx1, fy1) = per_mille(100.0, 600.0);
        let (fx2, fy2) = per_mille(400.0, 400.0);
        let (cx1, cy1) = per_mille(95.0, 395.0);
        let (cx2, cy2) = per_mille(300.0, 376.0);
        Ok(PageLayout {
            regions: vec![
                LayoutBox {
                    label: "image".to_string(),
                    score: 0.95,
                    bbox_2d: [fx1, fy1, fx2, fy2],
                    task_type: "skip".to_string(),
                },
                LayoutBox {
                    label: "figure_title".to_string(),
                    score: 0.9,
                    bbox_2d: [cx1, cy1, cx2, cy2],
                    task_type: "text".to_string(),
                },
            ],
        })
    }
}

/// How many pixels of the crop are the figure's blue fill or red line.
fn drawn_pixels(png: &[u8]) -> usize {
    let image = image::load_from_memory(png).unwrap().to_rgb8();
    image
        .pixels()
        .filter(|p| {
            let [r, g, b] = p.0;
            (b > 150 && r < 80) || (r > 180 && g < 80 && b < 80)
        })
        .count()
}

#[tokio::test]
async fn a_vector_figure_is_cropped_from_the_rendered_page_with_its_caption() {
    let _quiet = PDFIUM.lock().await;
    let pdf = vector_figure_pdf();
    let detector = FakeDetector::new(false);
    let pass = LayoutFigurePass::new(detector.clone(), 1000);

    let (figures, report) = pass
        .run(&pdf, &HashSet::new(), &SeatRelease::default())
        .await
        .unwrap();

    assert_eq!(
        detector.calls.load(Ordering::SeqCst),
        2,
        "every page is looked at"
    );
    assert_eq!(report.pages, 2);
    assert_eq!(report.figures, 1);
    assert!(report.failed_pages.is_empty());
    assert_eq!(figures.len(), 1);
    let figure = &figures[0];
    assert_eq!(figure.page, 0);
    assert_eq!(figure.kind, "image");
    assert_eq!(figure.id.as_deref(), Some("p0-layout0"));
    assert_eq!(figure.caption.as_deref(), Some("Gambar 1.1 Grafik fungsi"));
    // Normalised 0–1 from the top left, padded around the 100..400 × 400..600 pt box.
    assert!(
        figure.bbox[0] < 0.17 && figure.bbox[2] > 0.67,
        "{:?}",
        figure.bbox
    );
    assert!(
        figure.bbox[1] < 0.29 && figure.bbox[3] > 0.52,
        "{:?}",
        figure.bbox
    );
    // The crop shows the vector drawing: no image object holds these pixels.
    let drawn = drawn_pixels(&figure.image_bytes);
    assert!(
        drawn > 10_000,
        "the vector figure was not rendered into the crop ({drawn} px)"
    );
}

#[tokio::test]
async fn pages_that_already_have_figures_are_not_cropped_twice() {
    let _quiet = PDFIUM.lock().await;
    let pdf = vector_figure_pdf();
    let detector = FakeDetector::new(false);
    let pass = LayoutFigurePass::new(detector.clone(), 1000);

    let (figures, report) = pass
        .run(&pdf, &HashSet::from([0]), &SeatRelease::default())
        .await
        .unwrap();

    assert_eq!(detector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.pages, 1);
    assert!(figures.is_empty());
}

#[tokio::test]
async fn a_failing_detector_is_reported_not_read_as_no_figures() {
    let _quiet = PDFIUM.lock().await;
    let pdf = vector_figure_pdf();
    let pass = LayoutFigurePass::new(FakeDetector::new(true), 1000);

    let (figures, report) = pass
        .run(&pdf, &HashSet::new(), &SeatRelease::default())
        .await
        .unwrap();

    assert!(figures.is_empty());
    assert_eq!(report.pages, 0);
    assert_eq!(report.failed_pages, vec![1, 2]);
}

#[tokio::test]
async fn the_page_cap_is_reported() {
    let _quiet = PDFIUM.lock().await;
    let pdf = vector_figure_pdf();
    let detector = FakeDetector::new(false);
    let pass = LayoutFigurePass::new(detector.clone(), 1);

    let (figures, report) = pass
        .run(&pdf, &HashSet::new(), &SeatRelease::default())
        .await
        .unwrap();

    assert_eq!(detector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.skipped_pages, 1);
    assert_eq!(figures.len(), 1);
}

#[tokio::test]
async fn the_production_extractor_adds_layout_figures_only_when_figures_are_asked_for() {
    let _quiet = PDFIUM.lock().await;
    let pdf = vector_figure_pdf();
    let extractor = ProductionExtractor::new(LayoutExtractorChain::new(Vec::new(), None), None)
        .with_layout_figures(LayoutFigurePass::new(FakeDetector::new(false), 1000));

    let with = extractor
        .extract_with_options(
            "book.pdf",
            "application/pdf",
            &pdf,
            ExtractionOptions {
                figures: true,
                ..ExtractionOptions::default()
            },
        )
        .await
        .unwrap();
    assert!(with.error.is_none(), "{:?}", with.error);
    assert!(
        with.content.contains("Gambar 1.1"),
        "the text layer is untouched"
    );
    assert_eq!(with.figures.len(), 1);
    assert_eq!(with.layout_figures.as_ref().map(|r| r.figures), Some(1));

    let without = extractor
        .extract_with_options(
            "book.pdf",
            "application/pdf",
            &pdf,
            ExtractionOptions::default(),
        )
        .await
        .unwrap();
    assert!(without.figures.is_empty());
    assert!(without.layout_figures.is_none());
}
