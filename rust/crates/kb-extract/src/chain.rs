//! Layout extractor chain for scanned PDFs.
//!
//! Mirrors the layout extraction chain in `src/lib/ingest/extract.ts:51-118`.
//! Tries configured layout extractors (sidecar, mineru-api, mistral) in order
//! until one yields non-blank text. Falls through to OpenRouter vision OCR
//! if the whole chain fails.

use std::sync::Arc;

use kb_ports::{ExtractionOptions, ExtractionResult, Extractor, PortError};

use crate::ExtractError;
use crate::clip_reason;
use crate::openrouter_vision::OpenRouterVisionProvider;

/// An ordered chain of layout extractors with a final vision OCR fallback.
#[derive(Clone)]
pub struct LayoutExtractorChain {
    extractors: Vec<(String, Arc<dyn Extractor>)>,
    fallback_ocr: Option<Arc<OpenRouterVisionProvider>>,
}

impl LayoutExtractorChain {
    /// Create a new layout extractor chain.
    #[must_use]
    pub fn new(
        extractors: Vec<(String, Arc<dyn Extractor>)>,
        fallback_ocr: Option<Arc<OpenRouterVisionProvider>>,
    ) -> Self {
        Self {
            extractors,
            fallback_ocr,
        }
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
                .extract_with_options(filename, "application/pdf", pdf_bytes, options)
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
            match ocr.process_scanned_pdf(pdf_bytes, filename).await {
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
