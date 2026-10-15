//! Vision-LLM PDF extractor.
//!
//! Mirrors `src/lib/rag/extractors/vision-llm-extractor.ts`.
//! Sends the PDF to an OpenAI-compatible / OpenRouter chat endpoint using the
//! `file` content type and asks for clean, compact Markdown. Splits large PDFs
//! into page segments to respect output-token limits.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use kb_ports::{ExtractionOptions, ExtractionResult, Extractor, PortError};
use serde::{Deserialize, Serialize};

use crate::ExtractError;
use crate::pdfium::{get_pdf_page_count, get_pdfium, split_pdf_by_page_count};

/// Prompt used for Vision-LLM text extraction.
pub const EXTRACTION_PROMPT: &str = r#"Extract the full textual content of this PDF as clean, COMPACT Markdown.

Strict rules:
- Headings: # / ## / ### matching document hierarchy
- Lists: "- " or "1. " with ONE space
- Tables: standard Markdown pipes with a single space of padding on each side of cell content. DO NOT pad cells with many spaces or align columns — compact tables only.
- Inline math: $...$ ; block math: $$...$$
- Code blocks: fenced with triple backticks

Do not summarize. Do not omit content. Do not add commentary. Output ONLY the extracted Markdown."#;

/// Vision-LLM PDF extractor targeting OpenRouter or an OpenAI-compatible endpoint.
#[derive(Debug, Clone)]
pub struct VisionLlmExtractor {
    model: String,
    base_url: String,
    api_key: String,
    max_tokens: u32,
    segment_pages: u32,
    segment_concurrency: usize,
    client: reqwest::Client,
}

impl VisionLlmExtractor {
    /// Create a new VisionLlmExtractor.
    pub fn new(
        model: impl Into<String>,
        base_url: Option<String>,
        api_key: impl Into<String>,
        max_tokens: Option<u32>,
        segment_pages: Option<u32>,
        segment_concurrency: Option<usize>,
    ) -> Result<Self, ExtractError> {
        let model = model.into().trim().to_string();
        let api_key = api_key.into().trim().to_string();
        if api_key.is_empty() {
            return Err(ExtractError::Unsupported(
                "VisionLlmExtractor requires an API key — set KB_EXTRACT_VISION_API_KEY or OPENROUTER_API_KEY".to_string(),
            ));
        }

        let base_url = base_url
            .unwrap_or_else(|| "https://openrouter.ai/api/v1/chat/completions".to_string())
            .trim()
            .to_string();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5 * 60))
            .build()
            .map_err(|e| ExtractError::Container(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            model,
            base_url,
            api_key,
            max_tokens: max_tokens.unwrap_or(16000),
            segment_pages: segment_pages.unwrap_or(25),
            segment_concurrency: segment_concurrency.unwrap_or(4),
            client,
        })
    }

    /// Extract markdown text from PDF bytes.
    pub async fn extract_pdf(&self, pdf_bytes: &[u8]) -> Result<ExtractionResult, ExtractError> {
        let pdfium = get_pdfium()?;
        let page_count = get_pdf_page_count(pdfium, pdf_bytes).unwrap_or(0);

        if page_count == 0 || page_count <= self.segment_pages {
            let text = self.extract_single_call(pdf_bytes, "document.pdf").await?;
            return Ok(ExtractionResult {
                content: text,
                file_type: "pdf".to_string(),
                used_ocr: true,
                figures: Vec::new(),
                pages_blocks: None,
                page_map: None,
                error: None,
            });
        }

        // Split into segments and process in parallel
        let segments = split_pdf_by_page_count(pdfium, pdf_bytes, self.segment_pages)?;
        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.segment_concurrency));

        let mut tasks = Vec::with_capacity(segments.len());
        for (i, segment) in segments.into_iter().enumerate() {
            let sem = semaphore.clone();
            let this = self.clone();
            let seg_name = format!("document-segment-{}-of-{}.pdf", i + 1, tasks.capacity());
            tasks.push(tokio::spawn(async move {
                let _permit = sem
                    .acquire()
                    .await
                    .map_err(|e| ExtractError::Container(e.to_string()))?;
                this.extract_single_call(&segment, &seg_name).await
            }));
        }

        let mut results = Vec::with_capacity(tasks.len());
        for task in tasks {
            let text = task
                .await
                .map_err(|e| ExtractError::Container(format!("segment task panicked: {e}")))?
                .map_err(|e| ExtractError::Container(format!("segment extraction failed: {e}")))?;
            results.push(text);
        }

        let combined = results.join("\n\n");
        Ok(ExtractionResult {
            content: combined,
            file_type: "pdf".to_string(),
            used_ocr: true,
            figures: Vec::new(),
            pages_blocks: None,
            page_map: None,
            error: None,
        })
    }

    async fn extract_single_call(
        &self,
        pdf_bytes: &[u8],
        filename: &str,
    ) -> Result<String, ExtractError> {
        let b64 = BASE64_STANDARD.encode(pdf_bytes);
        let req = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: vec![
                    ChatContentPart::File {
                        part_type: "file".to_string(),
                        file: ChatFilePayload {
                            filename: filename.to_string(),
                            file_data: format!("data:application/pdf;base64,{b64}"),
                        },
                    },
                    ChatContentPart::Text {
                        part_type: "text".to_string(),
                        text: EXTRACTION_PROMPT.to_string(),
                    },
                ],
            }],
            max_tokens: self.max_tokens,
            temperature: 0.0,
        };

        let res = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| ExtractError::Container(format!("vision-llm request failed: {e}")))?;

        if !res.status().is_success() {
            let body = res.text().await.unwrap_or_default();
            let preview = if body.len() > 300 { &body[..300] } else { &body };
            return Err(ExtractError::Container(format!(
                "vision-llm {} error: {preview}",
                self.model
            )));
        }

        let resp_json: ChatCompletionResponse = res
            .json()
            .await
            .map_err(|e| ExtractError::Container(format!("vision-llm invalid JSON: {e}")))?;

        let text = resp_json
            .choices
            .and_then(|mut c| c.pop())
            .and_then(|c| c.message)
            .and_then(|m| m.content)
            .unwrap_or_default();

        Ok(text)
    }
}

#[async_trait::async_trait]
impl Extractor for VisionLlmExtractor {
    async fn extract_with_options(
        &self,
        _filename: &str,
        _mime_type: &str,
        file_bytes: &[u8],
        _options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        self.extract_pdf(file_bytes)
            .await
            .map_err(PortError::from)
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: Vec<ChatContentPart>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatContentPart {
    Text {
        #[serde(rename = "type")]
        part_type: String,
        text: String,
    },
    File {
        #[serde(rename = "type")]
        part_type: String,
        file: ChatFilePayload,
    },
}

#[derive(Serialize)]
struct ChatFilePayload {
    filename: String,
    file_data: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Option<Vec<ChatChoice>>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatMessageResponse>,
}

#[derive(Deserialize)]
struct ChatMessageResponse {
    content: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn vision_extractor_requires_api_key() {
        assert!(VisionLlmExtractor::new("openai/gpt-4.1-nano", None, "", None, None, None).is_err());
    }
}
