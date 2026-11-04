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
use crate::config::ExtractConfig;
use crate::hybrid::HybridExtractor;
use crate::mineru::MineruExtractor;
use crate::mineru_api::MineruApiExtractor;
use crate::mistral::MistralOcrExtractor;
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
}

impl ProductionExtractor {
    /// Create a new production extractor with the given chain and OCR provider.
    #[must_use]
    pub fn new(chain: LayoutExtractorChain, vision_ocr: Option<Arc<dyn ScannedPageOcr>>) -> Self {
        Self { chain, vision_ocr }
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
        crate::dispatch::extract_document(&name, &mime, &bytes, options, &chain, vision.as_deref())
            .await
            .map_err(PortError::from)
    }
}

/// Build the full production extractor wired to the configured layout chain and vision OCR.
pub fn build_production_extractor(
    cfg: &ExtractConfig,
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
    Ok(ProductionExtractor::new(chain, vision_ocr))
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
