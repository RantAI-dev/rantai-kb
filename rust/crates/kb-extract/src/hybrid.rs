//! Hybrid extractor that runs a structural extractor and a text-layer extractor in parallel.
//!
//! Mirrors `src/lib/rag/extractors/hybrid-extractor.ts`.
//! Combines tables and layout reading order from the structural model with
//! character-fidelity prose from the text layer.

use std::sync::Arc;

use kb_ports::{ExtractionOptions, ExtractionResult, Extractor, PortError};

use crate::ExtractError;
use crate::hybrid_merge::merge_structural_with_text_layer;

/// Hybrid extractor combining structural extraction with text-layer extraction.
pub struct HybridExtractor {
    structural: Arc<dyn Extractor>,
    text_layer: Arc<dyn Extractor>,
}

impl HybridExtractor {
    /// Create a new hybrid extractor from structural and text-layer components.
    #[must_use]
    pub fn new(structural: Arc<dyn Extractor>, text_layer: Arc<dyn Extractor>) -> Self {
        Self {
            structural,
            text_layer,
        }
    }
}

#[async_trait::async_trait]
impl Extractor for HybridExtractor {
    async fn extract_with_options(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        let (structural_res, text_layer_res) = tokio::join!(
            self.structural
                .extract_with_options(filename, mime_type, file_bytes, options),
            self.text_layer
                .extract_with_options(filename, mime_type, file_bytes, options),
        );

        match (structural_res, text_layer_res) {
            (Ok(struct_val), Ok(tl_val)) => {
                let merged = merge_structural_with_text_layer(&struct_val.content, &tl_val.content);
                Ok(ExtractionResult {
                    content: merged,
                    file_type: struct_val.file_type,
                    used_ocr: struct_val.used_ocr,
                    figures: struct_val.figures,
                    pages_blocks: struct_val.pages_blocks,
                    page_map: struct_val.page_map.or(tl_val.page_map),
                    error: None,
                })
            }
            (Ok(struct_val), Err(tl_err)) => {
                tracing::warn!(
                    "[rag/hybrid] text-layer extractor failed ({tl_err}) — returning structural-only output"
                );
                Ok(struct_val)
            }
            (Err(struct_err), Ok(tl_val)) => {
                tracing::warn!(
                    "[rag/hybrid] structural extractor failed ({struct_err}) — returning text-layer-only output"
                );
                Ok(tl_val)
            }
            (Err(struct_err), Err(tl_err)) => {
                let msg = format!(
                    "Both extractors failed — structural: {struct_err}; textLayer: {tl_err}"
                );
                Err(PortError::from(ExtractError::Container(msg)))
            }
        }
    }
}
