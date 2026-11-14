//! Sentinel-based extractor factory and layout chain assembly.
//!
//! Mirrors `buildExtractor` in `src/lib/rag/extractors/index.ts`.
//! Builds extractors from sentinels:
//!   "unpdf"  → PdfiumTextExtractor (replaces UnpdfExtractor)
//!   "mineru" → MineruExtractor
//!   "hybrid" → HybridExtractor(mineru + unpdf)
//!   "smart"  → SmartRouterExtractor(unpdf + smart_fallback)
//!   other    → VisionLlmExtractor with that model id

use std::sync::Arc;

use kb_ports::{ExtractionOptions, ExtractionResult, Extractor, PortError};

use crate::ExtractError;
use crate::chain::{LayoutExtractorChain, ScannedPageOcr};
use crate::config::{ExtractConfig, OcrMode};
use crate::hybrid::HybridExtractor;
use crate::mineru::MineruExtractor;
use crate::mineru_api::MineruApiExtractor;
use crate::mistral::MistralOcrExtractor;
use crate::ocr_router::{
    OcrRouter, OcrRouterSettings, OcrTransport, process_breaker,
};
use crate::openrouter_vision::OpenRouterVisionProvider;
use crate::page_ocr::{PageOcrExtractor, PageOcrSettings};
use crate::pdfium::{extract_pdf_text_layer, get_pdfium};
use crate::smart_router::SmartRouterExtractor;
use crate::vision_llm::VisionLlmExtractor;

/// Text-layer extractor using Pdfium. Replaces `UnpdfExtractor` from the TS codebase.
#[derive(Debug, Clone, Copy, Default)]
pub struct PdfiumTextExtractor;

#[async_trait::async_trait]
impl Extractor for PdfiumTextExtractor {
    async fn extract_with_options(
        &self,
        _filename: &str,
        _mime_type: &str,
        file_bytes: &[u8],
        _options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        let pdfium = get_pdfium()?;
        extract_pdf_text_layer(pdfium, file_bytes).map_err(PortError::from)
    }
}

/// The production extractor wired to all parsers, layout chains, and vision OCR.
#[derive(Clone)]
pub struct ProductionExtractor {
    chain: LayoutExtractorChain,
    vision_ocr: Option<Arc<dyn ScannedPageOcr>>,
    ocr_router: Option<Arc<OcrRouter>>,
}

impl ProductionExtractor {
    /// Create a new production extractor with the given chain and OCR provider.
    #[must_use]
    pub fn new(chain: LayoutExtractorChain, vision_ocr: Option<Arc<dyn ScannedPageOcr>>) -> Self {
        Self {
            chain,
            vision_ocr,
            ocr_router: None,
        }
    }

    /// Add the per-page OCR router (`ING-6`). Without one the PDF path is the
    /// document-level scanned test the service has always used.
    #[must_use]
    pub fn with_ocr_router(mut self, router: Arc<OcrRouter>) -> Self {
        self.ocr_router = Some(router);
        self
    }

    /// The extractor as a port object.
    #[must_use]
    pub fn shared(self) -> Arc<dyn Extractor> {
        Arc::new(self)
    }
}

#[async_trait::async_trait]
impl Extractor for ProductionExtractor {
    async fn extract(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
    ) -> Result<ExtractionResult, PortError> {
        self.extract_with_options(
            filename,
            mime_type,
            file_bytes,
            ExtractionOptions::default(),
        )
        .await
    }

    async fn extract_with_options(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        let name = filename.to_string();
        let mime = mime_type.to_string();
        let bytes = file_bytes.to_vec();
        let chain = self.chain.clone();
        let vision = self.vision_ocr.clone();
        let router = self.ocr_router.clone();
        crate::dispatch::extract_document(
            &name,
            &mime,
            &bytes,
            options,
            &chain,
            vision.as_deref(),
            router.as_deref(),
        )
        .await
        .map_err(PortError::from)
    }

    async fn outline(
        &self,
        mime_type: &str,
        file_bytes: &[u8],
        pages: &[String],
    ) -> Option<kb_core::tree::Outline> {
        if !mime_type.eq_ignore_ascii_case("application/pdf") {
            return None;
        }
        let bytes = file_bytes.to_vec();
        let pages = pages.to_vec();
        tokio::task::spawn_blocking(move || {
            let pdfium = crate::pdfium::get_pdfium().ok()?;
            let outline = crate::outline::extract_outline(pdfium, &bytes, &pages);
            (!outline.nodes.is_empty()).then_some(outline)
        })
        .await
        .ok()
        .flatten()
    }
}

/// Build the full production extractor wired to the configured layout chain and vision OCR.
pub fn build_production_extractor(
    cfg: &ExtractConfig,
) -> Result<ProductionExtractor, ExtractError> {
    build_production_extractor_with_cache(cfg, None)
}

/// The production extractor with the page cache the composition root owns.
///
/// The cache is a `kb-ports` trait, so `kb-extract` never names `kb-cache`;
/// `kb-server` passes its disk implementation and every routed page is served
/// from it on a re-upload.
pub fn build_production_extractor_with_cache(
    cfg: &ExtractConfig,
    page_cache: Option<Arc<dyn kb_ports::OcrPageCache>>,
) -> Result<ProductionExtractor, ExtractError> {
    let chain = build_layout_chain(cfg)?;
    let vision_ocr = build_page_ocr(cfg);
    if !cfg.ocr_base_url.trim().is_empty() && vision_ocr.is_some() {
        tracing::info!(
            url = cfg.ocr_base_url.trim(),
            model = %cfg.ocr_model,
            concurrency = cfg.ocr_concurrency,
            dpi = cfg.ocr_dpi,
            "local page OCR on"
        );
    }
    let mut extractor = ProductionExtractor::new(chain, vision_ocr);
    if let Some(router) = build_ocr_router(cfg, page_cache) {
        tracing::info!(
            mode = cfg.resolved_ocr_mode().as_str(),
            pages = cfg.ocr_max_pages,
            batch = cfg.ocr_batch,
            "OCR page router on"
        );
        extractor = extractor.with_ocr_router(router);
    }
    Ok(extractor)
}

/// The OCR provider the page router uses: the local chat adapter first (a
/// local tier never sends a page to a paid provider), then Mistral when its key
/// and its place in `KB_LAYOUT_EXTRACTOR_ORDER` both say to use it.
fn build_ocr_provider(cfg: &ExtractConfig) -> Option<Arc<dyn crate::ocr_router::OcrPageProvider>> {
    if !cfg.ocr_base_url.trim().is_empty() {
        if let Ok(ocr) = PageOcrExtractor::new(PageOcrSettings::from_extract_config(cfg)) {
            return Some(Arc::new(ocr));
        }
    }
    if !cfg.mistral_ocr_key.trim().is_empty()
        && cfg.layout_extractor_order.iter().any(|name| name == "mistral")
    {
        if let Ok(mistral) = MistralOcrExtractor::new(
            &cfg.mistral_ocr_key,
            Some(cfg.mistral_ocr_base.clone()),
            Some(cfg.mistral_ocr_model.clone()),
            None,
        ) {
            return Some(Arc::new(mistral));
        }
    }
    None
}

/// Build the page router when the mode asks for one.
///
/// `Auto` with no provider stays `None`: that installation keeps the old
/// document-level path, not a router that can never send anything. An explicit
/// `off` still builds one, so `metadata.ocr` reports the pages it did not read.
fn build_ocr_router(
    cfg: &ExtractConfig,
    page_cache: Option<Arc<dyn kb_ports::OcrPageCache>>,
) -> Option<Arc<OcrRouter>> {
    let settings = OcrRouterSettings::from_config(cfg);
    let provider = build_ocr_provider(cfg);
    let mode = settings.mode;

    if mode == OcrMode::Off && cfg.ocr_mode == OcrMode::Auto {
        return None;
    }
    if mode != OcrMode::Off && provider.is_none() {
        tracing::warn!(
            mode = mode.as_str(),
            "KB_OCR_MODE asks for OCR but no provider is configured; keeping the local path"
        );
        return None;
    }

    // A document provider batches pages; a chat provider reads images one at a
    // time and its batch size is fixed at one by the transport.
    let mut settings = settings;
    if let Some(provider) = provider.as_ref() {
        if let OcrTransport::Pdf { batch } = provider.transport() {
            settings.batch = batch;
        }
    }

    let breaker = process_breaker(settings.breaker_threshold, settings.breaker_cooldown);
    Some(Arc::new(OcrRouter::new(
        settings, provider, page_cache, Some(breaker),
    )))
}

/// The local page-OCR adapter, when `KB_OCR_BASE_URL` is set.
fn local_page_ocr(cfg: &ExtractConfig) -> Option<Arc<dyn ScannedPageOcr>> {
    if cfg.ocr_base_url.trim().is_empty() {
        return None;
    }
    match PageOcrExtractor::new(PageOcrSettings::from_extract_config(cfg)) {
        Ok(ocr) => Some(Arc::new(ocr)),
        Err(e) => {
            tracing::warn!(error = %e, "local page OCR not built");
            None
        }
    }
}

/// The OpenRouter vision provider, when its key is present — unchanged.
fn openrouter_page_ocr(cfg: &ExtractConfig) -> Option<Arc<dyn ScannedPageOcr>> {
    if cfg.openrouter_api_key.is_empty() {
        return None;
    }
    OpenRouterVisionProvider::new(
        &cfg.openrouter_api_key,
        Some(cfg.ocr_fallback_model.clone()),
        None,
    )
    .ok()
    .map(|ocr| Arc::new(ocr) as Arc<dyn ScannedPageOcr>)
}

/// The OCR provider for scanned pages and image uploads: the **local** adapter
/// when `KB_OCR_BASE_URL` is set, so the local tiers never send a page to a paid
/// provider; otherwise OpenRouter vision, exactly as before.
fn build_page_ocr(cfg: &ExtractConfig) -> Option<Arc<dyn ScannedPageOcr>> {
    local_page_ocr(cfg).or_else(|| openrouter_page_ocr(cfg))
}

/// Build an extractor from a model-id sentinel or OpenRouter model ID.
pub fn build_extractor(
    model_id: &str,
    cfg: &ExtractConfig,
) -> Result<Arc<dyn Extractor>, ExtractError> {
    match model_id {
        "unpdf" => Ok(Arc::new(PdfiumTextExtractor)),
        "mineru" => {
            let mineru = MineruExtractor::new(&cfg.mineru_base_url, Some(cfg.mineru_timeout))?;
            Ok(Arc::new(mineru))
        }
        "hybrid" => {
            let mineru = Arc::new(MineruExtractor::new(
                &cfg.mineru_base_url,
                Some(cfg.mineru_timeout),
            )?);
            let text_layer = Arc::new(PdfiumTextExtractor);
            Ok(Arc::new(HybridExtractor::new(mineru, text_layer)))
        }
        "smart" => {
            if cfg.smart_fallback == "smart" {
                return Err(ExtractError::Unsupported(
                    "KB_EXTRACT_SMART_FALLBACK cannot be \"smart\" — that would recurse infinitely. Set it to a terminal extractor sentinel (\"unpdf\", \"mineru\", \"hybrid\") or an OpenRouter model id (e.g. \"openai/gpt-4.1-nano\").".to_string(),
                ));
            }
            let fallback = build_extractor(&cfg.smart_fallback, cfg)?;
            let text_layer = Arc::new(PdfiumTextExtractor);
            Ok(Arc::new(SmartRouterExtractor::new(
                text_layer, fallback, None,
            )))
        }
        other => {
            let vision = VisionLlmExtractor::new(
                other,
                Some(cfg.vision_base_url.clone()),
                cfg.effective_vision_api_key(),
                None,
                None,
                None,
            )?;
            Ok(Arc::new(vision))
        }
    }
}

/// Assemble the layout extractor chain according to `KB_LAYOUT_EXTRACTOR_ORDER`.
pub fn build_layout_chain(cfg: &ExtractConfig) -> Result<LayoutExtractorChain, ExtractError> {
    let mut extractors: Vec<(String, Arc<dyn Extractor>)> = Vec::new();

    for name in &cfg.layout_extractor_order {
        match name.as_str() {
            "sidecar" => {
                if !cfg.mineru_base_url.is_empty() {
                    if let Ok(sidecar) =
                        MineruExtractor::new(&cfg.mineru_base_url, Some(cfg.mineru_timeout))
                    {
                        extractors.push(("sidecar".to_string(), Arc::new(sidecar)));
                    }
                }
            }
            "mineru-api" => {
                if !cfg.mineru_api_key.is_empty() {
                    if let Ok(api) = MineruApiExtractor::new(
                        &cfg.mineru_api_key,
                        Some(cfg.mineru_api_base.clone()),
                        Some(cfg.mineru_api_lang.clone()),
                        Some(cfg.mineru_api_timeout),
                    ) {
                        extractors.push(("mineru-api".to_string(), Arc::new(api)));
                    }
                }
            }
            "mistral" if !cfg.mistral_ocr_key.is_empty() => {
                if let Ok(mistral) = MistralOcrExtractor::new(
                    &cfg.mistral_ocr_key,
                    Some(cfg.mistral_ocr_base.clone()),
                    Some(cfg.mistral_ocr_model.clone()),
                    None,
                ) {
                    extractors.push(("mistral".to_string(), Arc::new(mistral)));
                }
            }
            // Local page OCR (`KB_OCR_BASE_URL`), the local tiers' scanner:
            // listed in the order it runs like any other parser, and skipped
            // when no endpoint is configured — the same rule `mistral` follows
            // for its key. When it is not listed it still serves as the chain's
            // final fallback (see `build_page_ocr`).
            "ocr" if !cfg.ocr_base_url.trim().is_empty() => {
                if let Ok(ocr) = PageOcrExtractor::new(PageOcrSettings::from_extract_config(cfg)) {
                    extractors.push(("ocr".to_string(), Arc::new(ocr)));
                }
            }
            _ => {}
        }
    }

    // The final fallback is the local adapter when it is configured, so a
    // scanned page never reaches a paid provider on a local tier.
    let fallback_ocr = build_page_ocr(cfg);

    Ok(LayoutExtractorChain::new(extractors, fallback_ocr))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn build_extractor_detects_smart_infinite_recursion() {
        let cfg = ExtractConfig {
            smart_fallback: "smart".to_string(),
            ..Default::default()
        };
        match build_extractor("smart", &cfg) {
            Err(err) => {
                assert!(err.to_string().contains("cannot be \"smart\""));
            }
            Ok(_) => panic!("expected smart recursion error"),
        }
    }

    #[test]
    fn build_extractor_unpdf_succeeds() {
        let cfg = ExtractConfig::default();
        assert!(build_extractor("unpdf", &cfg).is_ok());
    }

    #[test]
    fn the_default_order_is_byte_identical_and_builds_no_local_ocr() {
        let cfg = ExtractConfig::default();
        assert_eq!(
            cfg.layout_extractor_order,
            vec!["sidecar", "mineru-api", "mistral"],
            "adding the `ocr` adapter must not change the default order"
        );
        let chain = build_layout_chain(&cfg).unwrap();
        assert!(chain.extractor_names().is_empty(), "nothing is configured");
        assert!(
            !chain.has_fallback_ocr(),
            "no HTTP key and no OCR URL means no provider"
        );
    }

    #[test]
    fn ocr_joins_the_chain_only_when_its_endpoint_is_set() {
        let mut cfg = ExtractConfig {
            layout_extractor_order: vec!["ocr".to_string()],
            ..Default::default()
        };
        // Listed but unconfigured: exactly like `mistral` without its key.
        let chain = build_layout_chain(&cfg).unwrap();
        assert!(chain.extractor_names().is_empty());
        assert!(!chain.has_fallback_ocr());

        cfg.ocr_base_url = "http://127.0.0.1:8000/v1/chat/completions".to_string();
        let chain = build_layout_chain(&cfg).unwrap();
        assert_eq!(chain.extractor_names(), vec!["ocr"]);
        assert!(
            chain.has_fallback_ocr(),
            "the local adapter is also the scanned-page fallback"
        );
    }

    #[test]
    fn a_configured_local_ocr_wins_over_the_openrouter_vision_provider() {
        // With both configured, the local adapter is the one that serves
        // scanned PDFs, because that is the tier's "no paid call" rule.
        let cfg = ExtractConfig {
            layout_extractor_order: vec!["ocr".to_string()],
            ocr_base_url: "http://127.0.0.1:8000/v1/chat/completions".to_string(),
            openrouter_api_key: "test-key".to_string(),
            ..Default::default()
        };
        let chain = build_layout_chain(&cfg).unwrap();
        assert_eq!(chain.extractor_names(), vec!["ocr"]);
        assert!(chain.has_fallback_ocr());
        assert!(build_production_extractor(&cfg).is_ok());
    }
}
