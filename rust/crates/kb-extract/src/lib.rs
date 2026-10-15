//! Text extraction: the parser registry and the `Extractor` adapter.
//!
//! This crate is the Rust side of `src/lib/ingest/extract.ts` plus the parser
//! registry it dispatches into (`src/lib/files/parsers.ts`,
//! `src/lib/files/mime-types.ts`, and `detectFileType` in
//! `src/lib/rag/file-processor.ts`). It is an adapter rather than domain logic:
//! it reads file formats, so it sits beside `kb-store-*` and `kb-inference`
//! rather than in `kb-core`, and `kb-engine` reaches it only through the
//! `Extractor` port.
//!
//! One rule governs every branch, and it is the one the TypeScript rewrite
//! established: a file that cannot be read produces a reason and no content.
//! Nothing here ever returns placeholder text for a failed parse, because the
//! chunker cannot tell it apart from real text and would index it.

pub mod builder;
pub mod chain;
mod code;
pub mod config;
mod detect;
mod dispatch;
mod docx;
mod epub;
mod gltf;
mod html;
pub mod hybrid;
pub mod hybrid_merge;
mod mime;
pub mod mineru;
pub mod mineru_api;
pub mod mistral;
pub mod ods;
pub mod odt;
pub mod openrouter_vision;
pub mod pdfium;
mod pptx;
mod rtf;
pub mod scanned;
pub mod smart_router;
#[cfg(test)]
mod testkit;
mod text;
pub mod vision_llm;
mod xlsx;
mod xml;
mod zip_read;

pub use builder::{
    PdfiumTextExtractor, ProductionExtractor, build_extractor, build_layout_chain,
    build_production_extractor,
};
pub use chain::LayoutExtractorChain;
pub use code::CODE_EXT_LANGUAGE;
pub use config::ExtractConfig;
pub use detect::{SupportedFileType, detect_file_type, extension_of};
pub use dispatch::{extract, extract_document, extract_from_buffer};
pub use hybrid::HybridExtractor;
pub use hybrid_merge::merge_structural_with_text_layer;
pub use mime::{DOCUMENT_MIME_TYPES, ext_to_mime, is_document_mime, mime_to_ext};
pub use mineru::MineruExtractor;
pub use mineru_api::MineruApiExtractor;
pub use mistral::MistralOcrExtractor;
pub use ods::parse_ods;
pub use odt::parse_odt;
pub use openrouter_vision::OpenRouterVisionProvider;
pub use pdfium::{
    MINIMAL_BLANK_PDF, MINIMAL_PDF_HELLO_WORLD, extract_pdf_text_layer, get_pdf_page_count,
    get_pdfium, init_pdfium, render_pdf_page_to_png, split_pdf_by_page_count,
};
pub use scanned::is_pdf_scanned;
pub use smart_router::SmartRouterExtractor;
pub use vision_llm::VisionLlmExtractor;

use async_trait::async_trait;
use kb_ports::{ExtractionOptions, ExtractionResult, Extractor, PortError};

/// Why a file could not be turned into text.
///
/// Every variant ends up as the reason on the ingest job row, so the messages
/// name the file and the format rather than a Rust error type.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// The format is recognised but not ported yet, or has no parser at all.
    #[error("{0}")]
    Unsupported(String),
    /// The bytes are not the container the parser expects, or an entry is
    /// missing from it.
    #[error("{0}")]
    Container(String),
    /// The container held XML that would not parse.
    #[error("{0}")]
    Xml(String),
}

impl From<ExtractError> for PortError {
    /// A parser failure is malformed input, not an unavailable backend: the
    /// engine turns it into a job failure with the parser's own message.
    fn from(err: ExtractError) -> Self {
        PortError::Malformed(err.to_string())
    }
}

/// Longest error message the TypeScript service keeps from a failed parse
/// (`extract.ts:124`, `.slice(0, 200)`).
const MAX_REASON_UTF16: usize = 200;

/// Clip a parser reason to the TypeScript length, counted in UTF-16 units
/// because that is what `String.prototype.slice` counts.
pub(crate) fn clip_reason(reason: &str) -> String {
    let mut out = String::new();
    let mut units = 0usize;
    for ch in reason.chars() {
        let width = ch.len_utf16();
        if units + width > MAX_REASON_UTF16 {
            break;
        }
        units += width;
        out.push(ch);
    }
    out
}

/// The extractor the local worker or tests can bind to: everything local, no
/// network, no model.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalExtractor;

impl LocalExtractor {
    /// The extractor as a port object, for the composition root.
    #[must_use]
    pub fn shared() -> std::sync::Arc<dyn Extractor> {
        std::sync::Arc::new(Self)
    }
}

#[async_trait]
impl Extractor for LocalExtractor {
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
        _options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        let name = filename.to_string();
        let mime = mime_type.to_string();
        let bytes = file_bytes.to_vec();
        // Parsing a 50 MB docx is CPU-bound work on bytes we already hold, so
        // it runs off the runtime's worker threads rather than stalling them.
        tokio::task::spawn_blocking(move || dispatch::extract(&name, &mime, &bytes))
            .await
            .map_err(|e| PortError::Unavailable(format!("extractor task failed: {e}")))?
            .map_err(PortError::from)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_clipped_in_utf16_units() {
        assert_eq!(clip_reason("abc"), "abc");
        // A non-BMP character is two UTF-16 units, so 150 of them fill the
        // 200-unit budget that 200 `char`s would not.
        let emoji = "\u{1F600}".repeat(250);
        assert_eq!(clip_reason(&emoji).chars().count(), 100);
        assert_eq!(clip_reason(&emoji).encode_utf16().count(), MAX_REASON_UTF16);
    }
}
