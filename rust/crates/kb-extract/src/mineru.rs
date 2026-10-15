//! Client for the on-prem MinerU sidecar extractor.
//!
//! Mirrors `src/lib/rag/extractors/mineru-extractor.ts`.
//! Posts the PDF as multipart form data to `{base_url}/extract` and receives
//! markdown, per-page text, and (in structured mode) figures and reading blocks.

use std::time::Duration;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use kb_ports::{
    ExtractedFigure, ExtractionOptions, ExtractionResult, PageBlock, PageBlockKind, PageText,
    PortError,
};
use serde::Deserialize;

use crate::ExtractError;

/// Client for the on-prem MinerU sidecar extractor.
#[derive(Debug, Clone)]
pub struct MineruExtractor {
    base_url: String,
    #[allow(dead_code)]
    timeout: Duration,
    client: reqwest::Client,
}

impl MineruExtractor {
    /// Create a new extractor client pointing at the sidecar base URL.
    pub fn new(base_url: impl Into<String>, timeout: Option<Duration>) -> Result<Self, ExtractError> {
        let raw = base_url.into();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ExtractError::Unsupported(
                "MineruExtractor requires a base URL — set KB_EXTRACT_MINERU_BASE_URL (e.g. http://localhost:8100)".to_string(),
            ));
        }
        let normalized = trimmed.trim_end_matches('/').trim_end_matches("/extract").to_string();
        let timeout = timeout.unwrap_or(Duration::from_secs(20 * 60)); // 20 minutes default
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ExtractError::Container(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            base_url: normalized,
            timeout,
            client,
        })
    }

    /// Extract text and figures from PDF bytes.
    pub async fn extract_pdf(
        &self,
        pdf_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, ExtractError> {
        let boundary = format!(
            "----mineru{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let crlf = "\r\n";

        let mut body = Vec::new();
        // File part
        body.extend_from_slice(
            format!(
                "--{boundary}{crlf}Content-Disposition: form-data; name=\"file\"; filename=\"document.pdf\"{crlf}Content-Type: application/pdf{crlf}{crlf}"
            )
            .as_bytes(),
        );
        body.extend_from_slice(pdf_bytes);

        // Optional structured part
        if options.figures {
            body.extend_from_slice(
                format!(
                    "{crlf}--{boundary}{crlf}Content-Disposition: form-data; name=\"structured\"{crlf}{crlf}true"
                )
                .as_bytes(),
            );
        }
        body.extend_from_slice(format!("{crlf}--{boundary}--{crlf}").as_bytes());

        let url = format!("{}/extract", self.base_url);
        let res = self
            .client
            .post(&url)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .map_err(|e| ExtractError::Container(format!("mineru sidecar request failed: {e}")))?;

        let status = res.status();
        if !status.is_success() {
            let body_text = res.text().await.unwrap_or_default();
            let preview = if body_text.len() > 300 {
                &body_text[..300]
            } else {
                &body_text
            };
            return Err(ExtractError::Container(format!(
                "mineru sidecar {status}: {preview}"
            )));
        }

        let resp_json: MineruResponse = res
            .json()
            .await
            .map_err(|e| ExtractError::Container(format!("mineru sidecar invalid JSON: {e}")))?;

        let text = resp_json.text.unwrap_or_default();

        let figures = if options.figures {
            resp_json
                .figures
                .unwrap_or_default()
                .into_iter()
                .filter_map(|f| {
                    let image_bytes = BASE64_STANDARD.decode(f.image_b64.as_bytes()).ok()?;
                    Some(ExtractedFigure {
                        kind: f.kind,
                        page: f.page,
                        bbox: f.bbox,
                        caption: f.caption,
                        image_bytes,
                        id: None,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };

        let page_map = resp_json.pages_text.map(|pt| {
            pt.into_iter()
                .enumerate()
                .map(|(page, text)| PageText {
                    page: page as u32,
                    text,
                })
                .collect()
        });

        let pages_blocks = resp_json.pages_blocks.map(|pb| {
            pb.into_iter()
                .map(|page| {
                    page.into_iter()
                        .map(|b| {
                            let kind = match b.kind.as_str() {
                                "figure" => PageBlockKind::Figure,
                                "caption" => PageBlockKind::Caption,
                                _ => PageBlockKind::Text,
                            };
                            PageBlock {
                                kind,
                                id: b.id,
                                text: b.text,
                            }
                        })
                        .collect()
                })
                .collect()
        });

        Ok(ExtractionResult {
            content: text,
            file_type: "pdf".to_string(),
            used_ocr: true,
            figures,
            pages_blocks,
            page_map,
            error: None,
        })
    }
}

#[async_trait::async_trait]
impl kb_ports::Extractor for MineruExtractor {
    async fn extract_with_options(
        &self,
        _filename: &str,
        _mime_type: &str,
        file_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        self.extract_pdf(file_bytes, options)
            .await
            .map_err(PortError::from)
    }
}

#[derive(Deserialize)]
struct MineruResponse {
    text: Option<String>,
    #[allow(dead_code)]
    ms: Option<u64>,
    #[allow(dead_code)]
    pages: Option<u32>,
    pages_text: Option<Vec<String>>,
    figures: Option<Vec<MineruFigure>>,
    pages_blocks: Option<Vec<Vec<MineruBlock>>>,
}

#[derive(Deserialize)]
struct MineruFigure {
    #[serde(rename = "type")]
    kind: String,
    page: u32,
    bbox: [f32; 4],
    caption: Option<String>,
    image_b64: String,
    #[allow(dead_code)]
    id: Option<String>,
    #[allow(dead_code)]
    block_index: Option<u32>,
}

#[derive(Deserialize)]
struct MineruBlock {
    kind: String,
    id: Option<String>,
    text: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_base_url() {
        let e = MineruExtractor::new("http://localhost:8100/extract/", None).unwrap();
        assert_eq!(e.base_url, "http://localhost:8100");
    }

    #[test]
    fn rejects_empty_base_url() {
        assert!(MineruExtractor::new("   ", None).is_err());
    }
}
