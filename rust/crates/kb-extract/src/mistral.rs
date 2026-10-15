//! Client for Mistral OCR API extractor.
//!
//! Mirrors `src/lib/rag/extractors/mistral-ocr-extractor.ts`.
//! Posts the PDF as a base64 data URL to `{base_url}/v1/ocr` and receives
//! per-page markdown, dimensions, and cropped figures. Inline image markdown
//! references `![id](id)` are parsed to construct reading-order blocks and captions.

use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use fancy_regex::Regex;
use kb_ports::{
    ExtractedFigure, ExtractionOptions, ExtractionResult, PageBlock, PageBlockKind, PageText,
    PortError,
};
use serde::{Deserialize, Serialize};

use crate::ExtractError;

static CAPTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(gambar|tabel|foto|diagram|grafik|bagan|ilustrasi|sumber)\b")
        .unwrap_or_else(|_| std::process::abort())
});

static IMG_MD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"!\[([^\]]*)\]\([^)]*\)")
        .unwrap_or_else(|_| std::process::abort())
});

/// Client for the Mistral OCR API.
#[derive(Debug, Clone)]
pub struct MistralOcrExtractor {
    base_url: String,
    token: String,
    model: String,
    client: reqwest::Client,
}

impl MistralOcrExtractor {
    /// Create a new Mistral OCR client.
    pub fn new(
        token: impl Into<String>,
        base_url: Option<String>,
        model: Option<String>,
        timeout: Option<Duration>,
    ) -> Result<Self, ExtractError> {
        let token = token.into().trim().to_string();
        if token.is_empty() {
            return Err(ExtractError::Unsupported(
                "MistralOcrExtractor requires KB_MISTRAL_OCR_KEY".to_string(),
            ));
        }

        let base_url = base_url
            .unwrap_or_else(|| "https://api.mistral.ai".to_string())
            .trim_end_matches('/')
            .to_string();
        let model = model.unwrap_or_else(|| "mistral-ocr-latest".to_string());
        let timeout = timeout.unwrap_or(Duration::from_secs(5 * 60)); // 5 minutes default
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ExtractError::Container(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            base_url,
            token,
            model,
            client,
        })
    }

    /// Extract text and figures from PDF bytes.
    pub async fn extract_pdf(
        &self,
        pdf_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, ExtractError> {
        let b64_pdf = BASE64_STANDARD.encode(pdf_bytes);
        let data_url = format!("data:application/pdf;base64,{b64_pdf}");

        let req = MistralOcrRequest {
            model: self.model.clone(),
            document: MistralDocUrl {
                doc_type: "document_url".to_string(),
                document_url: data_url,
            },
            include_image_base64: options.figures,
        };

        let res = self
            .client
            .post(format!("{}/v1/ocr", self.base_url))
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await
            .map_err(|e| ExtractError::Container(format!("mistral ocr request failed: {e}")))?;

        if !res.status().is_success() {
            let body = res.text().await.unwrap_or_default();
            let preview = if body.len() > 300 { &body[..300] } else { &body };
            return Err(ExtractError::Container(format!("mistral ocr error: {preview}")));
        }

        let resp_json: MistralOcrResponse = res
            .json()
            .await
            .map_err(|e| ExtractError::Container(format!("mistral ocr invalid JSON: {e}")))?;

        let pages = resp_json.pages.unwrap_or_default();

        let text = pages
            .iter()
            .filter_map(|p| p.markdown.as_deref())
            .filter(|m| !m.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

        let mut figures = Vec::new();
        let mut pages_blocks = Vec::new();
        let mut page_map = Vec::new();

        for p in &pages {
            let md = p.markdown.as_deref().unwrap_or_default();
            if !md.trim().is_empty() {
                page_map.push(PageText {
                    page: p.index,
                    text: md.to_string(),
                });
            }

            if options.figures {
                let w = p.dimensions.as_ref().and_then(|d| d.width).unwrap_or(0.0);
                let h = p.dimensions.as_ref().and_then(|d| d.height).unwrap_or(0.0);

                if let Some(imgs) = &p.images {
                    for img in imgs {
                        let b64_raw = match &img.image_base64 {
                            Some(s) if !s.is_empty() => s,
                            _ => continue,
                        };
                        let b64 = if let Some(pos) = b64_raw.find(',') {
                            &b64_raw[pos + 1..]
                        } else {
                            b64_raw
                        };

                        let image_bytes = match BASE64_STANDARD.decode(b64.as_bytes()) {
                            Ok(bytes) => bytes,
                            Err(_) => continue,
                        };

                        let bbox = if w > 0.0 && h > 0.0 {
                            [
                                img.top_left_x.unwrap_or(0.0) / w,
                                img.top_left_y.unwrap_or(0.0) / h,
                                img.bottom_right_x.unwrap_or(w) / w,
                                img.bottom_right_y.unwrap_or(h) / h,
                            ]
                        } else {
                            [0.0, 0.0, 1.0, 1.0]
                        };

                        let caption = caption_for_image(md, &img.id);
                        let kind = figure_type_from_caption(caption.as_deref());

                        figures.push(ExtractedFigure {
                            kind: kind.to_string(),
                            page: p.index,
                            bbox,
                            caption,
                            image_bytes,
                        });
                    }
                }

                pages_blocks.push(blocks_from_markdown(md, p.index));
            }
        }

        Ok(ExtractionResult {
            content: text,
            file_type: "pdf".to_string(),
            used_ocr: true,
            figures,
            pages_blocks: if options.figures {
                Some(pages_blocks)
            } else {
                None
            },
            page_map: if page_map.is_empty() {
                None
            } else {
                Some(page_map)
            },
            error: None,
        })
    }
}

#[async_trait::async_trait]
impl kb_ports::Extractor for MistralOcrExtractor {
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

/// Reading-order blocks for one page from Mistral's inline image markdown.
pub fn blocks_from_markdown(markdown: &str, page_index: u32) -> Vec<PageBlock> {
    let mut out = Vec::new();
    if markdown.trim().is_empty() {
        return out;
    }

    let mut last = 0usize;
    for m in IMG_MD_RE.find_iter(markdown) {
        let m = match m {
            Ok(m) => m,
            Err(_) => break,
        };
        let before = markdown[last..m.start()].trim();
        if !before.is_empty() {
            out.push(PageBlock {
                kind: PageBlockKind::Text,
                id: None,
                text: Some(before.to_string()),
            });
        }
        // Extract alt text from ![alt](url)
        let matched_text = m.as_str();
        let alt = if let Some(close_bracket) = matched_text.find(']') {
            &matched_text[2..close_bracket]
        } else {
            ""
        };
        out.push(PageBlock {
            kind: PageBlockKind::Figure,
            id: Some(format!("p{page_index}-{alt}")),
            text: None,
        });
        last = m.end();
    }

    let tail = markdown[last..].trim();
    if !tail.is_empty() {
        out.push(PageBlock {
            kind: PageBlockKind::Text,
            id: None,
            text: Some(tail.to_string()),
        });
    }

    out
}

fn clean_caption_line(l: &str) -> String {
    let mut s = l.trim();
    while s.starts_with('#') || s.starts_with('>') || s.starts_with('*') || s.starts_with('-') || s.starts_with(' ') {
        s = &s[1..].trim_start();
    }
    s.chars().take(200).collect()
}

fn caption_for_image(markdown: &str, image_id: &str) -> Option<String> {
    if markdown.is_empty() || image_id.is_empty() {
        return None;
    }
    let lines: Vec<&str> = markdown.lines().collect();
    let pattern = format!("]({image_id})");
    let ref_idx = lines
        .iter()
        .position(|l| l.contains(&pattern) || l.contains(image_id))?;

    let is_img = |l: &str| IMG_MD_RE.is_match(l).unwrap_or(false);

    // 1. Look for explicit caption-like pattern within window +/- 3 lines
    for i in (ref_idx.saturating_sub(3))..=(ref_idx + 3).min(lines.len().saturating_sub(1)) {
        if i == ref_idx {
            continue;
        }
        let raw = lines[i].trim();
        if raw.is_empty() || is_img(raw) {
            continue;
        }
        let clean = clean_caption_line(raw);
        if CAPTION_RE.is_match(&clean).unwrap_or(false) {
            return Some(clean);
        }
    }

    // 2. Nearest non-empty line (below preferred, then above)
    for dir in [1, -1] {
        let mut i = ref_idx as isize + dir;
        while i >= 0 && (i as usize) < lines.len() && (i - ref_idx as isize).abs() <= 3 {
            let raw = lines[i as usize].trim();
            if !raw.is_empty() && !is_img(raw) {
                let clean = clean_caption_line(raw);
                if clean.len() >= 3 {
                    return Some(clean);
                }
            }
            i += dir;
        }
    }

    None
}

fn figure_type_from_caption(caption: Option<&str>) -> &'static str {
    let cap = match caption {
        Some(c) => c.trim().to_lowercase(),
        None => return "image",
    };
    if cap.starts_with("tabel") {
        return "table";
    }
    if cap.starts_with("grafik")
        || cap.starts_with("diagram")
        || cap.starts_with("bagan")
        || cap.starts_with("chart")
        || cap.starts_with("kurva")
    {
        return "chart";
    }
    "image"
}

#[derive(Serialize)]
struct MistralOcrRequest {
    model: String,
    document: MistralDocUrl,
    include_image_base64: bool,
}

#[derive(Serialize)]
struct MistralDocUrl {
    #[serde(rename = "type")]
    doc_type: String,
    document_url: String,
}

#[derive(Deserialize)]
struct MistralOcrResponse {
    pages: Option<Vec<MistralPage>>,
}

#[derive(Deserialize)]
struct MistralPage {
    index: u32,
    markdown: Option<String>,
    images: Option<Vec<MistralImage>>,
    dimensions: Option<MistralDimensions>,
}

#[derive(Deserialize)]
struct MistralImage {
    id: String,
    top_left_x: Option<f32>,
    top_left_y: Option<f32>,
    bottom_right_x: Option<f32>,
    bottom_right_y: Option<f32>,
    image_base64: Option<String>,
}

#[derive(Deserialize)]
struct MistralDimensions {
    #[allow(dead_code)]
    dpi: Option<f32>,
    height: Option<f32>,
    width: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_from_markdown_with_image_tags() {
        let md = "Introduction paragraph.\n\n![img-0](img-0)\n\nConclusion text.";
        let blocks = blocks_from_markdown(md, 1);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].kind, PageBlockKind::Text);
        assert_eq!(blocks[0].text.as_deref(), Some("Introduction paragraph."));
        assert_eq!(blocks[1].kind, PageBlockKind::Figure);
        assert_eq!(blocks[1].id.as_deref(), Some("p1-img-0"));
        assert_eq!(blocks[2].kind, PageBlockKind::Text);
        assert_eq!(blocks[2].text.as_deref(), Some("Conclusion text."));
    }

    #[test]
    fn extracts_caption_from_nearby_line() {
        let md = "Some text\n![fig1](fig1)\nGambar 1.2 Struktur Sel\nNext paragraph";
        let cap = caption_for_image(md, "fig1");
        assert_eq!(cap.as_deref(), Some("Gambar 1.2 Struktur Sel"));
        assert_eq!(figure_type_from_caption(cap.as_deref()), "image");
    }

    #[test]
    fn identifies_table_and_chart_types() {
        assert_eq!(figure_type_from_caption(Some("Tabel 3.1 Data Nilai")), "table");
        assert_eq!(figure_type_from_caption(Some("Grafik Pertumbuhan Penduduk")), "chart");
    }
}
