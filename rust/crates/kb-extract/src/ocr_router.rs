//! The page router: which PDF page goes where, and what happens when a page
//! cannot be read (`ING-6`).
//!
//! One PDF page is decided on its own, not the document as a whole:
//!
//! - `KB_OCR_MODE=off` never calls a provider. The page keeps its local text
//!   layer (usually none) and the report says so.
//! - `hybrid` sends a page only when [`page_is_scan`](crate::scanned::page_is_scan)
//!   says it is a scan: raster images cover most of it and it carries almost no
//!   visible text. A mixed PDF therefore keeps its digital pages verbatim and
//!   sends only the scans.
//! - `full` sends every page; the local text is that page's fallback.
//!
//! Pages are merged back in page order with one `page_map` entry per readable
//! page, so a chunk still carries its true page number. A page that fails after
//! its retries becomes `[page N could not be read]` and is listed in
//! `metadata.ocr.failedPages`; the document is not failed for one lost page. A
//! document where no page produced any text at all is failed, because indexing
//! an empty document is worse than retrying it.
//!
//! Three process-wide guards keep the offload cheap and bounded:
//!
//! - a page cache ([`kb_ports::OcrPageCache`]) keyed by provider, model, prompt,
//!   resolution and the page's own bytes, so a re-upload or a retried job pays
//!   only for the pages it has not read yet;
//! - one semaphore shared by every document, so `KB_OCR_CONCURRENCY` is a
//!   ceiling for the process, not per document;
//! - one breaker ([`OcrBreaker`]) that stops sending for
//!   `KB_OCR_BREAKER_COOLDOWN_S` after `KB_OCR_BREAKER` consecutive transient
//!   failures. Pages in that window take their fallback instead.
//!
//! Providers are a trait so the tests can use a fake and the router never names
//! an adapter. The chat adapter takes one rendered page image per request; a
//! document provider (Mistral) takes a sub-PDF of `KB_OCR_BATCH` pages built by
//! pdfium copying pages into a new document.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use kb_core::quality::{QualityOptions, check_page_against_median, count_chars, median_page_chars};
use kb_ports::{
    ExtractedFigure, ExtractionOptions, ExtractionResult, OcrPageCache, OcrReport, PageBlock,
    PageBlockKind, PageText,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::ExtractError;
use crate::config::{ExtractConfig, OcrMode};
use crate::mistral::blocks_from_markdown;
use crate::page_render::{PageRenderSpec, render_image_pages, render_pdf_page_indices_spec};
use crate::pdfium::{get_pdfium, pdf_page_subset};
use crate::scanned::{PageFacts, page_needs_ocr, read_page_facts};

/// One initial attempt plus three retries on a transient provider error.
const MAX_ATTEMPTS: u32 = 4;
/// JPEG quality of the page images a chat provider reads. OCR reads glyph
/// edges, not charts.
const OCR_JPEG_QUALITY: u8 = 85;

/// How a provider wants a page: as one image, or inside a sub-PDF of `batch`
/// pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrTransport {
    /// One rendered page image per request.
    Image,
    /// A sub-PDF of up to `batch` pages per request.
    Pdf { batch: u32 },
}

/// One page's answer from a provider document request.
#[derive(Debug, Clone)]
pub struct OcrPageRead {
    /// Index within the document that was sent (0-based).
    pub page: u32,
    pub text: String,
    pub figures: Vec<ExtractedFigure>,
}

/// What one page read produced.
///
/// A provider that reads a page whole returns the text and nothing else. A
/// provider that also asks a layout detector where the page's regions are
/// says so here, so the report can tell an operator that the layout path ran
/// — and that it failed on some pages, rather than leaving a page silently
/// read the wrong way.
#[derive(Debug, Clone, Default)]
pub struct OcrPageAnswer {
    /// The page's text, as the provider read it.
    pub text: String,
    /// A layout detector answered for this page.
    pub layout: bool,
    /// The layout detector failed for this page, so the page kept the
    /// whole-page answer.
    pub layout_failed: bool,
}

impl OcrPageAnswer {
    /// A page read whole, with no layout detector involved.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            layout: false,
            layout_failed: false,
        }
    }
}

/// What the page cache stores for one page: its text and, when a layout
/// provider read it with figures, its crops. Plain text from an older entry
/// is still accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedPage {
    text: String,
    #[serde(default)]
    figures: Vec<ExtractedFigure>,
}

/// A provider call that failed, with whether retrying it is worthwhile.
///
/// The breaker counts only transient failures: a wrong model name (404) is a
/// configuration mistake, and stopping every other document for a minute would
/// hide it rather than fix it.
#[derive(Debug, Clone)]
pub struct OcrCallError {
    pub message: String,
    pub transient: bool,
}

impl OcrCallError {
    /// A connection error, timeout, 429 or 5xx: worth retrying, and counted by
    /// the breaker.
    #[must_use]
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
        }
    }

    /// A request the provider refuses: not retried, not counted.
    #[must_use]
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
        }
    }
}

/// A page OCR service.
///
/// Implementations do one attempt per call and return the provider's own
/// answer; retrying, the breaker and the cache belong to [`OcrRouter`], so the
/// two adapters cannot drift apart.
#[async_trait]
pub trait OcrPageProvider: Send + Sync {
    /// Stable id used in the cache key and `metadata.ocr.provider`
    /// (`"mistral"`, `"chat"`, …).
    fn id(&self) -> &str;
    fn model(&self) -> &str;
    /// The task prefix or instruction, part of the cache key.
    fn prompt(&self) -> &str;
    /// Resolution used for page images, part of the cache key.
    fn dpi(&self) -> u32;
    /// How the page image is rendered for this provider.
    ///
    /// The default is the JPEG the chat adapter has always sent; a provider
    /// that wants lossless PNG (Xiaomi-OCR-0 reads a table better from a PNG)
    /// or a bounded page (the model's input is 2048 px) says so here.
    fn image_spec(&self) -> PageRenderSpec {
        PageRenderSpec::jpeg(self.dpi(), OCR_JPEG_QUALITY)
    }
    /// What else changes the page's answer, and so belongs in the cache key.
    ///
    /// The recipe and the table format change what comes out of the same
    /// model for the same page, so a cache entry written under one recipe must
    /// not be served under the other.
    fn cache_tag(&self) -> &str {
        ""
    }
    fn transport(&self) -> OcrTransport;
    /// One request over a sub-PDF. The output indices are relative to it.
    async fn read_pdf(
        &self,
        pdf_bytes: &[u8],
        include_figures: bool,
    ) -> Result<Vec<OcrPageRead>, OcrCallError>;
    /// One request over an image upload. Providers that only take documents
    /// return a permanent error, and the caller keeps its old image path.
    async fn read_image(
        &self,
        image_bytes: &[u8],
        mime: &str,
    ) -> Result<OcrPageAnswer, OcrCallError>;
}

/// The process-wide outage breaker. Cheap to clone behind an `Arc`.
#[derive(Debug)]
pub struct OcrBreaker {
    threshold: u32,
    cooldown: Duration,
    state: Mutex<BreakerState>,
}

#[derive(Debug, Default)]
struct BreakerState {
    consecutive: u32,
    open_until: Option<Instant>,
}

impl OcrBreaker {
    /// `threshold == 0` disables the breaker.
    #[must_use]
    pub fn new(threshold: u32, cooldown: Duration) -> Self {
        Self {
            threshold,
            cooldown,
            state: Mutex::new(BreakerState::default()),
        }
    }

    /// Whether a page may be sent now.
    #[must_use]
    pub fn allows(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match state.open_until {
            Some(until) => Instant::now() >= until,
            None => true,
        }
    }

    /// One provider answer came back: the outage is over.
    pub fn record_success(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.consecutive = 0;
        state.open_until = None;
    }

    /// One transient provider failure. Returns `true` when this failure opened
    /// the breaker.
    pub fn record_failure(&self) -> bool {
        if self.threshold == 0 {
            return false;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.consecutive = state.consecutive.saturating_add(1);
        if state.consecutive >= self.threshold {
            state.open_until = Some(Instant::now() + self.cooldown);
            state.consecutive = 0;
            return true;
        }
        false
    }
}

/// The process-wide breaker, built once from the first configuration that
/// asks for one.
static BREAKER: OnceLock<Arc<OcrBreaker>> = OnceLock::new();

/// The breaker every router in this process shares, built once from the first
/// configuration that asks for one.
#[must_use]
pub fn process_breaker(threshold: u32, cooldown: Duration) -> Arc<OcrBreaker> {
    Arc::clone(BREAKER.get_or_init(|| Arc::new(OcrBreaker::new(threshold, cooldown))))
}

/// The process-wide breaker, when one was already built.
///
/// The health probe uses this to close a breaker that is still open after the
/// provider came back: without a page read there is nothing else that could
/// close it, so a job retried in the cooldown window was refused every page
/// and reported "no text for any page" (`#35`).
///
/// `None` when no router has asked for a breaker yet: this never builds one,
/// because a breaker built here would carry the wrong threshold.
#[must_use]
pub fn existing_breaker() -> Option<Arc<OcrBreaker>> {
    BREAKER.get().cloned()
}

/// The router's tunables, resolved from [`ExtractConfig`].
#[derive(Debug, Clone)]
pub struct OcrRouterSettings {
    pub mode: OcrMode,
    pub batch: u32,
    pub max_pages: u32,
    pub scan_area: f32,
    pub scan_text: u32,
    pub concurrency: u32,
    pub breaker_threshold: u32,
    pub breaker_cooldown: Duration,
    pub dpi: u32,
    /// Attempts per provider call (one initial plus retries). Tests set 1 to
    /// exercise the breaker without waiting through backoff.
    pub attempts: u32,
    /// The extraction quality gate (`KB_EXTRACT_QUALITY_GATE`), when it is on.
    ///
    /// With it on, a page whose text layer is unusable — garbage from a broken
    /// font map, or a handful of stray strings — is read by OCR instead of
    /// being indexed as it is (`#29`).
    pub quality: Option<QualityOptions>,
}

impl OcrRouterSettings {
    /// The settings a resolved configuration describes.
    #[must_use]
    pub fn from_config(cfg: &ExtractConfig) -> Self {
        Self {
            mode: cfg.resolved_ocr_mode(),
            batch: cfg.ocr_batch.max(1),
            max_pages: cfg.ocr_max_pages.max(1),
            scan_area: cfg.ocr_scan_area,
            scan_text: cfg.ocr_scan_text,
            concurrency: cfg.ocr_concurrency.max(1),
            breaker_threshold: cfg.ocr_breaker,
            breaker_cooldown: cfg.ocr_breaker_cooldown,
            dpi: cfg.ocr_dpi,
            attempts: MAX_ATTEMPTS,
            quality: cfg.quality_options(),
        }
    }
}

/// One page after routing: its text (if any) and where it came from.
#[derive(Debug, Default, Clone)]
struct PageOutcome {
    text: Option<String>,
    figures: Vec<ExtractedFigure>,
    from_cache: bool,
    /// The page was sent, or was meant to be and the breaker refused.
    attempted: bool,
    /// The breaker refused the page: the provider is in its outage cooldown,
    /// so the page was never tried.
    breaker_skipped: bool,
    /// A layout detector answered for this page.
    layout: bool,
    /// The layout detector failed for this page.
    layout_failed: bool,
}

/// What one group of pages did.
#[derive(Debug, Default)]
struct RouteOutcome {
    pages: Vec<(u32, PageOutcome)>,
    sent: u32,
    cached: u32,
    breaker_open: bool,
    /// How many pages the breaker refused to send.
    skipped_by_breaker: u32,
    /// Whether a provider call failed with a transport error: the provider
    /// could not be reached, so the pages are worth retrying.
    transport_error: bool,
    layout_pages: u32,
    layout_failed: u32,
    error: Option<String>,
}

impl RouteOutcome {
    fn merge(&mut self, other: Self) {
        self.pages.extend(other.pages);
        self.sent += other.sent;
        self.cached += other.cached;
        self.breaker_open |= other.breaker_open;
        self.skipped_by_breaker += other.skipped_by_breaker;
        self.transport_error |= other.transport_error;
        self.layout_pages += other.layout_pages;
        self.layout_failed += other.layout_failed;
        if self.error.is_none() {
            self.error = other.error;
        }
    }
}

/// A page's index and the bytes an image provider is sent.
#[derive(Debug, Clone)]
struct PageArtifact {
    page: u32,
    mime: String,
    bytes: Vec<u8>,
}

/// The pages a document will send, and the ones past the cap.
#[derive(Debug, Default)]
struct Candidates {
    send: Vec<u32>,
    skipped: Vec<u32>,
    /// The pages the quality gate sent although they carry a text layer
    /// (`#29`). The report names them, so an operator can see which pages of a
    /// book had to be read again by a model.
    quality_pages: Vec<u32>,
}

/// Routes PDF pages and image uploads to OCR under the mode's rules.
pub struct OcrRouter {
    settings: OcrRouterSettings,
    provider: Option<Arc<dyn OcrPageProvider>>,
    cache: Option<Arc<dyn OcrPageCache>>,
    breaker: Arc<OcrBreaker>,
    permits: Arc<Semaphore>,
}

impl OcrRouter {
    /// Build a router. `breaker` defaults to the process-wide one.
    #[must_use]
    pub fn new(
        settings: OcrRouterSettings,
        provider: Option<Arc<dyn OcrPageProvider>>,
        cache: Option<Arc<dyn OcrPageCache>>,
        breaker: Option<Arc<OcrBreaker>>,
    ) -> Self {
        let breaker = breaker.unwrap_or_else(|| {
            process_breaker(settings.breaker_threshold, settings.breaker_cooldown)
        });
        let permits = Arc::new(Semaphore::new(settings.concurrency.max(1) as usize));
        Self {
            settings,
            provider,
            cache,
            breaker,
            permits,
        }
    }

    /// The permits shared by every document in this process, so a second
    /// component can join the same pool.
    #[must_use]
    pub fn permits(&self) -> Arc<Semaphore> {
        Arc::clone(&self.permits)
    }

    /// The mode this router runs.
    #[must_use]
    pub fn mode(&self) -> OcrMode {
        self.settings.mode
    }

    /// Route a PDF, page by page.
    ///
    /// # Errors
    ///
    /// When the PDF cannot be opened, or when **no** page produced any text at
    /// all after the provider failed: a document that would index as empty is
    /// a failed job, not a successful empty one.
    pub async fn extract_pdf(
        &self,
        pdf_bytes: &[u8],
        filename: &str,
        options: &ExtractionOptions,
    ) -> Result<ExtractionResult, ExtractError> {
        let bytes = pdf_bytes.to_vec();
        let blocking = bytes.clone();
        let (facts, outline) = tokio::task::spawn_blocking(move || {
            let pdfium = get_pdfium()?;
            let facts = read_page_facts(pdfium, &blocking)?;
            let local_texts: Vec<String> = facts.iter().map(|f| f.text.clone()).collect();
            let outline = crate::outline::extract_outline(pdfium, &blocking, &local_texts);
            Ok::<_, ExtractError>((facts, outline))
        })
        .await
        .map_err(|e| ExtractError::Container(format!("OCR page facts task: {e}")))??;

        if facts.is_empty() {
            return Err(ExtractError::Container(format!(
                "OCR failed for \"{filename}\": the PDF has no pages"
            )));
        }

        let mut report = self.report(facts.len() as u32);
        let mut text = PageTexts::from_facts(&facts);
        let mut outcomes: Vec<PageOutcome> = facts.iter().map(|_| PageOutcome::default()).collect();

        let provider = match (&self.provider, self.settings.mode) {
            (Some(provider), mode) if mode != OcrMode::Off => Some(Arc::clone(provider)),
            _ => None,
        };

        let candidates = self.candidates(&facts, provider.is_some());
        report.skipped_pages = candidates.skipped.iter().map(|p| p + 1).collect();
        // The pages the quality gate sent although they carry a text layer:
        // the operator sees which pages of a book had to be read again by a
        // model (`#29`).
        report.reread_pages = candidates.quality_pages.iter().map(|p| p + 1).collect();
        let mut last_error: Option<String> = None;
        let mut skipped_by_breaker = 0u32;
        let mut transport_error = false;
        let mut sent = 0u32;

        if let Some(provider) = provider {
            // The pages that need OCR are about to wait on the remote service:
            // give the worker's extract seat back first (ING-6). The local
            // page facts are already read; what is left here is network wait
            // and a cheap merge.
            if !candidates.send.is_empty() {
                options.seat.release();
            }
            let routed = match provider.transport() {
                OcrTransport::Image => {
                    self.route_images_in_pdf(&bytes, &candidates.send, provider.as_ref())
                        .await?
                }
                OcrTransport::Pdf { batch } => {
                    self.route_pdf_batches(
                        &bytes,
                        &candidates.send,
                        provider.as_ref(),
                        batch.max(1),
                        options.figures,
                    )
                    .await?
                }
            };

            report.ocr_pages = routed.sent;
            report.cached_pages = routed.cached;
            report.breaker_opened = routed.breaker_open;
            report.layout_pages = routed.layout_pages;
            report.layout_failed = routed.layout_failed;
            sent = routed.sent;
            skipped_by_breaker = routed.skipped_by_breaker;
            transport_error = routed.transport_error;
            last_error = routed.error;
            for (page, outcome) in routed.pages {
                let index = page as usize;
                if index >= outcomes.len() {
                    continue;
                }
                if outcome.attempted && outcome.text.as_deref().is_none_or(str::is_empty) {
                    report.failed_pages.push(page + 1);
                }
                outcomes[index] = outcome;
            }
        }

        let mut figures = Vec::new();
        let mut has_text = false;
        let mut used_ocr = false;
        for (index, fact) in facts.iter().enumerate() {
            let outcome = &outcomes[index];
            if let Some(ocr) = outcome.text.as_deref().map(str::trim) {
                if !ocr.is_empty() {
                    text.set(index, ocr.to_string());
                    figures.extend(outcome.figures.iter().cloned());
                    has_text = true;
                    used_ocr = true;
                    continue;
                }
            }
            let local = fact.text.trim();
            let was_candidate =
                candidates.send.contains(&fact.page) || candidates.skipped.contains(&fact.page);
            if !local.is_empty() && (self.settings.mode == OcrMode::Full || was_candidate) {
                text.set(index, local.to_string());
                report.local_pages += 1;
                has_text = true;
            } else if was_candidate {
                // A marker keeps a lost page visible in the document, but it
                // is not text: a document made only of markers must fail.
                text.set(index, marker(fact.page));
            } else {
                report.local_pages += u32::from(!local.is_empty());
                text.set(index, local.to_string());
                has_text |= !local.is_empty();
            }
        }

        if !has_text {
            // Three different problems, three different reasons (`#35`):
            // a breaker that refused every page, a provider that could not be
            // reached, and a document that really has no readable text. The
            // first two are worth retrying; the third is not.
            let (reason, retryable) = if skipped_by_breaker > 0 && sent == 0 {
                (
                    "OCR breaker open: the OCR provider is in its outage cooldown; \
                     retry when it is healthy"
                        .to_string(),
                    true,
                )
            } else if transport_error {
                let detail =
                    last_error.unwrap_or_else(|| "the provider did not answer".to_string());
                (format!("the OCR provider is unreachable: {detail}"), true)
            } else if let Some(error) = last_error {
                (error, false)
            } else if candidates.send.is_empty() {
                ("the PDF has no readable text".to_string(), false)
            } else {
                (
                    "the OCR provider returned no text for any page".to_string(),
                    false,
                )
            };
            let message = format!("OCR failed for \"{filename}\": {reason}");
            return Err(if retryable {
                ExtractError::Unavailable(message)
            } else {
                ExtractError::Container(message)
            });
        }

        report.failed_pages.sort_unstable();
        report.failed_pages.dedup();
        report.skipped_pages.sort_unstable();
        report.skipped_pages.dedup();
        Ok(text.into_result(
            "pdf",
            report,
            Some(outline),
            used_ocr,
            figures,
            options.figures,
        ))
    }

    /// Route a standalone image (or every TIFF page) through the same cache,
    /// pool and breaker as PDF pages.
    ///
    /// # Errors
    ///
    /// When a provider cannot read images, or when no page produced text in a
    /// mode that asked for OCR.
    pub async fn extract_image(
        &self,
        image_bytes: &[u8],
        mime: &str,
        filename: &str,
    ) -> Result<ExtractionResult, ExtractError> {
        let bytes = image_bytes.to_vec();
        let mime = mime.to_string();
        let max_pages = self.settings.max_pages;
        let pages = tokio::task::spawn_blocking(move || {
            let pages = render_image_pages(&bytes, 0, max_pages);
            if pages.is_empty() {
                // A picture normalisation cannot decode is still one page: send
                // the bytes as they came rather than call it zero pages.
                vec![PageArtifact {
                    page: 0,
                    mime: if mime.is_empty() {
                        "image/png".to_string()
                    } else {
                        mime
                    },
                    bytes,
                }]
            } else {
                pages
                    .into_iter()
                    .map(|page| PageArtifact {
                        page: page.page,
                        mime: page.mime,
                        bytes: page.bytes,
                    })
                    .collect()
            }
        })
        .await
        .map_err(|e| ExtractError::Container(format!("OCR image page task: {e}")))?;

        let mut report = self.report(pages.len() as u32);
        let mut text = PageTexts::empty(pages.len());
        let mut used_ocr = false;
        let mut skipped_by_breaker = 0u32;
        let mut transport_error = false;
        let mut sent = 0u32;
        let mut last_error: Option<String> = None;

        let provider = match (&self.provider, self.settings.mode) {
            (Some(provider), mode) if mode != OcrMode::Off => Some(Arc::clone(provider)),
            _ => None,
        };

        if let Some(provider) = provider {
            if provider.transport() != OcrTransport::Image {
                return Err(ExtractError::Unsupported(format!(
                    "OCR provider \"{}\" does not read image uploads",
                    provider.id()
                )));
            }
            let routed = self.route_image_items(pages, provider.as_ref()).await?;
            report.ocr_pages = routed.sent;
            report.cached_pages = routed.cached;
            report.breaker_opened = routed.breaker_open;
            report.layout_pages = routed.layout_pages;
            report.layout_failed = routed.layout_failed;
            sent = routed.sent;
            skipped_by_breaker = routed.skipped_by_breaker;
            transport_error = routed.transport_error;
            last_error = routed.error;
            for (page, outcome) in routed.pages {
                let index = page as usize;
                if index >= text.len() {
                    continue;
                }
                if outcome.attempted && outcome.text.as_deref().is_none_or(str::is_empty) {
                    report.failed_pages.push(page + 1);
                }
                match outcome.text.as_deref().map(str::trim) {
                    Some(ocr) if !ocr.is_empty() => {
                        text.set(index, ocr.to_string());
                        used_ocr = true;
                    }
                    _ if outcome.attempted => text.set(index, marker(page)),
                    _ => {}
                }
            }
        }

        if text.all_empty() {
            if self.settings.mode == OcrMode::Off {
                // No provider was asked: this is a picture with no text in it,
                // or no OCR configured, not a failed job.
                return Ok(text.into_result("image", report, None, false, Vec::new(), false));
            }
            // The same three cases as a PDF (`#35`): a breaker that refused
            // every page, a provider that could not be reached, and a picture
            // the provider could not read.
            let (reason, retryable) = if skipped_by_breaker > 0 && sent == 0 {
                (
                    "OCR breaker open: the OCR provider is in its outage cooldown; \
                     retry when it is healthy"
                        .to_string(),
                    true,
                )
            } else if transport_error {
                let detail =
                    last_error.unwrap_or_else(|| "the provider did not answer".to_string());
                (format!("the OCR provider is unreachable: {detail}"), true)
            } else if let Some(error) = last_error {
                (error, false)
            } else {
                ("no page could be read".to_string(), false)
            };
            let message = format!("Image OCR failed for \"{filename}\": {reason}");
            return Err(if retryable {
                ExtractError::Unavailable(message)
            } else {
                ExtractError::Container(message)
            });
        }

        report.failed_pages.sort_unstable();
        report.failed_pages.dedup();
        Ok(text.into_result("image", report, None, used_ocr, Vec::new(), false))
    }

    fn report(&self, pages: u32) -> OcrReport {
        OcrReport {
            mode: self.settings.mode.as_str().to_string(),
            provider: self
                .provider
                .as_ref()
                .filter(|_| self.settings.mode != OcrMode::Off)
                .map(|p| p.id().to_string()),
            pages,
            ..OcrReport::default()
        }
    }

    /// Which pages go to OCR, in document order, and which fall past the cap.
    fn candidates(&self, facts: &[PageFacts], has_provider: bool) -> Candidates {
        let mut out = Candidates::default();
        if !has_provider || self.settings.mode == OcrMode::Off {
            return out;
        }
        // The median page of the document, so a page that holds far less text
        // than the rest is caught (`#29`). It is the median of the pages that
        // carry text at all: an empty page would drag it down.
        let median = self.quality_median(facts);
        let mut budget = self.settings.max_pages;
        for fact in facts {
            let scan = match self.settings.mode {
                OcrMode::Full => true,
                OcrMode::Hybrid => {
                    page_needs_ocr(fact, self.settings.scan_area, self.settings.scan_text)
                }
                OcrMode::Off | OcrMode::Auto => false,
            };
            // A text layer that is not text: the page is a scan in everything
            // but name, and indexing it as it is poisons the document.
            let broken = !scan && self.quality_broken(fact, median);
            let wants = scan || broken;
            if !wants {
                continue;
            }
            if budget > 0 {
                budget -= 1;
                out.send.push(fact.page);
                if broken {
                    out.quality_pages.push(fact.page);
                }
            } else {
                out.skipped.push(fact.page);
            }
        }
        out
    }

    /// The median number of characters a page of this document carries.
    fn quality_median(&self, facts: &[PageFacts]) -> usize {
        if self.settings.quality.is_none() {
            return 0;
        }
        let chars: Vec<usize> = facts
            .iter()
            .map(|f| count_chars(&f.text))
            .filter(|c| *c > 0)
            .collect();
        median_page_chars(&chars)
    }

    /// Whether the page's own text layer is unusable.
    fn quality_broken(&self, fact: &PageFacts, median: usize) -> bool {
        let Some(opts) = self.settings.quality else {
            return false;
        };
        check_page_against_median(&fact.text, median, &opts).is_broken()
    }

    /// Chat-style providers: render the candidate pages once, then read them
    /// in concurrency-sized groups.
    async fn route_images_in_pdf(
        &self,
        pdf_bytes: &[u8],
        pages: &[u32],
        provider: &dyn OcrPageProvider,
    ) -> Result<RouteOutcome, ExtractError> {
        if pages.is_empty() {
            return Ok(RouteOutcome::default());
        }
        let bytes = pdf_bytes.to_vec();
        let indices = pages.to_vec();
        let spec = provider.image_spec();
        let images = tokio::task::spawn_blocking(move || {
            render_pdf_page_indices_spec(&bytes, &indices, spec)
        })
        .await
        .map_err(|e| ExtractError::Container(format!("OCR render task: {e}")))??;

        let items: Vec<PageArtifact> = images
            .into_iter()
            .map(|page| PageArtifact {
                page: page.page,
                mime: page.mime,
                bytes: page.bytes,
            })
            .collect();
        self.route_image_items(items, provider).await
    }

    /// Cache-check and send a list of page images.
    async fn route_image_items(
        &self,
        items: Vec<PageArtifact>,
        provider: &dyn OcrPageProvider,
    ) -> Result<RouteOutcome, ExtractError> {
        let concurrency = self.settings.concurrency.max(1) as usize;
        let items = Arc::new(items);
        let ranges: Vec<(usize, usize)> = (0..items.len())
            .step_by(concurrency)
            .map(|start| (start, (start + concurrency).min(items.len())))
            .collect();
        let results: Vec<RouteOutcome> = stream::iter(ranges)
            .map(|(start, end)| {
                let items = Arc::clone(&items);
                async move { self.read_image_chunk(&items[start..end], provider).await }
            })
            .buffered(concurrency)
            .collect()
            .await;

        let mut out = RouteOutcome::default();
        for batch in results {
            out.merge(batch);
        }
        Ok(out)
    }

    async fn read_image_chunk(
        &self,
        items: &[PageArtifact],
        provider: &dyn OcrPageProvider,
    ) -> RouteOutcome {
        let mut outcome = RouteOutcome::default();

        for item in items {
            let mut page = PageOutcome {
                attempted: true,
                ..PageOutcome::default()
            };
            let key = page_cache_key(provider, &item.bytes);
            if let Some(hit) = self.cache_get(&key).await {
                outcome.cached += 1;
                page.text = Some(hit.text);
                page.figures = hit.figures;
                page.from_cache = true;
                outcome.pages.push((item.page, page));
                continue;
            }

            if item.bytes.is_empty() {
                outcome
                    .error
                    .get_or_insert_with(|| format!("page {} could not be rendered", item.page + 1));
                outcome.pages.push((item.page, page));
                continue;
            }

            if !self.breaker.allows() {
                // The provider is in its outage cooldown. The page is not
                // tried and its answer is not cached: a retry must be able to
                // read it (`#35`).
                outcome.breaker_open = true;
                outcome.skipped_by_breaker += 1;
                page.breaker_skipped = true;
                outcome.pages.push((item.page, page));
                continue;
            }

            let permit = match Arc::clone(&self.permits).acquire_owned().await {
                Ok(permit) => permit,
                Err(e) => {
                    outcome
                        .error
                        .get_or_insert_with(|| format!("OCR pool closed: {e}"));
                    outcome.pages.push((item.page, page));
                    continue;
                }
            };
            let answer = retry(
                || provider.read_image(&item.bytes, &item.mime),
                self.settings.attempts,
            )
            .await;
            drop(permit);
            outcome.sent += 1;

            match answer {
                Ok(answer) if !answer.text.trim().is_empty() => {
                    self.breaker.record_success();
                    let text = answer.text.trim().to_string();
                    // A page whose layout path failed keeps the whole-page
                    // answer, but it is not cached: the next run, with the
                    // detector back, must be free to read it properly.
                    if !answer.layout_failed {
                        self.cache_put(&key, &text, &[]).await;
                    }
                    page.text = Some(text);
                    page.layout = answer.layout;
                    page.layout_failed = answer.layout_failed;
                    if answer.layout {
                        outcome.layout_pages += 1;
                    }
                    if answer.layout_failed {
                        outcome.layout_failed += 1;
                    }
                }
                Ok(answer) => {
                    // A 200 with no text is a page that was not read.
                    self.breaker.record_success();
                    page.layout = answer.layout;
                    page.layout_failed = answer.layout_failed;
                    if answer.layout {
                        outcome.layout_pages += 1;
                    }
                    if answer.layout_failed {
                        outcome.layout_failed += 1;
                    }
                }
                Err(err) => {
                    if err.transient && self.breaker.record_failure() {
                        outcome.breaker_open = true;
                    }
                    if err.transient {
                        // A transport error (connection refused, timeout, 5xx):
                        // the provider could not be reached, so the pages are
                        // worth retrying. Nothing about this page is cached
                        // (`#35`).
                        outcome.transport_error = true;
                    }
                    outcome.error.get_or_insert(err.message);
                }
            }
            outcome.pages.push((item.page, page));
        }

        outcome
    }

    /// Document providers: one sub-PDF per batch of cache-missing pages.
    async fn route_pdf_batches(
        &self,
        pdf_bytes: &[u8],
        pages: &[u32],
        provider: &dyn OcrPageProvider,
        batch: u32,
        include_figures: bool,
    ) -> Result<RouteOutcome, ExtractError> {
        if pages.is_empty() {
            return Ok(RouteOutcome::default());
        }

        // The page's identity: the source document's SHA-256 and the page
        // index. Hashing a one-page PDF saved by pdfium would be unstable — it
        // embeds a random `/ID` on every save, discussed in `docs/kb-tiers.md`
        // — so a page is addressed by what it is, not by a fresh serialisation
        // of it. Provider, model, prompt and resolution are in the key too.
        let digest: [u8; 32] = Sha256::digest(pdf_bytes).into();

        let mut outcome = RouteOutcome::default();
        let mut keys: BTreeMap<u32, String> = BTreeMap::new();
        let mut to_send: Vec<u32> = Vec::new();
        for page in pages {
            let key = document_page_cache_key(provider, &digest, *page);
            match self.cache_get(&key).await {
                Some(hit) => {
                    outcome.cached += 1;
                    outcome.pages.push((
                        *page,
                        PageOutcome {
                            text: Some(hit.text),
                            figures: hit.figures,
                            from_cache: true,
                            attempted: true,
                            ..PageOutcome::default()
                        },
                    ));
                }
                None => {
                    keys.insert(*page, key);
                    to_send.push(*page);
                }
            }
        }

        let batches: Vec<Vec<u32>> = to_send
            .chunks(batch.max(1) as usize)
            .map(<[u32]>::to_vec)
            .collect();
        if batches.is_empty() {
            return Ok(outcome);
        }

        // Build every sub-PDF on one thread, one after another: pdfium's
        // document creation is the one call this crate does not run in
        // parallel (the requests that follow are). A build failure fails only
        // its own batch.
        let bytes = pdf_bytes.to_vec();
        let specs = batches.clone();
        let splits = tokio::task::spawn_blocking(move || {
            let pdfium = match get_pdfium() {
                Ok(pdfium) => pdfium,
                Err(err) => {
                    return specs
                        .iter()
                        .map(|pages| (pages.clone(), Err(err.to_string())))
                        .collect();
                }
            };
            specs
                .iter()
                .map(|pages| match pdf_page_subset(pdfium, &bytes, pages) {
                    Ok(pdf) => (pages.clone(), Ok(pdf)),
                    Err(err) => (pages.clone(), Err(err.to_string())),
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| ExtractError::Container(format!("OCR page split task: {e}")))?;

        let concurrency = self.settings.concurrency.max(1) as usize;
        let keys_ref = &keys;
        let results: Vec<RouteOutcome> = stream::iter(splits)
            .map(|(batch_pages, split)| async move {
                self.read_pdf_batch(batch_pages, split, provider, include_figures, keys_ref)
                    .await
            })
            .buffered(concurrency)
            .collect()
            .await;

        for result in results {
            outcome.merge(result);
        }
        Ok(outcome)
    }

    async fn read_pdf_batch(
        &self,
        pages: Vec<u32>,
        split: Result<Vec<u8>, String>,
        provider: &dyn OcrPageProvider,
        include_figures: bool,
        keys: &BTreeMap<u32, String>,
    ) -> RouteOutcome {
        let mut outcome = RouteOutcome::default();
        let failed_page = |page: u32| {
            (
                page,
                PageOutcome {
                    attempted: true,
                    ..PageOutcome::default()
                },
            )
        };

        if !self.breaker.allows() {
            outcome.breaker_open = true;
            outcome.skipped_by_breaker += pages.len() as u32;
            outcome.pages.extend(pages.iter().map(|p| {
                (
                    *p,
                    PageOutcome {
                        attempted: true,
                        breaker_skipped: true,
                        ..PageOutcome::default()
                    },
                )
            }));
            return outcome;
        }

        let sub_pdf = match split {
            Ok(pdf) => pdf,
            Err(err) => {
                outcome.error = Some(format!("could not build an OCR page batch: {err}"));
                outcome.pages.extend(pages.iter().map(|p| failed_page(*p)));
                return outcome;
            }
        };

        let permit = match Arc::clone(&self.permits).acquire_owned().await {
            Ok(permit) => permit,
            Err(e) => {
                outcome.error = Some(format!("OCR pool closed: {e}"));
                outcome.pages.extend(pages.iter().map(|p| failed_page(*p)));
                return outcome;
            }
        };
        let answer = retry(
            || provider.read_pdf(&sub_pdf, include_figures),
            self.settings.attempts,
        )
        .await;
        drop(permit);
        outcome.sent = pages.len() as u32;

        match answer {
            Ok(reads) => {
                self.breaker.record_success();
                let mut by_index: BTreeMap<u32, OcrPageRead> =
                    reads.into_iter().map(|r| (r.page, r)).collect();
                for (position, page) in pages.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    let relative = position as u32;
                    let read = by_index.remove(&relative);
                    match read {
                        Some(read) if !read.text.trim().is_empty() => {
                            let text = read.text.trim().to_string();
                            let figures = offset_figures(read.figures, read.page, *page);
                            if let Some(key) = keys.get(page) {
                                self.cache_put(key, &text, &figures).await;
                            }
                            outcome.pages.push((
                                *page,
                                PageOutcome {
                                    text: Some(text),
                                    figures,
                                    attempted: true,
                                    ..PageOutcome::default()
                                },
                            ));
                        }
                        _ => outcome.pages.push(failed_page(*page)),
                    }
                }
            }
            Err(err) => {
                if err.transient && self.breaker.record_failure() {
                    outcome.breaker_open = true;
                }
                if err.transient {
                    outcome.transport_error = true;
                }
                outcome.error = Some(err.message);
                outcome.pages.extend(pages.iter().map(|p| failed_page(*p)));
            }
        }

        outcome
    }

    async fn cache_get(&self, key: &str) -> Option<CachedPage> {
        let raw = match &self.cache {
            Some(cache) => cache.get(key).await?,
            None => return None,
        };
        serde_json::from_str::<CachedPage>(&raw)
            .ok()
            .or(Some(CachedPage {
                text: raw,
                figures: Vec::new(),
            }))
    }

    async fn cache_put(&self, key: &str, text: &str, figures: &[ExtractedFigure]) {
        if let Some(cache) = &self.cache {
            let entry = CachedPage {
                text: text.to_string(),
                figures: figures.to_vec(),
            };
            match serde_json::to_string(&entry) {
                Ok(json) => cache.put(key, &json).await,
                Err(e) => tracing::warn!(error = %e, "OCR page result could not be serialised"),
            }
        }
    }
}

/// Final page texts, including empty pages, in document order.
struct PageTexts(Vec<String>);

impl PageTexts {
    fn from_facts(facts: &[PageFacts]) -> Self {
        Self(facts.iter().map(|f| f.text.clone()).collect())
    }

    fn empty(pages: usize) -> Self {
        Self(vec![String::new(); pages])
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    /// True when no page carries real text: a marker is not text.
    fn all_empty(&self) -> bool {
        self.0.iter().all(|t| {
            let t = t.trim();
            t.is_empty() || (t.starts_with("[page ") && t.ends_with(" could not be read]"))
        })
    }

    fn set(&mut self, index: usize, text: String) {
        if index < self.0.len() {
            self.0[index] = text;
        }
    }

    fn into_result(
        self,
        file_type: &str,
        report: OcrReport,
        outline: Option<kb_core::tree::Outline>,
        used_ocr: bool,
        figures: Vec<ExtractedFigure>,
        want_figures: bool,
    ) -> ExtractionResult {
        let mut content_pages = Vec::new();
        let mut page_map = Vec::new();
        let mut blocks = Vec::new();
        for (index, raw) in self.0.into_iter().enumerate() {
            let text = raw.trim().to_string();
            #[allow(clippy::cast_possible_truncation)]
            let page = index as u32;
            if text.is_empty() {
                blocks.push(Vec::new());
                continue;
            }
            content_pages.push(text.clone());
            page_map.push(PageText {
                page,
                text: text.clone(),
            });
            // An OCR page read by a layout-capable engine carries inline image
            // markdown; turning it into blocks keeps its figures in reading
            // order, exactly as the whole-document Mistral path does.
            if text.contains("![") {
                blocks.push(blocks_from_markdown(&text, page));
            } else {
                blocks.push(vec![PageBlock {
                    kind: PageBlockKind::Text,
                    id: None,
                    text: Some(text),
                }]);
            }
        }

        ExtractionResult {
            content: content_pages.join("\n\n"),
            file_type: file_type.to_string(),
            used_ocr,
            figures: if want_figures { figures } else { Vec::new() },
            pages_blocks: Some(blocks),
            page_map: if page_map.is_empty() {
                None
            } else {
                Some(page_map)
            },
            outline,
            tables: Vec::new(),
            ocr: Some(report),
            error: None,
        }
    }
}

/// One page's marker when it could not be read.
fn marker(page: u32) -> String {
    format!("[page {} could not be read]", page + 1)
}

/// Cache key over the provider's identity and the page's own bytes (an image
/// upload, or a rendered page).
fn page_cache_key(provider: &dyn OcrPageProvider, artifact: &[u8]) -> String {
    page_cache_key_of(provider, &[artifact])
}

/// Cache key for one page of a document provider: the source document's
/// SHA-256 and the page index identify the page, because pdfium's saved
/// one-page PDF carries a random `/ID` and would never hit twice.
fn document_page_cache_key(
    provider: &dyn OcrPageProvider,
    document_digest: &[u8; 32],
    page: u32,
) -> String {
    page_cache_key_of(provider, &[document_digest.as_slice(), &page.to_le_bytes()])
}

fn page_cache_key_of(provider: &dyn OcrPageProvider, identity: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    let dpi = provider.dpi().to_le_bytes();
    for part in [
        provider.id().as_bytes(),
        provider.model().as_bytes(),
        provider.prompt().as_bytes(),
        provider.cache_tag().as_bytes(),
        dpi.as_slice(),
    ]
    .into_iter()
    .chain(identity.iter().copied())
    {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

/// Retry a provider call on transient errors, with exponential backoff.
async fn retry<F, Fut, T>(mut call: F, attempts: u32) -> Result<T, OcrCallError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, OcrCallError>>,
{
    let attempts = attempts.max(1);
    let mut last = OcrCallError::permanent("no attempt was made");
    for attempt in 1..=attempts {
        match call().await {
            Ok(value) => return Ok(value),
            Err(err) if err.transient && attempt < attempts => {
                tracing::warn!(attempt, error = %err.message, "OCR transient error; backing off");
                last = err;
                backoff(attempt).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last)
}

async fn backoff(attempt: u32) {
    let millis = 200u64.saturating_mul(1 << attempt.saturating_sub(1).min(4));
    tokio::time::sleep(Duration::from_millis(millis.min(2_000))).await;
}

/// Move a figure's page index and id to the document page that carried it: a
/// batch response numbers pages from 0 within the sub-PDF, and the reader
/// needs the page the viewer shows.
fn offset_figures(figures: Vec<ExtractedFigure>, relative: u32, page: u32) -> Vec<ExtractedFigure> {
    let prefix = format!("p{relative}-");
    figures
        .into_iter()
        .map(|mut figure| {
            figure.page = page;
            if let Some(id) = figure.id.as_deref() {
                if let Some(rest) = id.strip_prefix(&prefix) {
                    figure.id = Some(format!("p{page}-{rest}"));
                }
            }
            figure
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_breaker_opens_after_the_threshold() {
        let breaker = OcrBreaker::new(3, Duration::from_secs(60));
        assert!(breaker.allows());
        assert!(!breaker.record_failure());
        assert!(!breaker.record_failure());
        assert!(
            breaker.record_failure(),
            "the third consecutive failure opens it"
        );
        assert!(!breaker.allows(), "no page is sent while it is open");
    }

    #[test]
    fn a_success_resets_the_consecutive_count() {
        let breaker = OcrBreaker::new(3, Duration::from_secs(60));
        breaker.record_failure();
        breaker.record_failure();
        breaker.record_success();
        assert!(!breaker.record_failure());
        assert!(!breaker.record_failure());
        assert!(
            breaker.allows(),
            "two more failures after a success are not three"
        );
    }

    #[test]
    fn a_zero_threshold_disables_the_breaker() {
        let breaker = OcrBreaker::new(0, Duration::from_secs(60));
        for _ in 0..10 {
            assert!(!breaker.record_failure());
        }
        assert!(breaker.allows());
    }

    #[test]
    fn the_marker_names_the_page_as_a_reader_counts() {
        assert_eq!(marker(0), "[page 1 could not be read]");
        assert_eq!(marker(299), "[page 300 could not be read]");
    }

    #[test]
    fn a_cache_key_changes_with_every_part_of_the_identity() {
        struct Fake(&'static str, &'static str, &'static str, u32);
        #[async_trait]
        impl OcrPageProvider for Fake {
            fn id(&self) -> &str {
                self.0
            }
            fn model(&self) -> &str {
                self.1
            }
            fn prompt(&self) -> &str {
                self.2
            }
            fn dpi(&self) -> u32 {
                self.3
            }
            fn transport(&self) -> OcrTransport {
                OcrTransport::Image
            }
            async fn read_pdf(
                &self,
                _pdf: &[u8],
                _figures: bool,
            ) -> Result<Vec<OcrPageRead>, OcrCallError> {
                Ok(Vec::new())
            }
            async fn read_image(
                &self,
                _image: &[u8],
                _mime: &str,
            ) -> Result<OcrPageAnswer, OcrCallError> {
                Ok(OcrPageAnswer::default())
            }
        }
        let base = Fake("chat", "m", "OCR:", 150);
        let key = page_cache_key(&base, b"page");
        assert_eq!(key, page_cache_key(&base, b"page"));
        assert_ne!(key, page_cache_key(&base, b"other page"));
        assert_ne!(
            key,
            page_cache_key(&Fake("mistral", "m", "OCR:", 150), b"page")
        );
        assert_ne!(
            key,
            page_cache_key(&Fake("chat", "m2", "OCR:", 150), b"page")
        );
        assert_ne!(
            key,
            page_cache_key(&Fake("chat", "m", "Table:", 150), b"page")
        );
        assert_ne!(
            key,
            page_cache_key(&Fake("chat", "m", "OCR:", 200), b"page")
        );
    }
}
