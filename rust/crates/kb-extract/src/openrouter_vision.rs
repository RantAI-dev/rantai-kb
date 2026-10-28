//! OpenRouter / OpenAI-compatible Vision OCR provider for images and scanned PDF fallback.
//!
//! Replaces the legacy local Ollama OCR pipeline dropped in this slice.
//! Mirrors `src/lib/ocr/providers/openrouter-provider.ts` and `src/lib/ocr/ocr-pipeline.ts`.

use std::time::Duration;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use kb_ports::ExtractionResult;
use serde::{Deserialize, Serialize};

use crate::ExtractError;
use crate::pdfium::{get_pdf_page_count, get_pdfium, render_pdf_page_to_png};

const DEFAULT_VISION_MODEL: &str = "google/gemini-2.5-flash";
pub const OPENROUTER_VISION_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

const OCR_PROMPT: &str = r"Analyze this document image and extract all text.

Format the output as markdown:
- Use proper headings (# ## ###) for document structure
- Format tables using markdown table syntax
- Use lists where appropriate
- Preserve paragraph breaks

Be thorough and accurate. Extract all visible text exactly as it appears.";

/// Vision OCR provider using an OpenRouter / OpenAI-compatible vision endpoint.
#[derive(Debug, Clone)]
pub struct OpenRouterVisionProvider {
    model: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl OpenRouterVisionProvider {
    /// Create a new vision OCR provider.
    pub fn new(
        api_key: impl Into<String>,
        model: Option<String>,
        base_url: Option<String>,
    ) -> Result<Self, ExtractError> {
        let key = api_key.into().trim().to_string();
        if key.is_empty() {
            return Err(ExtractError::Unsupported(
                "OpenRouterVisionProvider requires an API key — set OPENROUTER_API_KEY".to_string(),
            ));
        }

        let model = model.unwrap_or_else(|| DEFAULT_VISION_MODEL.to_string());
        let base_url = base_url.unwrap_or_else(|| OPENROUTER_VISION_URL.to_string());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3 * 60))
            .build()
            .map_err(|e| ExtractError::Container(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            model,
            base_url,
            api_key: key,
            client,
        })
    }

    /// Process a single image (png, jpg, webp, gif, heic) and extract markdown text.
    pub async fn process_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        filename: &str,
    ) -> Result<ExtractionResult, ExtractError> {
        let b64 = BASE64_STANDARD.encode(image_bytes);
        let data_url = format!("data:{mime_type};base64,{b64}");
        let raw_text = self.call_vision_api(&data_url).await?;
        let clean_text = clean_markdown(&raw_text);
        let content = format!("[Image: {filename}]\n\n{clean_text}");

        Ok(ExtractionResult {
            content,
            file_type: "image".to_string(),
            used_ocr: true,
            figures: Vec::new(),
            pages_blocks: None,
            page_map: None,
            error: None,
        })
    }

    /// Extract text from an image file.
    pub async fn extract_image_file(
        &self,
        filename: &str,
        mime_type: &str,
        image_bytes: &[u8],
    ) -> Result<ExtractionResult, ExtractError> {
        self.process_image(image_bytes, mime_type, filename).await
    }

    /// Process a scanned PDF by rendering pages to images with pdfium and running OCR.
    pub async fn process_scanned_pdf(
        &self,
        pdf_bytes: &[u8],
        filename: &str,
    ) -> Result<ExtractionResult, ExtractError> {
        let pdfium = get_pdfium()?;
        let page_count = get_pdf_page_count(pdfium, pdf_bytes)?;

        if page_count == 0 {
            return Err(ExtractError::Container(format!(
                "OCR failed for \"{filename}\": PDF contains 0 pages"
            )));
        }

        let mut page_texts = Vec::new();

        for p in 0..page_count {
            let png_bytes = render_pdf_page_to_png(pdfium, pdf_bytes, p)?;
            let b64 = BASE64_STANDARD.encode(&png_bytes);
            let data_url = format!("data:image/png;base64,{b64}");
            let text = self.call_vision_api(&data_url).await?;
            page_texts.push(clean_markdown(&text));
        }

        let combined = page_texts.join("\n\n---\n\n");

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

    async fn call_vision_api(&self, image_data_url: &str) -> Result<String, ExtractError> {
        let req = VisionChatRequest {
            model: self.model.clone(),
            messages: vec![VisionMessage {
                role: "user".to_string(),
                content: vec![
                    VisionContent::Text {
                        part_type: "text".to_string(),
                        text: OCR_PROMPT.to_string(),
                    },
                    VisionContent::ImageUrl {
                        part_type: "image_url".to_string(),
                        image_url: VisionImageUrl {
                            url: image_data_url.to_string(),
                        },
                    },
                ],
            }],
            max_tokens: 4000,
        };

        let res = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| ExtractError::Container(format!("vision OCR request failed: {e}")))?;

        if !res.status().is_success() {
            let body = res.text().await.unwrap_or_default();
            let preview = if body.len() > 300 {
                &body[..300]
            } else {
                &body
            };
            return Err(ExtractError::Container(format!(
                "vision OCR {} error: {preview}",
                self.model
            )));
        }

        let resp_json: VisionChatResponse = res
            .json()
            .await
            .map_err(|e| ExtractError::Container(format!("vision OCR invalid JSON: {e}")))?;

        let text = resp_json
            .choices
            .and_then(|mut c| c.pop())
            .and_then(|c| c.message)
            .and_then(|m| m.content)
            .unwrap_or_default();

        Ok(text)
    }
}

fn clean_markdown(raw: &str) -> String {
    let mut s = raw.trim();
    if s.starts_with("```markdown") {
        s = &s[11..];
    } else if s.starts_with("```md") {
        s = &s[5..];
    } else if s.starts_with("```") {
        s = &s[3..];
    }
    if s.ends_with("```") {
        s = &s[..s.len() - 3];
    }
    s.trim().to_string()
}

#[derive(Serialize)]
struct VisionChatRequest {
    model: String,
    messages: Vec<VisionMessage>,
    max_tokens: u32,
}

#[derive(Serialize)]
struct VisionMessage {
    role: String,
    content: Vec<VisionContent>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum VisionContent {
    Text {
        #[serde(rename = "type")]
        part_type: String,
        text: String,
    },
    ImageUrl {
        #[serde(rename = "type")]
        part_type: String,
        image_url: VisionImageUrl,
    },
}

#[derive(Serialize)]
struct VisionImageUrl {
    url: String,
}

#[derive(Deserialize)]
struct VisionChatResponse {
    choices: Option<Vec<VisionChoice>>,
}

#[derive(Deserialize)]
struct VisionChoice {
    message: Option<VisionMessageResponse>,
}

#[derive(Deserialize)]
struct VisionMessageResponse {
    content: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn clean_markdown_strips_enclosing_fences() {
        assert_eq!(
            clean_markdown("```markdown\n# Header\nProse text\n```"),
            "# Header\nProse text"
        );
        assert_eq!(
            clean_markdown("```\nSingle code block\n```"),
            "Single code block"
        );
        assert_eq!(clean_markdown("Plain text"), "Plain text");
    }

    #[test]
    fn requires_api_key() {
        assert!(OpenRouterVisionProvider::new("", None, None).is_err());
    }
}
