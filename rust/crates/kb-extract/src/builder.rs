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
use crate::chain::LayoutExtractorChain;
use crate::config::ExtractConfig;
use crate::hybrid::HybridExtractor;
use crate::mineru::MineruExtractor;
use crate::mineru_api::MineruApiExtractor;
use crate::mistral::MistralOcrExtractor;
use crate::openrouter_vision::OpenRouterVisionProvider;
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
    vision_ocr: Option<OpenRouterVisionProvider>,
}

impl ProductionExtractor {
    /// Create a new production extractor with the given chain and vision provider.
    #[must_use]
    pub fn new(chain: LayoutExtractorChain, vision_ocr: Option<OpenRouterVisionProvider>) -> Self {
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
        crate::dispatch::extract_document(&name, &mime, &bytes, options, &chain, vision.as_ref())
            .await
            .map_err(PortError::from)
    }
}

/// Build the full production extractor wired to the configured layout chain and vision OCR.
pub fn build_production_extractor(
    cfg: &ExtractConfig,
) -> Result<ProductionExtractor, ExtractError> {
    let chain = build_layout_chain(cfg)?;
    let vision_ocr = if !cfg.openrouter_api_key.is_empty() {
        OpenRouterVisionProvider::new(
            &cfg.openrouter_api_key,
            Some(cfg.ocr_fallback_model.clone()),
            None,
        )
        .ok()
    } else {
        None
    };
    Ok(ProductionExtractor::new(chain, vision_ocr))
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
            _ => {}
        }
    }

    let fallback_ocr = if !cfg.openrouter_api_key.is_empty() {
        OpenRouterVisionProvider::new(
            &cfg.openrouter_api_key,
            Some(cfg.ocr_fallback_model.clone()),
            None,
        )
        .ok()
        .map(Arc::new)
    } else {
        None
    };

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
}
