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
//!
//! Not ported yet, and refused with an explicit error rather than guessed at:
//! PDFs (text layer and layout extractors), images (OCR), and the legacy binary
//! Office/OpenDocument formats that the TypeScript service hands to
//! `officeparser`.

mod code;
mod detect;
mod dispatch;
mod docx;
mod epub;
mod gltf;
mod html;
mod mime;
mod pptx;
mod rtf;
#[cfg(test)]
mod testkit;
mod text;
mod xlsx;
mod xml;
mod zip_read;

pub use code::CODE_EXT_LANGUAGE;
pub use detect::{SupportedFileType, detect_file_type, extension_of};
pub use dispatch::{extract, extract_from_buffer};
pub use mime::{DOCUMENT_MIME_TYPES, ext_to_mime, is_document_mime, mime_to_ext};

use async_trait::async_trait;
use kb_ports::{ExtractionResult, Extractor, PortError};

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

/// The extractor the production worker is bound to: everything local, no
/// network, no model.
///
/// PDFs and images are refused — the layout and OCR paths land in a later slice,
/// and an empty result would be indexed as if it were the document's text.
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
