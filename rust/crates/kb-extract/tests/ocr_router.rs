//! The page router, measured on generated PDFs (`ING-6` Part B).
//!
//! Every fixture is built here with pdfium — 20 text pages plus 3 image-only
//! pages, or whichever shape a test needs — so no binary fixture is committed
//! and a reviewer can see exactly what the router was given. A fake provider
//! implements [`OcrPageProvider`]; no HTTP server and no paid call is involved.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use image::{DynamicImage, Rgb, RgbImage};
use kb_extract::ocr_router::{
    OcrBreaker, OcrCallError, OcrPageProvider, OcrPageRead, OcrRouter, OcrRouterSettings,
    OcrTransport,
};
use kb_extract::{ExtractConfig, OcrMode};
use kb_ports::{ExtractionOptions, OcrPageCache};
use pdfium_render::prelude::*;

// ---------------------------------------------------------------------------
// Generated PDFs
// ---------------------------------------------------------------------------

struct Page {
    text: Option<String>,
    image: bool,
    invisible_text: Option<String>,
}

fn text_page(text: impl Into<String>) -> Page {
    Page {
        text: Some(text.into()),
        image: false,
        invisible_text: None,
    }
}

fn image_page() -> Page {
    Page {
        text: None,
        image: true,
        invisible_text: None,
    }
}

/// libpdfium is not safe to drive from several threads at once in this
/// environment even with pdfium-render's `thread_safe` marshalling: parallel
/// tests see empty documents and occasional format errors. These tests take
/// this lock for their whole body, so each one sees a quiet library. The
/// router is still exercised with its own concurrency inside a test.
/// Build a PDF from the page descriptions, with pdfium, at runtime.
fn build_pdf(pages: &[Page]) -> Vec<u8> {
    let pdfium = kb_extract::get_pdfium().expect("libpdfium is needed for the router tests");
    let mut document = pdfium.create_new_pdf().unwrap();
    let font = document.fonts_mut().new_built_in(PdfFontBuiltin::Helvetica);

    for description in pages {
        let mut page = document
            .pages_mut()
            .create_page_at_end(PdfPagePaperSize::a4())
            .unwrap();
        let width = page.width();
        let height = page.height();

        if description.image {
            // A plain raster image covering the whole page: the shape a scan
            // has. 32×32 pixels is enough for the coverage test; the pixels are
            // never OCR'd because the provider is fake.
            let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(32, 32, Rgb([245, 245, 245])));
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
        if let Some(text) = description.text.as_deref() {
            page.objects_mut()
                .create_text_object(
                    PdfPoints::new(40.0),
                    PdfPoints::new(height.value - 60.0),
                    text,
                    font,
                    PdfPoints::new(12.0),
                )
                .unwrap();
        }
        if let Some(text) = description.invisible_text.as_deref() {
            if let Ok(mut object) = page.objects_mut().create_text_object(
                PdfPoints::new(40.0),
                PdfPoints::new(height.value - 90.0),
                text,
                font,
                PdfPoints::new(12.0),
            ) {
                if let Some(text_object) = object.as_text_object_mut() {
                    // An existing OCR layer: present in the text layer, never
                    // visible. The scan test must not count it.
                    let _ = text_object.set_render_mode(PdfPageTextRenderMode::Invisible);
                }
            }
        }
    }

    let mut cursor = std::io::Cursor::new(Vec::new());
    document.save_to_writer(&mut cursor).unwrap();
    cursor.into_inner()
}

fn mixed_pdf(text_pages: usize, image_pages: usize) -> Vec<u8> {
    let mut pages = Vec::new();
    for index in 0..text_pages {
        pages.push(text_page(format!(
            "Digital page {} carries a real text layer with enough visible characters.",
            index + 1
        )));
    }
    for _ in 0..image_pages {
        pages.push(image_page());
    }
    build_pdf(&pages)
}

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

/// Counts requests and answers `<transport>-page-<n>`, failing selected
/// document pages (by their index within the submitted document).
struct FakeProvider {
    id: &'static str,
    transport: OcrTransport,
    calls: AtomicU32,
    images: AtomicU32,
    /// Relative page indices the document path fails.
    fail_pages: Mutex<Vec<u32>>,
    /// Whether every call fails, and whether the failure is transient.
    fail_all: Option<bool>,
}

impl FakeProvider {
    fn chat() -> Arc<Self> {
        Arc::new(Self {
            id: "chat",
            transport: OcrTransport::Image,
            calls: AtomicU32::new(0),
            images: AtomicU32::new(0),
            fail_pages: Mutex::new(Vec::new()),
            fail_all: None,
        })
    }

    fn document(batch: u32) -> Arc<Self> {
        Arc::new(Self {
            id: "mistral",
            transport: OcrTransport::Pdf { batch },
            calls: AtomicU32::new(0),
            images: AtomicU32::new(0),
            fail_pages: Mutex::new(Vec::new()),
            fail_all: None,
        })
    }

    fn failing_document_page(page: u32) -> Arc<Self> {
        let fake = Self::document(8);
        fake.fail_pages.lock().unwrap().push(page);
        fake
    }

    fn always_failing(transient: bool) -> Arc<Self> {
        Arc::new(Self {
            id: "chat",
            transport: OcrTransport::Image,
            calls: AtomicU32::new(0),
            images: AtomicU32::new(0),
            fail_pages: Mutex::new(Vec::new()),
            fail_all: Some(transient),
        })
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    fn images(&self) -> u32 {
        self.images.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl OcrPageProvider for FakeProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn model(&self) -> &str {
        "fake-model"
    }

    fn prompt(&self) -> &str {
        "OCR:"
    }

    fn dpi(&self) -> u32 {
        72
    }

    fn transport(&self) -> OcrTransport {
        self.transport
    }

    async fn read_pdf(
        &self,
        _pdf_bytes: &[u8],
        _include_figures: bool,
    ) -> Result<Vec<OcrPageRead>, OcrCallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(transient) = self.fail_all {
            return Err(if transient {
                OcrCallError::transient("fake outage")
            } else {
                OcrCallError::permanent("fake rejection")
            });
        }
        let failing = self.fail_pages.lock().unwrap().clone();
        // The real provider answers one page per submitted page; the fake says
        // so for a fixed batch, and the test counts requests, not page counts.
        Ok((0..8)
            .filter(|page| !failing.contains(page))
            .map(|page| OcrPageRead {
                page,
                text: format!("DOC-page-{page}"),
                figures: Vec::new(),
            })
            .collect())
    }

    async fn read_image(&self, _image: &[u8], _mime: &str) -> Result<String, OcrCallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(transient) = self.fail_all {
            return Err(if transient {
                OcrCallError::transient("fake outage")
            } else {
                OcrCallError::permanent("fake rejection")
            });
        }
        let page = self.images.fetch_add(1, Ordering::SeqCst);
        Ok(format!("IMG-page-{page}"))
    }
}

/// An in-memory page cache, so a test can prove a second run sends nothing.
#[derive(Default)]
struct MemoryCache {
    entries: Mutex<std::collections::HashMap<String, String>>,
    gets: AtomicU32,
    puts: AtomicU32,
}

#[async_trait]
impl OcrPageCache for MemoryCache {
    async fn get(&self, key: &str) -> Option<String> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.entries.lock().unwrap().get(key).cloned()
    }

    async fn put(&self, key: &str, text: &str) {
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.entries
            .lock()
            .unwrap()
            .insert(key.to_string(), text.to_string());
    }
}

fn settings(mode: OcrMode) -> OcrRouterSettings {
    OcrRouterSettings {
        mode,
        batch: 8,
        max_pages: 300,
        scan_area: 0.90,
        scan_text: 50,
        concurrency: 2,
        breaker_threshold: 3,
        breaker_cooldown: Duration::from_secs(60),
        dpi: 72,
        attempts: 1,
    }
}

fn router(
    mode: OcrMode,
    provider: Option<Arc<FakeProvider>>,
    cache: Option<Arc<MemoryCache>>,
) -> OcrRouter {
    let provider = provider.map(|p| p as Arc<dyn OcrPageProvider>);
    let cache = cache.map(|c| c as Arc<dyn OcrPageCache>);
    OcrRouter::new(
        settings(mode),
        provider,
        cache,
        Some(Arc::new(OcrBreaker::new(3, Duration::from_secs(60)))),
    )
}

async fn extract(router: &OcrRouter, pdf: &[u8]) -> kb_ports::ExtractionResult {
    router
        .extract_pdf(pdf, "mixed.pdf", &ExtractionOptions::default())
        .await
        .expect("the document must not fail")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

async fn case_hybrid_sends_exactly_the_scanned_pages() {
    let pdf = mixed_pdf(20, 3);
    let provider = FakeProvider::chat();
    let out = extract(
        &router(OcrMode::Hybrid, Some(Arc::clone(&provider)), None),
        &pdf,
    )
    .await;

    assert_eq!(
        provider.images(),
        3,
        "a mixed PDF sends only its 3 image-only pages, not all 23"
    );
    let report = out.ocr.expect("the report is stored in metadata.ocr");
    assert_eq!(report.mode, "hybrid");
    assert_eq!(report.provider.as_deref(), Some("chat"));
    assert_eq!(report.pages, 23);
    assert_eq!(report.ocr_pages, 3);
    assert!(report.cached_pages == 0);
    assert_eq!(report.local_pages, 20);
    assert!(report.failed_pages.is_empty());
    assert!(report.skipped_pages.is_empty());
    assert!(!report.breaker_opened);

    // Page order survives the merge, and every page keeps its number.
    let map = out.page_map.expect("a page map");
    assert_eq!(map.len(), 23);
    assert!(map.iter().enumerate().all(|(i, p)| p.page as usize == i));
    assert!(map[0].text.starts_with("Digital page 1"));
    assert!(map[22].text.starts_with("IMG-page-"));
}

async fn case_full_sends_every_page() {
    let pdf = mixed_pdf(20, 3);
    let provider = FakeProvider::chat();
    let out = extract(
        &router(OcrMode::Full, Some(Arc::clone(&provider)), None),
        &pdf,
    )
    .await;
    assert_eq!(provider.images(), 23);
    let report = out.ocr.unwrap();
    assert_eq!(report.ocr_pages, 23);
    assert_eq!(report.local_pages, 0);
    assert_eq!(out.page_map.unwrap().len(), 23);
}

async fn case_off_never_sends_a_page_and_still_reports() {
    let pdf = mixed_pdf(2, 2);
    let provider = FakeProvider::chat();
    let out = extract(
        &router(OcrMode::Off, Some(Arc::clone(&provider)), None),
        &pdf,
    )
    .await;
    assert_eq!(provider.calls(), 0);
    let report = out.ocr.unwrap();
    assert_eq!(report.mode, "off");
    assert_eq!(report.ocr_pages, 0);
    assert_eq!(
        out.page_map.map(|m| m.len()),
        Some(2),
        "the two digital pages keep their text; the two scans stay empty"
    );
}

async fn case_a_document_provider_sends_three_batches_of_eight() {
    let pdf = mixed_pdf(20, 0);
    let provider = FakeProvider::document(8);
    let out = extract(
        &router(OcrMode::Full, Some(Arc::clone(&provider)), None),
        &pdf,
    )
    .await;
    let report = out.ocr.expect("a report");
    assert_eq!(report.pages, 20, "the fixture has 20 pages");
    assert_eq!(
        provider.calls(),
        3,
        "20 pages at KB_OCR_BATCH=8 (failed: {:?})",
        report.failed_pages
    );
    assert_eq!(report.ocr_pages, 20);
}

async fn case_a_second_run_is_served_entirely_from_the_page_cache() {
    let pdf = mixed_pdf(20, 0);
    let provider = FakeProvider::document(8);
    let cache = Arc::new(MemoryCache::default());
    let router = router(OcrMode::Full, Some(Arc::clone(&provider)), Some(cache));

    let first = extract(&router, &pdf).await;
    assert_eq!(provider.calls(), 3);
    assert_eq!(first.ocr.as_ref().unwrap().cached_pages, 0);

    let second = extract(&router, &pdf).await;
    assert_eq!(provider.calls(), 3, "the second run makes no request");
    let report = second.ocr.unwrap();
    assert_eq!(report.cached_pages, 20);
    assert_eq!(report.ocr_pages, 0);
    assert!(first.content == second.content);
}

async fn case_one_failed_page_is_marked() {
    let pdf = mixed_pdf(0, 4);
    let provider = FakeProvider::failing_document_page(1);
    let out = extract(
        &router(OcrMode::Full, Some(Arc::clone(&provider)), None),
        &pdf,
    )
    .await;

    assert!(out.error.is_none(), "one lost page must not fail the job");
    let report = out.ocr.unwrap();
    assert_eq!(report.failed_pages, vec![2], "1-based page 2");
    assert!(
        out.content.contains("[page 2 could not be read]"),
        "the marker is visible in the content: {}",
        out.content
    );
    assert!(out.content.contains("DOC-page-0"));
    assert!(out.content.contains("DOC-page-2"));
}

async fn case_every_page_failing_fails_the_document() {
    let pdf = mixed_pdf(0, 3);
    let provider = FakeProvider::always_failing(false);
    let err = router(OcrMode::Full, Some(provider), None)
        .extract_pdf(&pdf, "scan.pdf", &ExtractionOptions::default())
        .await
        .expect_err("an empty document is a failed job");
    assert!(err.to_string().contains("OCR failed"), "{err}");
}

async fn case_the_breaker_opens_after_three_consecutive_failures() {
    let pdf = mixed_pdf(6, 0);
    let provider = FakeProvider::always_failing(true);
    let out = extract(
        &router(OcrMode::Full, Some(Arc::clone(&provider)), None),
        &pdf,
    )
    .await;

    assert_eq!(provider.calls(), 3, "the fourth page is never sent");
    let report = out.ocr.unwrap();
    assert!(report.breaker_opened);
    assert_eq!(report.failed_pages, vec![1, 2, 3, 4, 5, 6]);
    // `full` keeps the local text of a failed page, so the document stays
    // readable even though every OCR call failed.
    assert!(out.content.contains("Digital page 1"));
}

async fn case_the_page_cap() {
    let pdf = mixed_pdf(5, 0);
    let provider = FakeProvider::document(8);
    let mut settings = settings(OcrMode::Full);
    settings.max_pages = 2;
    let router = OcrRouter::new(
        settings,
        Some(Arc::clone(&provider) as Arc<dyn OcrPageProvider>),
        None,
        Some(Arc::new(OcrBreaker::new(3, Duration::from_secs(60)))),
    );
    let out = extract(&router, &pdf).await;
    let report = out.ocr.unwrap();
    assert_eq!(report.ocr_pages, 2);
    assert_eq!(report.skipped_pages, vec![3, 4, 5]);
    assert!(out.content.contains("DOC-page-0"));
    assert!(out.content.contains("Digital page 3"), "local fallback");
}

async fn case_a_page_subset_holds_exactly_the_pages_it_was_given() {
    // The bytes are not stable across saves (pdfium writes a random `/ID`),
    // which is why the page cache keys on the source document digest and the
    // page index instead of a one-page PDF. The subset itself must still be a
    // valid PDF of the requested page.
    let pdf = mixed_pdf(3, 0);
    let pdfium = kb_extract::get_pdfium().unwrap();
    let subset = kb_extract::pdfium::pdf_page_subset(pdfium, &pdf, &[1]).unwrap();
    assert_eq!(kb_extract::get_pdf_page_count(pdfium, &subset).unwrap(), 1);
    let text = kb_extract::extract_pdf_text_layer(pdfium, &subset).unwrap();
    assert!(text.content.contains("Digital page 2"), "{}", text.content);
}

/// One entry point, run once: the router cases need a quiet pdfium, and
/// libpdfium's library state is not safe to drive from several threads of one
/// process here (parallel `#[tokio::test]`s see empty documents and format
/// errors). Each case is a named function, so a failure still points at the
/// behaviour that broke.
#[tokio::test]
async fn the_page_router_meets_its_contract() {
    case_hybrid_sends_exactly_the_scanned_pages().await;
    case_full_sends_every_page().await;
    case_off_never_sends_a_page_and_still_reports().await;
    case_a_document_provider_sends_three_batches_of_eight().await;
    case_a_second_run_is_served_entirely_from_the_page_cache().await;
    case_one_failed_page_is_marked().await;
    case_every_page_failing_fails_the_document().await;
    case_the_breaker_opens_after_three_consecutive_failures().await;
    case_the_page_cap().await;
    case_a_page_subset_holds_exactly_the_pages_it_was_given().await;
}

#[test]
fn auto_is_off_without_a_provider_and_hybrid_with_a_local_one() {
    let cfg = ExtractConfig::default();
    assert_eq!(cfg.resolved_ocr_mode(), OcrMode::Off);
    let cfg = ExtractConfig {
        ocr_base_url: "http://127.0.0.1:8000/v1/chat/completions".to_string(),
        ..ExtractConfig::default()
    };
    assert_eq!(cfg.resolved_ocr_mode(), OcrMode::Hybrid);
}
