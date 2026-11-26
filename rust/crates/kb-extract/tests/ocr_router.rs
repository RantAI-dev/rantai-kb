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
    OcrBreaker, OcrCallError, OcrPageAnswer, OcrPageProvider, OcrPageRead, OcrRouter,
    OcrRouterSettings, OcrTransport,
};
use kb_extract::{ExtractConfig, ExtractError, OcrMode};
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

/// A page whose glyphs are drawn as paths: no text object, no raster image.
fn vector_page() -> Page {
    Page {
        text: None,
        image: false,
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
    fail_all: Mutex<Option<bool>>,
    /// Image reads in flight now, and the most seen at once.
    in_flight: AtomicU32,
    peak_in_flight: AtomicU32,
    /// How long an image read takes, so concurrent reads overlap.
    image_delay: Duration,
}

impl FakeProvider {
    fn chat() -> Arc<Self> {
        Arc::new(Self {
            id: "chat",
            transport: OcrTransport::Image,
            calls: AtomicU32::new(0),
            images: AtomicU32::new(0),
            fail_pages: Mutex::new(Vec::new()),
            fail_all: Mutex::new(None),
            in_flight: AtomicU32::new(0),
            peak_in_flight: AtomicU32::new(0),
            image_delay: Duration::ZERO,
        })
    }

    fn document(batch: u32) -> Arc<Self> {
        Arc::new(Self {
            id: "mistral",
            transport: OcrTransport::Pdf { batch },
            calls: AtomicU32::new(0),
            images: AtomicU32::new(0),
            fail_pages: Mutex::new(Vec::new()),
            fail_all: Mutex::new(None),
            in_flight: AtomicU32::new(0),
            peak_in_flight: AtomicU32::new(0),
            image_delay: Duration::ZERO,
        })
    }

    fn failing_document_page(page: u32) -> Arc<Self> {
        let fake = Self::document(8);
        fake.fail_pages.lock().unwrap().push(page);
        fake
    }

    fn always_failing(transient: bool) -> Arc<Self> {
        Self::failing_with(OcrTransport::Image, transient)
    }

    fn failing_with(transport: OcrTransport, transient: bool) -> Arc<Self> {
        Arc::new(Self {
            id: "chat",
            transport,
            calls: AtomicU32::new(0),
            images: AtomicU32::new(0),
            fail_pages: Mutex::new(Vec::new()),
            fail_all: Mutex::new(Some(transient)),
            in_flight: AtomicU32::new(0),
            peak_in_flight: AtomicU32::new(0),
            image_delay: Duration::ZERO,
        })
    }

    /// The backend comes back: the next call answers normally.
    fn recover(&self) {
        *self.fail_all.lock().unwrap() = None;
    }

    /// An image provider whose reads take `delay`, to observe parallelism.
    fn slow_chat(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            id: "chat",
            transport: OcrTransport::Image,
            calls: AtomicU32::new(0),
            images: AtomicU32::new(0),
            fail_pages: Mutex::new(Vec::new()),
            fail_all: Mutex::new(None),
            in_flight: AtomicU32::new(0),
            peak_in_flight: AtomicU32::new(0),
            image_delay: delay,
        })
    }

    fn peak_in_flight(&self) -> u32 {
        self.peak_in_flight.load(Ordering::SeqCst)
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
        include_figures: bool,
    ) -> Result<Vec<OcrPageRead>, OcrCallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(transient) = *self.fail_all.lock().unwrap() {
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
                figures: if include_figures && page == 0 {
                    vec![kb_ports::ExtractedFigure {
                        kind: "image".to_string(),
                        page,
                        bbox: [0.0, 0.0, 1.0, 1.0],
                        caption: Some("Gambar 1. Bagan".to_string()),
                        image_bytes: vec![1, 2, 3],
                        id: Some(format!("p{page}-img-0")),
                    }]
                } else {
                    Vec::new()
                },
            })
            .collect())
    }

    async fn read_image(&self, _image: &[u8], _mime: &str) -> Result<OcrPageAnswer, OcrCallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(transient) = *self.fail_all.lock().unwrap() {
            return Err(if transient {
                OcrCallError::transient("fake outage")
            } else {
                OcrCallError::permanent("fake rejection")
            });
        }
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);
        if !self.image_delay.is_zero() {
            tokio::time::sleep(self.image_delay).await;
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        let page = self.images.fetch_add(1, Ordering::SeqCst);
        Ok(OcrPageAnswer::text(format!("IMG-page-{page}")))
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

/// A page whose text layer is not text: the shape a PDF with a broken font map
/// has. Punctuation and digits, no letters at all.
fn broken_text_page() -> Page {
    text_page("§¤¡¿«»¶·¸©® 1234567890 §¤¡¿«»¶·¸©® 1234567890")
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
        quality: None,
    }
}

/// The same settings with the extraction quality gate on (`#29`).
fn settings_with_quality(mode: OcrMode) -> OcrRouterSettings {
    OcrRouterSettings {
        quality: Some(kb_core::quality::QualityOptions::default()),
        ..settings(mode)
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

/// A router with the extraction quality gate on (`#29`).
fn router_with_quality(
    mode: OcrMode,
    provider: Option<Arc<FakeProvider>>,
    cache: Option<Arc<MemoryCache>>,
) -> OcrRouter {
    let provider = provider.map(|p| p as Arc<dyn OcrPageProvider>);
    let cache = cache.map(|c| c as Arc<dyn OcrPageCache>);
    OcrRouter::new(
        settings_with_quality(mode),
        provider,
        cache,
        Some(Arc::new(OcrBreaker::new(3, Duration::from_secs(60)))),
    )
}

/// A router whose breaker is already open: the state a job retried inside the
/// outage cooldown of an earlier job finds (`#35`). The breaker is
/// process-wide, so one document's outage refuses every later document's
/// pages too.
fn router_with_open_breaker(
    mode: OcrMode,
    provider: Option<Arc<FakeProvider>>,
    cache: Option<Arc<MemoryCache>>,
) -> OcrRouter {
    let breaker = Arc::new(OcrBreaker::new(3, Duration::from_secs(60)));
    for _ in 0..3 {
        breaker.record_failure();
    }
    assert!(!breaker.allows(), "the breaker is open");
    let provider = provider.map(|p| p as Arc<dyn OcrPageProvider>);
    let cache = cache.map(|c| c as Arc<dyn OcrPageCache>);
    OcrRouter::new(settings(mode), provider, cache, Some(breaker))
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

/// The quality gate sends a page whose text layer is unusable to OCR, even
/// though the page carries visible characters (`#29`).
///
/// The page is not a scan: it has no image, and it has text. What it has is
/// punctuation and digits — the shape a PDF with a broken font map produces.
/// Without the gate the page is indexed as it is and the document comes out
/// with a handful of junk chunks.
async fn case_the_quality_gate_sends_a_page_with_a_broken_text_layer() {
    let pdf = build_pdf(&[
        text_page(
            "Digital page one carries a real text layer with plenty of words and characters.",
        ),
        broken_text_page(),
    ]);

    // The gate off: the broken page is indexed as it is.
    let quiet = router(OcrMode::Hybrid, Some(FakeProvider::document(8)), None);
    let result = extract(&quiet, &pdf).await;
    let report = result.ocr.expect("the router reports what it did");
    assert!(
        report.reread_pages.is_empty(),
        "without the gate no page is read again: {:?}",
        report.reread_pages
    );

    // The gate on: the broken page goes to the provider.
    let provider = FakeProvider::document(8);
    let router = router_with_quality(OcrMode::Hybrid, Some(provider.clone()), None);
    let result = extract(&router, &pdf).await;
    let report = result.ocr.expect("the router reports what it did");
    assert_eq!(
        report.reread_pages,
        vec![2],
        "the page with the broken text layer is read again by the model"
    );
    assert!(
        provider.calls() > 0,
        "the provider was asked for the broken page"
    );
}

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
    assert!(out.used_ocr, "three pages were read by OCR");
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

/// A page with no extractable text and no raster image — the vector-outline
/// PDF of `#28`. It must still go to OCR: there is nothing else to index.
async fn case_hybrid_sends_a_page_with_no_text_at_all() {
    let pdf = build_pdf(&[vector_page(), vector_page(), vector_page()]);
    let provider = FakeProvider::chat();
    let out = extract(
        &router(OcrMode::Hybrid, Some(Arc::clone(&provider)), None),
        &pdf,
    )
    .await;

    assert_eq!(
        provider.images(),
        3,
        "every text-less page is an OCR candidate, raster coverage or not"
    );
    let report = out.ocr.expect("the report is stored in metadata.ocr");
    assert_eq!(report.ocr_pages, 3);
    assert_eq!(report.pages, 3);
    assert!(out.used_ocr, "the pages were read by OCR");
    assert!(out.content.contains("IMG-page-"), "{}", out.content);
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
    assert!(!out.used_ocr, "nothing was OCR'd");
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

/// `#35`: a job retried inside the outage cooldown of an earlier job finds
/// the process-wide breaker open. The old code skipped every page and then
/// reported "the OCR provider returned no text for any page", which reads
/// like a broken document and hides the real cause. The retry must name the
/// breaker and be retryable.
async fn case_a_retry_inside_the_breaker_cooldown_says_the_breaker_is_open() {
    let pdf = mixed_pdf(0, 3);
    let provider = FakeProvider::chat();
    let cache = Arc::new(MemoryCache::default());
    let err = router_with_open_breaker(
        OcrMode::Full,
        Some(Arc::clone(&provider)),
        Some(Arc::clone(&cache)),
    )
    .extract_pdf(&pdf, "scan.pdf", &ExtractionOptions::default())
    .await
    .expect_err("nothing could be read");

    let message = err.to_string();
    assert!(
        matches!(&err, ExtractError::Unavailable(_)),
        "the job must be retryable, not failed: {err:?}"
    );
    assert!(message.contains("breaker"), "{message}");
    assert!(
        !message.contains("no text for any page"),
        "the old message hid the real cause: {message}"
    );
    assert_eq!(
        provider.calls(),
        0,
        "no page is sent to a provider that is in its cooldown"
    );
    assert_eq!(
        cache.puts.load(Ordering::SeqCst),
        0,
        "a skipped page is not cached"
    );
}

/// `#35`: a transport error (the provider could not be reached) is never
/// cached. A cached empty answer would make every later retry read
/// "nothing" and the document would stay broken for good.
async fn case_a_transport_error_is_never_cached() {
    let pdf = mixed_pdf(0, 1);
    let provider = FakeProvider::always_failing(true);
    let cache = Arc::new(MemoryCache::default());
    let router = router(
        OcrMode::Full,
        Some(Arc::clone(&provider)),
        Some(Arc::clone(&cache)),
    );

    let err = router
        .extract_pdf(&pdf, "scan.pdf", &ExtractionOptions::default())
        .await
        .expect_err("the provider is down");
    let message = err.to_string();
    assert!(
        matches!(&err, ExtractError::Unavailable(_)),
        "an unreachable provider is retryable: {err:?}"
    );
    assert!(message.contains("unreachable"), "{message}");
    assert_eq!(
        cache.puts.load(Ordering::SeqCst),
        0,
        "a failed read is not cached"
    );

    // The backend comes back and the same job runs again: the page is read
    // for real, because nothing was cached under its key.
    provider.recover();
    let out = router
        .extract_pdf(&pdf, "scan.pdf", &ExtractionOptions::default())
        .await
        .expect("the provider is back");
    assert!(out.used_ocr, "the retry really read the page");
    assert!(out.content.contains("IMG-page-"), "{}", out.content);
}

/// The same rule on the document transport: a failed batch is not cached.
async fn case_a_failed_document_batch_is_not_cached() {
    let pdf = mixed_pdf(0, 3);
    let provider = FakeProvider::failing_with(OcrTransport::Pdf { batch: 8 }, true);
    let cache = Arc::new(MemoryCache::default());
    let router = router(
        OcrMode::Full,
        Some(Arc::clone(&provider)),
        Some(Arc::clone(&cache)),
    );

    let err = router
        .extract_pdf(&pdf, "scan.pdf", &ExtractionOptions::default())
        .await
        .expect_err("the provider is down");
    assert!(
        matches!(&err, ExtractError::Unavailable(_)),
        "an unreachable provider is retryable: {err:?}"
    );
    assert_eq!(
        cache.puts.load(Ordering::SeqCst),
        0,
        "a failed batch is not cached"
    );

    provider.recover();
    let out = router
        .extract_pdf(&pdf, "scan.pdf", &ExtractionOptions::default())
        .await
        .expect("the provider is back");
    assert!(out.used_ocr, "the retry really read the pages");
    assert!(out.content.contains("DOC-page-0"), "{}", out.content);
}

async fn case_figures_keep_the_document_page_and_survive_the_cache() {
    let pdf = mixed_pdf(1, 0);
    let provider = FakeProvider::document(8);
    let cache = Arc::new(MemoryCache::default());
    let router = router(OcrMode::Full, Some(Arc::clone(&provider)), Some(cache));
    let options = ExtractionOptions {
        figures: true,
        ..ExtractionOptions::default()
    };
    let first = router.extract_pdf(&pdf, "one.pdf", &options).await.unwrap();
    assert_eq!(first.figures.len(), 1);
    assert_eq!(first.figures[0].page, 0);
    assert_eq!(first.figures[0].id.as_deref(), Some("p0-img-0"));

    // A page cache hit returns the crops too, so a re-upload does not lose a
    // figure it already paid for.
    let second = router
        .extract_pdf(&pdf, "one.pdf", &options)
        .await
        .expect("cached");
    assert_eq!(provider.calls(), 1);
    assert_eq!(second.figures.len(), 1);
    assert_eq!(second.figures[0].image_bytes, vec![1, 2, 3]);
}

async fn case_an_image_upload_uses_the_same_page_path() {
    let provider = FakeProvider::chat();
    let cache = Arc::new(MemoryCache::default());
    let router = router(OcrMode::Hybrid, Some(Arc::clone(&provider)), Some(cache));
    let image = b"\x89PNG\r\n\x1a\n".as_slice();
    let first = router
        .extract_image(image, "image/png", "scan.png")
        .await
        .unwrap();
    assert_eq!(provider.calls(), 1);
    let report = first.ocr.expect("a report");
    assert_eq!(report.pages, 1);
    assert_eq!(report.ocr_pages, 1);
    assert!(first.content.starts_with("IMG-page-"), "{}", first.content);

    let second = router
        .extract_image(image, "image/png", "scan.png")
        .await
        .unwrap();
    assert_eq!(provider.calls(), 1, "the image is read from the page cache");
    assert_eq!(second.ocr.unwrap().cached_pages, 1);
}

async fn case_a_tiff_page_is_one_ocr_page() {
    let provider = FakeProvider::chat();
    let router = router(OcrMode::Hybrid, Some(Arc::clone(&provider)), None);
    let mut tif = std::io::Cursor::new(Vec::new());
    let mut encoder = tiff::encoder::TiffEncoder::new(&mut tif).unwrap();
    for _ in 0..2 {
        encoder
            .write_image::<tiff::encoder::colortype::Gray8>(1, 1, &[128])
            .unwrap();
    }
    let out = router
        .extract_image(&tif.into_inner(), "image/tiff", "fax.tif")
        .await
        .unwrap();
    assert_eq!(provider.calls(), 2, "one call per TIFF page");
    assert_eq!(out.page_map.unwrap().len(), 2);
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

/// `concurrency` pages of one document are read at once, whatever the page
/// count. Before, pages were grouped in runs of `concurrency` read one after
/// another, so 8 pages at concurrency 4 sent only 2 at a time.
async fn case_concurrency_is_the_pages_in_flight() {
    let pdf = mixed_pdf(0, 8);
    let provider = FakeProvider::slow_chat(Duration::from_millis(50));
    let router = OcrRouter::new(
        OcrRouterSettings {
            concurrency: 4,
            ..settings(OcrMode::Full)
        },
        Some(Arc::clone(&provider) as Arc<dyn OcrPageProvider>),
        None,
        Some(Arc::new(OcrBreaker::new(3, Duration::from_secs(60)))),
    );
    let out = extract(&router, &pdf).await;
    assert_eq!(provider.images(), 8, "every page was read");
    assert_eq!(
        provider.peak_in_flight(),
        4,
        "4 pages in flight at concurrency 4, not ceil(8 / 4) = 2"
    );
    assert!(out.used_ocr);
}

/// One entry point, run once: the router cases need a quiet pdfium, and
/// libpdfium's library state is not safe to drive from several threads of one
/// process here (parallel `#[tokio::test]`s see empty documents and format
/// errors). Each case is a named function, so a failure still points at the
/// behaviour that broke.
#[tokio::test]
async fn the_page_router_meets_its_contract() {
    case_the_quality_gate_sends_a_page_with_a_broken_text_layer().await;
    case_hybrid_sends_exactly_the_scanned_pages().await;
    case_hybrid_sends_a_page_with_no_text_at_all().await;
    case_full_sends_every_page().await;
    case_concurrency_is_the_pages_in_flight().await;
    case_off_never_sends_a_page_and_still_reports().await;
    case_a_document_provider_sends_three_batches_of_eight().await;
    case_a_second_run_is_served_entirely_from_the_page_cache().await;
    case_one_failed_page_is_marked().await;
    case_every_page_failing_fails_the_document().await;
    case_the_breaker_opens_after_three_consecutive_failures().await;
    case_a_retry_inside_the_breaker_cooldown_says_the_breaker_is_open().await;
    case_a_transport_error_is_never_cached().await;
    case_a_failed_document_batch_is_not_cached().await;
    case_the_page_cap().await;
    case_an_image_upload_uses_the_same_page_path().await;
    case_a_tiff_page_is_one_ocr_page().await;
    case_figures_keep_the_document_page_and_survive_the_cache().await;
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
