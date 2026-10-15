//! Smart router extractor that tries text-layer first and falls back to OCR/layout model.
//!
//! Mirrors `src/lib/rag/extractors/smart-router-extractor.ts`.
//! Uses heuristic signals from `kb-core::signals` to judge if text-layer output
//! is sufficient or if the document contains tables/currency/scans needing OCR.

use std::sync::Arc;

use kb_core::signals::{RouterOpts, is_unpdf_sufficient};
use kb_ports::{ExtractionOptions, ExtractionResult, Extractor, PortError};

use crate::ExtractError;

/// Smart router extractor running text-layer first, falling through to OCR.
pub struct SmartRouterExtractor {
    text_layer: Arc<dyn Extractor>,
    fallback: Arc<dyn Extractor>,
    opts: RouterOpts,
}

impl SmartRouterExtractor {
    /// Create a new SmartRouterExtractor.
    #[must_use]
    pub fn new(
        text_layer: Arc<dyn Extractor>,
        fallback: Arc<dyn Extractor>,
        opts: Option<RouterOpts>,
    ) -> Self {
        Self {
            text_layer,
            fallback,
            opts: opts.unwrap_or_default(),
        }
    }
}

#[async_trait::async_trait]
impl Extractor for SmartRouterExtractor {
    async fn extract_with_options(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        let text_layer_res = self
            .text_layer
            .extract_with_options(filename, mime_type, file_bytes, options.clone())
            .await;

        let tl_err_msg = match text_layer_res {
            Ok(res) => {
                let page_count = res.page_map.as_ref().map_or(1, |m| m.len() as u32).max(1);
                if is_unpdf_sufficient(&res.content, page_count, self.opts) {
                    return Ok(res);
                }
                "insufficient text output".to_string()
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(
                    "[rag/smart-router] text-layer extractor threw ({msg}), falling through to fallback"
                );
                msg
            }
        };

        match self
            .fallback
            .extract_with_options(filename, mime_type, file_bytes, options)
            .await
        {
            Ok(fb_res) => Ok(fb_res),
            Err(fb_err) => {
                let msg =
                    format!("Both extractors failed — textLayer: {tl_err_msg}; fallback: {fb_err}");
                Err(PortError::from(ExtractError::Container(msg)))
            }
        }
    }
}
