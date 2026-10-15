//! Client for the hosted MinerU API extractor.
//!
//! Mirrors `src/lib/rag/extractors/mineru-api-extractor.ts`.
//! Requests an upload URL, uploads the PDF, polls until finished, downloads the
//! result zip, and parses markdown, page map, and cropped figures.

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::sync::LazyLock;
use std::time::Duration;

use fancy_regex::Regex;
use kb_ports::{
    ExtractedFigure, ExtractionOptions, ExtractionResult, PageText, PortError,
};
use serde::{Deserialize, Serialize};

use crate::ExtractError;

static CAPTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(gambar|figure|fig\.?|tabel|table|chart|grafik|diagram)\b")
        .unwrap_or_else(|_| std::process::abort())
});

/// Client for the hosted MinerU API extractor.
#[derive(Debug, Clone)]
pub struct MineruApiExtractor {
    base_url: String,
    token: String,
    language: String,
    timeout: Duration,
    poll_interval: Duration,
    client: reqwest::Client,
}

impl MineruApiExtractor {
    /// Create a new hosted MinerU API extractor.
    pub fn new(
        token: impl Into<String>,
        base_url: Option<String>,
        language: Option<String>,
        timeout: Option<Duration>,
    ) -> Result<Self, ExtractError> {
        let token = token.into().trim().to_string();
        if token.is_empty() {
            return Err(ExtractError::Unsupported(
                "MineruApiExtractor requires KB_MINERU_API_KEY".to_string(),
            ));
        }

        let base_url = base_url
            .unwrap_or_else(|| "https://mineru.net".to_string())
            .trim_end_matches('/')
            .to_string();
        let language = language.unwrap_or_else(|| "id".to_string());
        let timeout = timeout.unwrap_or(Duration::from_secs(10 * 60)); // 10 minutes default
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ExtractError::Container(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            base_url,
            token,
            language,
            timeout,
            poll_interval: Duration::from_secs(4),
            client,
        })
    }

    /// Set custom poll interval (useful in tests).
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Extract text and figures from PDF bytes.
    pub async fn extract_pdf(
        &self,
        pdf_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, ExtractError> {
        // 1. Request presigned upload URL
        let batch_req = BatchRequest {
            model_version: "vlm".to_string(),
            language: self.language.clone(),
            enable_table: true,
            enable_formula: true,
            files: vec![BatchFile {
                name: "document.pdf".to_string(),
            }],
        };

        let batch_res = self
            .client
            .post(format!("{}/api/v4/file-urls/batch", self.base_url))
            .bearer_auth(&self.token)
            .json(&batch_req)
            .send()
            .await
            .map_err(|e| ExtractError::Container(format!("mineru api batch request failed: {e}")))?;

        if !batch_res.status().is_success() {
            let body = batch_res.text().await.unwrap_or_default();
            return Err(ExtractError::Container(format!(
                "mineru api batch error: {body}"
            )));
        }

        let batch_json: BatchResponse = batch_res
            .json()
            .await
            .map_err(|e| ExtractError::Container(format!("mineru api invalid batch json: {e}")))?;

        if batch_json.code != 0 {
            return Err(ExtractError::Container(format!(
                "mineru api batch returned code {}: {}",
                batch_json.code,
                batch_json.msg.unwrap_or_default()
            )));
        }

        let data = batch_json.data.ok_or_else(|| {
            ExtractError::Container("mineru api batch missing data object".to_string())
        })?;
        let batch_id = data.batch_id;
        let file_url = data
            .file_urls
            .first()
            .ok_or_else(|| ExtractError::Container("mineru api returned empty file_urls".to_string()))?;

        // 2. Upload file via PUT
        let put_res = self
            .client
            .put(file_url)
            .body(pdf_bytes.to_vec())
            .send()
            .await
            .map_err(|e| ExtractError::Container(format!("mineru api upload failed: {e}")))?;

        if !put_res.status().is_success() {
            return Err(ExtractError::Container(format!(
                "mineru api upload returned status {}",
                put_res.status()
            )));
        }

        // 3. Poll batch until done
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut zip_url = String::new();

        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(self.poll_interval).await;
            let poll_res = self
                .client
                .get(format!(
                    "{}/api/v4/extract-results/batch/{}",
                    self.base_url, batch_id
                ))
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|e| ExtractError::Container(format!("mineru api poll failed: {e}")))?;

            if !poll_res.status().is_success() {
                continue;
            }

            let poll_json: PollResponse = match poll_res.json().await {
                Ok(j) => j,
                Err(_) => continue,
            };

            if let Some(r) = poll_json
                .data
                .and_then(|d| d.extract_result)
                .and_then(|mut v| v.pop())
            {
                if r.state.as_deref() == Some("done") {
                    if let Some(url) = r.full_zip_url {
                        zip_url = url;
                        break;
                    }
                } else if r.state.as_deref() == Some("failed") {
                    return Err(ExtractError::Container(format!(
                        "mineru api extraction failed: {}",
                        r.err_msg.unwrap_or_else(|| "unknown error".to_string())
                    )));
                }
            }
        }

        if zip_url.is_empty() {
            return Err(ExtractError::Container(
                "mineru api timed out waiting for extraction".to_string(),
            ));
        }

        // 4. Download and unzip
        let zip_res = self
            .client
            .get(&zip_url)
            .send()
            .await
            .map_err(|e| ExtractError::Container(format!("mineru api zip download failed: {e}")))?;

        if !zip_res.status().is_success() {
            return Err(ExtractError::Container(format!(
                "mineru api zip download returned status {}",
                zip_res.status()
            )));
        }

        let zip_bytes = zip_res
            .bytes()
            .await
            .map_err(|e| ExtractError::Container(format!("mineru api zip read failed: {e}")))?;

        let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes.as_ref()))
            .map_err(|e| ExtractError::Container(format!("invalid mineru zip: {e}")))?;

        // Find .md file
        let mut md_text = String::new();
        let mut content_list_json: Option<Vec<ContentBlock>> = None;
        let mut image_entries: Vec<(String, Vec<u8>)> = Vec::new();

        for i in 0..archive.len() {
            let mut file = archive
                .by_index(i)
                .map_err(|e| ExtractError::Container(format!("zip entry read error: {e}")))?;
            let name = file.name().to_string();

            if name.ends_with(".md") && md_text.is_empty() {
                let mut buf = String::new();
                let _ = file.read_to_string(&mut buf);
                md_text = buf;
            } else if name.ends_with("content_list.json") {
                let mut buf = String::new();
                if file.read_to_string(&mut buf).is_ok() {
                    content_list_json = serde_json::from_str(&buf).ok();
                }
            } else if name.ends_with(".jpg") || name.ends_with(".png") || name.ends_with(".jpeg") {
                let mut buf = Vec::new();
                if file.read_to_end(&mut buf).is_ok() {
                    image_entries.push((name, buf));
                }
            }
        }

        let mut page_map = None;
        let mut figures = Vec::new();

        if let Some(blocks) = &content_list_json {
            let map: Vec<PageText> = blocks
                .iter()
                .filter_map(|b| {
                    let text = b.text.as_deref()?.trim();
                    if text.is_empty() {
                        None
                    } else {
                        Some(PageText {
                            page: b.page_idx.unwrap_or(0),
                            text: text.to_string(),
                        })
                    }
                })
                .collect();
            if !map.is_empty() {
                page_map = Some(map);
            }

            if options.figures {
                let figure_types: HashSet<&str> = ["image", "chart", "image_block"].into();
                for b in blocks {
                    let kind = match b.kind.as_deref() {
                        Some(k) if figure_types.contains(k) => k,
                        _ => continue,
                    };
                    let img_path = match b.img_path.as_deref() {
                        Some(p) => p,
                        None => continue,
                    };

                    let img_name = img_path.split('/').next_back().unwrap_or(img_path);
                    let image_bytes = image_entries
                        .iter()
                        .find(|(name, _)| name == img_path || name.ends_with(img_name))
                        .map(|(_, bytes)| bytes.clone());

                    let image_bytes = match image_bytes {
                        Some(b) => b,
                        None => continue,
                    };

                    let bbox = match b.bbox {
                        Some(c) => [c[0] / 1000.0, c[1] / 1000.0, c[2] / 1000.0, c[3] / 1000.0],
                        None => [0.0, 0.0, 1.0, 1.0],
                    };

                    let caption = b
                        .image_caption
                        .as_ref()
                        .map(|c| c.join(" ").trim().to_string())
                        .filter(|c| !c.is_empty())
                        .or_else(|| find_nearby_caption(blocks, b));

                    figures.push(ExtractedFigure {
                        kind: kind.to_string(),
                        page: b.page_idx.unwrap_or(0),
                        bbox,
                        caption,
                        image_bytes,
                    });
                }
            }
        }

        Ok(ExtractionResult {
            content: md_text,
            file_type: "pdf".to_string(),
            used_ocr: true,
            figures,
            pages_blocks: None,
            page_map,
            error: None,
        })
    }
}

#[async_trait::async_trait]
impl kb_ports::Extractor for MineruApiExtractor {
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

fn find_nearby_caption(blocks: &[ContentBlock], fig: &ContentBlock) -> Option<String> {
    let page = fig.page_idx.unwrap_or(0);
    let fig_bottom = fig.bbox.map_or(0.0, |b| b[3]);
    let mut best: Option<String> = None;
    let mut best_gap = f32::INFINITY;

    for b in blocks {
        if std::ptr::eq(b, fig) || b.page_idx.unwrap_or(0) != page {
            continue;
        }
        let text = match b.text.as_deref() {
            Some(t) if !t.trim().is_empty() => t.trim(),
            _ => continue,
        };
        let top = b.bbox.map_or(0.0, |bbox| bbox[1]);
        let gap = top - fig_bottom;
        if gap >= -30.0 && gap < best_gap && gap < 200.0 && CAPTION_RE.is_match(text).unwrap_or(false) {
            best_gap = gap;
            best = Some(text.to_string());
        }
    }

    best
}

#[derive(Serialize)]
struct BatchRequest {
    model_version: String,
    language: String,
    enable_table: bool,
    enable_formula: bool,
    files: Vec<BatchFile>,
}

#[derive(Serialize)]
struct BatchFile {
    name: String,
}

#[derive(Deserialize)]
struct BatchResponse {
    code: i32,
    msg: Option<String>,
    data: Option<BatchData>,
}

#[derive(Deserialize)]
struct BatchData {
    batch_id: String,
    file_urls: Vec<String>,
}

#[derive(Deserialize)]
struct PollResponse {
    #[allow(dead_code)]
    code: Option<i32>,
    data: Option<PollData>,
}

#[derive(Deserialize)]
struct PollData {
    extract_result: Option<Vec<PollResult>>,
}

#[derive(Deserialize)]
struct PollResult {
    state: Option<String>,
    full_zip_url: Option<String>,
    err_msg: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    img_path: Option<String>,
    page_idx: Option<u32>,
    bbox: Option<[f32; 4]>,
    image_caption: Option<Vec<String>>,
    #[allow(dead_code)]
    text_level: Option<i32>,
}
