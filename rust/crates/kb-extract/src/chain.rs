//! Layout extractor chain for scanned PDFs.
//!
//! Mirrors the layout extraction chain in `src/lib/ingest/extract.ts:51-118`.
//! Tries configured layout extractors (sidecar, mineru-api, mistral, ocr) in
//! order until one yields non-blank text. Falls through to a page-OCR provider
//! if the whole chain fails: the local adapter (`KB_OCR_BASE_URL`) when it is
//! configured, otherwise OpenRouter vision OCR.

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{ExtractionOptions, ExtractionResult, Extractor, PortError};

use crate::ExtractError;
use crate::clip_reason;

/// A provider that can turn scanned pages or an image upload into text.
///
/// Two adapters implement it: the local page OCR
/// ([`crate::page_ocr::PageOcrExtractor`], the local tiers) and the OpenRouter
/// vision provider ([`crate::openrouter_vision::OpenRouterVisionProvider`],
/// the cloud path). The chain and the dispatcher hold whichever one the
/// composition root chose, so the local adapter wins when it is configured
/// without either caller knowing which it got.
#[async_trait]
pub trait ScannedPageOcr: Send + Sync {
    /// OCR every page of a scanned PDF.
    async fn ocr_scanned_pdf(
        &self,
        pdf_bytes: &[u8],
        filename: &str,
    ) -> Result<ExtractionResult, ExtractError>;

    /// OCR one image upload (`png`, `jpg`, `tif`).
    async fn ocr_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        filename: &str,
    ) -> Result<ExtractionResult, ExtractError>;
}

/// An ordered chain of layout extractors with a final page-OCR fallback.
#[derive(Clone)]
pub struct LayoutExtractorChain {
    extractors: Vec<(String, Arc<dyn Extractor>)>,
    fallback_ocr: Option<Arc<dyn ScannedPageOcr>>,
}

impl LayoutExtractorChain {
    /// Create a new layout extractor chain.
    #[must_use]
    pub fn new(
        extractors: Vec<(String, Arc<dyn Extractor>)>,
        fallback_ocr: Option<Arc<dyn ScannedPageOcr>>,
    ) -> Self {
        Self {
            extractors,
            fallback_ocr,
        }
    }

    /// The names of the configured parsers, in attempt order. Empty when no
    /// parser is configured; `"ocr"` appears when the local adapter is both
    /// configured and listed in `KB_LAYOUT_EXTRACTOR_ORDER`.
    #[must_use]
    pub fn extractor_names(&self) -> Vec<&str> {
        self.extractors
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Whether a final page-OCR fallback is configured.
    #[must_use]
    pub fn has_fallback_ocr(&self) -> bool {
        self.fallback_ocr.is_some()
    }

    /// Extract scanned PDF text through the chain.
    pub async fn extract_scanned_pdf(
        &self,
        filename: &str,
        pdf_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, ExtractError> {
        for (name, extractor) in &self.extractors {
            match extractor
                .extract_with_options(filename, "application/pdf", pdf_bytes, options.clone())
                .await
            {
                Ok(res) if !res.content.trim().is_empty() => {
                    tracing::info!(
                        "[Knowledge] Layout extraction via {name} ({} figures, {} page blocks)",
                        res.figures.len(),
                        res.page_map.as_ref().map_or(0, |m| m.len())
                    );
                    return Ok(res);
                }
                Ok(_) => {
                    tracing::warn!("[Knowledge] {name} returned no text, trying next extractor");
                }
                Err(err) => {
                    tracing::warn!("[Knowledge] {name} extraction failed, trying next: {err}");
                }
            }
        }

        // Final OCR fallback
        if let Some(ocr) = &self.fallback_ocr {
            match ocr.ocr_scanned_pdf(pdf_bytes, filename).await {
                Ok(res) => Ok(res),
                Err(err) => {
                    let msg = clip_reason(&err.to_string());
                    Err(ExtractError::Container(format!(
                        "OCR failed for \"{filename}\": {msg}"
                    )))
                }
            }
        } else {
            Err(ExtractError::Container(format!(
                "OCR failed for \"{filename}\": all layout extractors failed and no fallback OCR configured"
            )))
        }
    }
}

#[async_trait::async_trait]
impl Extractor for LayoutExtractorChain {
    async fn extract_with_options(
        &self,
        filename: &str,
        _mime_type: &str,
        file_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        self.extract_scanned_pdf(filename, file_bytes, options)
            .await
            .map_err(PortError::from)
    }
}
