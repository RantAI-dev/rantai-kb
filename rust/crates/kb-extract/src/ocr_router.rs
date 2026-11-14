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
use kb_ports::{
    ExtractedFigure, ExtractionOptions, ExtractionResult, OcrPageCache, OcrReport, PageBlock,
    PageBlockKind, PageText,
};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::ExtractError;
use crate::config::{ExtractConfig, OcrMode};
use crate::mistral::blocks_from_markdown;
use crate::page_render::{render_image_pages, render_pdf_page_indices};
use crate::pdfium::{get_pdfium, pdf_page_subset_many};
use crate::scanned::{PageFacts, page_is_scan, read_page_facts};

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
    fn transport(&self) -> OcrTransport;
    /// One request over a sub-PDF. The output indices are relative to it.
    async fn read_pdf(
        &self,
        pdf_bytes: &[u8],
        include_figures: bool,
    ) -> Result<Vec<OcrPageRead>, OcrCallError>;
    /// One request over an image upload. Providers that only take documents
    /// return a permanent error, and the caller keeps its old image path.
    async fn read_image(&self, image_bytes: &[u8], mime: &str) -> Result<String, OcrCallError>;
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

/// The breaker every router in this process shares, built once from the first
/// configuration that asks for one.
#[must_use]
pub fn process_breaker(threshold: u32, cooldown: Duration) -> Arc<OcrBreaker> {
    static BREAKER: OnceLock<Arc<OcrBreaker>> = OnceLock::new();
    Arc::clone(BREAKER.get_or_init(|| Arc::new(OcrBreaker::new(threshold, cooldown))))
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
}

/// What one group of pages did.
#[derive(Debug, Default)]
struct RouteOutcome {
    pages: Vec<(u32, PageOutcome)>,
    sent: u32,
    cached: u32,
    breaker_open: bool,
    error: Option<String>,
}

impl RouteOutcome {
    fn merge(&mut self, other: Self) {
        self.pages.extend(other.pages);
        self.sent += other.sent;
        self.cached += other.cached;
        self.breaker_open |= other.breaker_open;
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

        if let Some(provider) = provider {
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
        for (index, fact) in facts.iter().enumerate() {
            let outcome = &outcomes[index];
            if let Some(ocr) = outcome.text.as_deref().map(str::trim) {
                if !ocr.is_empty() {
                    text.set(index, ocr.to_string());
                    figures.extend(outcome.figures.iter().cloned());
                    continue;
                }
            }
            let local = fact.text.trim();
            let was_candidate =
                candidates.send.contains(&fact.page) || candidates.skipped.contains(&fact.page);
            if !local.is_empty() && (self.settings.mode == OcrMode::Full || was_candidate) {
                text.set(index, local.to_string());
                report.local_pages += 1;
            } else if was_candidate {
                text.set(index, marker(fact.page));
            } else {
                report.local_pages += u32::from(!local.is_empty());
                text.set(index, local.to_string());
            }
        }

        if text.all_empty() {
            let reason = if candidates.send.is_empty() {
                "the PDF has no readable text".to_string()
            } else {
                "the OCR provider returned no text for any page".to_string()
            };
            return Err(ExtractError::Container(format!(
                "OCR failed for \"{filename}\": {reason}"
            )));
        }

        report.failed_pages.sort_unstable();
        report.failed_pages.dedup();
        report.skipped_pages.sort_unstable();
        report.skipped_pages.dedup();
        Ok(text.into_result("pdf", report, Some(outline), true, figures, options.figures))
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
            for (page, outcome) in routed.pages {
                let index = page as usize;
                if index >= text.len() {
                    continue;
                }
                if outcome.attempted && outcome.text.as_deref().is_none_or(str::is_empty) {
                    report.failed_pages.push(page + 1);
                }
                match outcome.text.as_deref().map(str::trim) {
                    Some(ocr) if !ocr.is_empty() => text.set(index, ocr.to_string()),
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
            return Err(ExtractError::Container(format!(
                "Image OCR failed for \"{filename}\": no page could be read"
            )));
        }

        report.failed_pages.sort_unstable();
        report.failed_pages.dedup();
        Ok(text.into_result("image", report, None, true, Vec::new(), false))
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
        let mut budget = self.settings.max_pages;
        for fact in facts {
            let wants = match self.settings.mode {
                OcrMode::Full => true,
                OcrMode::Hybrid => {
                    page_is_scan(fact, self.settings.scan_area, self.settings.scan_text)
                }
                OcrMode::Off | OcrMode::Auto => false,
            };
            if !wants {
                continue;
            }
            if budget > 0 {
                budget -= 1;
                out.send.push(fact.page);
            } else {
                out.skipped.push(fact.page);
            }
        }
        out
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
        let dpi = provider.dpi().max(1);
        let images = tokio::task::spawn_blocking(move || {
            render_pdf_page_indices(&bytes, &indices, dpi, OCR_JPEG_QUALITY)
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
            if let Some(text) = self.cache_get(&key).await {
                outcome.cached += 1;
                page.text = Some(text);
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
                outcome.breaker_open = true;
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
            let answer = retry(|| provider.read_image(&item.bytes, &item.mime)).await;
            drop(permit);
            outcome.sent += 1;

            match answer {
                Ok(text) if !text.trim().is_empty() => {
                    self.breaker.record_success();
                    let text = text.trim().to_string();
                    self.cache_put(&key, &text).await;
                    page.text = Some(text);
                }
                Ok(_) => {
                    // A 200 with no text is a page that was not read.
                    self.breaker.record_success();
                }
                Err(err) => {
                    if err.transient && self.breaker.record_failure() {
                        outcome.breaker_open = true;
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

        // The cache key is the page's own bytes: one one-page PDF per candidate
        // page, built with a single document load.
        let bytes = pdf_bytes.to_vec();
        let indices = pages.to_vec();
        let one_page = tokio::task::spawn_blocking(move || {
            let pdfium = get_pdfium()?;
            pdf_page_subset_many(pdfium, &bytes, &indices, 1)
        })
        .await
        .map_err(|e| ExtractError::Container(format!("OCR page split task: {e}")))??;

        let mut outcome = RouteOutcome::default();
        let mut keys: BTreeMap<u32, String> = BTreeMap::new();
        let mut to_send: Vec<u32> = Vec::new();
        for (page, artifact) in pages.iter().zip(&one_page) {
            let key = page_cache_key(provider, artifact);
            match self.cache_get(&key).await {
                Some(text) => {
                    outcome.cached += 1;
                    outcome.pages.push((
                        *page,
                        PageOutcome {
                            text: Some(text),
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
        let concurrency = self.settings.concurrency.max(1) as usize;
        let keys_ref = &keys;
        let results: Vec<RouteOutcome> = stream::iter(batches)
            .map(|batch_pages| async move {
                let bytes = pdf_bytes.to_vec();
                let pages = batch_pages.clone();
                let split = tokio::task::spawn_blocking(move || {
                    let pdfium = get_pdfium()?;
                    pdf_page_subset_many(pdfium, &bytes, &pages, pages.len().max(1))
                })
                .await;
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
        split: Result<Result<Vec<Vec<u8>>, ExtractError>, tokio::task::JoinError>,
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
            outcome.pages.extend(pages.iter().map(|p| failed_page(*p)));
            return outcome;
        }

        let sub_pdf = match split {
            Ok(Ok(mut parts)) => parts.pop().unwrap_or_default(),
            Ok(Err(err)) => {
                outcome.error = Some(format!("could not build an OCR page batch: {err}"));
                outcome.pages.extend(pages.iter().map(|p| failed_page(*p)));
                return outcome;
            }
            Err(err) => {
                outcome.error = Some(format!("OCR page split task: {err}"));
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
        let answer = retry(|| provider.read_pdf(&sub_pdf, include_figures)).await;
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
                            if let Some(key) = keys.get(page) {
                                self.cache_put(key, &text).await;
                            }
                            outcome.pages.push((
                                *page,
                                PageOutcome {
                                    text: Some(text),
                                    figures: offset_figures(read.figures, *page),
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
                outcome.error = Some(err.message);
                outcome.pages.extend(pages.iter().map(|p| failed_page(*p)));
            }
        }

        outcome
    }

    async fn cache_get(&self, key: &str) -> Option<String> {
        match &self.cache {
            Some(cache) => cache.get(key).await,
            None => None,
        }
    }

    async fn cache_put(&self, key: &str, text: &str) {
        if let Some(cache) = &self.cache {
            cache.put(key, text).await;
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

    fn all_empty(&self) -> bool {
        self.0.iter().all(|t| t.trim().is_empty())
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
            figures: if want_figures {
                figures
            } else {
                Vec::new()
            },
            pages_blocks: Some(blocks),
            page_map: if page_map.is_empty() {
                None
            } else {
                Some(page_map)
            },
            outline,
            ocr: Some(report),
            error: None,
        }
    }
}

/// One page's marker when it could not be read.
fn marker(page: u32) -> String {
    format!("[page {} could not be read]", page + 1)
}

/// Cache key over the provider's identity and the page's own bytes.
fn page_cache_key(provider: &dyn OcrPageProvider, artifact: &[u8]) -> String {
    let mut hasher = Sha256::new();
    let dpi = provider.dpi().to_le_bytes();
    for part in [
        provider.id().as_bytes(),
        provider.model().as_bytes(),
        provider.prompt().as_bytes(),
        dpi.as_slice(),
        artifact,
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

/// Retry a provider call on transient errors, with exponential backoff.
async fn retry<F, Fut, T>(mut call: F) -> Result<T, OcrCallError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, OcrCallError>>,
{
    let mut last = OcrCallError::permanent("no attempt was made");
    for attempt in 1..=MAX_ATTEMPTS {
        match call().await {
            Ok(value) => return Ok(value),
            Err(err) if err.transient && attempt < MAX_ATTEMPTS => {
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

/// Move a figure's page index to the document page that carried it: a batch
/// response numbers pages from 0 within the sub-PDF, and the reader needs the
/// page the viewer shows.
fn offset_figures(figures: Vec<ExtractedFigure>, page: u32) -> Vec<ExtractedFigure> {
    figures
        .into_iter()
        .map(|mut figure| {
            figure.page = page;
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
            async fn read_image(&self, _image: &[u8], _mime: &str) -> Result<String, OcrCallError> {
                Ok(String::new())
            }
        }
        let base = Fake("chat", "m", "OCR:", 150);
        let key = page_cache_key(&base, b"page");
        assert_eq!(key, page_cache_key(&base, b"page"));
        assert_ne!(key, page_cache_key(&base, b"other page"));
        assert_ne!(key, page_cache_key(&Fake("mistral", "m", "OCR:", 150), b"page"));
        assert_ne!(key, page_cache_key(&Fake("chat", "m2", "OCR:", 150), b"page"));
        assert_ne!(key, page_cache_key(&Fake("chat", "m", "Table:", 150), b"page"));
        assert_ne!(key, page_cache_key(&Fake("chat", "m", "OCR:", 200), b"page"));
    }
}
