//! Local page OCR over an OpenAI-compatible chat endpoint.
//!
//! PaddleOCR-VL-1.6 under vLLM (`vllm serve PaddlePaddle/PaddleOCR-VL-1.6
//! --trust-remote-code`) or llama.cpp (`llama-server` with the GGUF), and the
//! PP-OCRv6 sidecar the Lite tier runs, all answer `POST /v1/chat/completions`.
//! One page image goes in per request and the task is a prompt prefix —
//! `OCR:` for text, `Table Recognition:` for a table — with temperature 0.
//!
//! This is deliberately **not** [`crate::vision_llm::VisionLlmExtractor`]: that
//! sends a whole PDF as an OpenRouter `file` content part, which vLLM and
//! llama.cpp reject. Here each PDF page is rendered by pdfium
//! ([`render_pdf_page_range`]) at `KB_OCR_DPI` and sent as a base64 `image_url`,
//! exactly the shape the vLLM page embedder uses (`docs/kb-tiers.md`).
//!
//! Pages run with bounded concurrency (`KB_OCR_CONCURRENCY`) and keep their
//! order: page *n* of the answer is page *n* of the document, whatever order
//! the server answered in. The result carries `page_map` with one entry per
//! non-empty page and `content` equal to those pages joined by a blank line, so
//! `kb_core::chunk::content_is_pages` holds and every chunk is chunked page by
//! page with its true page number.
//!
//! A page that still fails after the retries fails the whole extraction rather
//! than producing a document with a silently missing page: the layout chain
//! then tries its next parser, and if none succeeds the job reports the reason.
//!
//! Spend: this adapter talks to a local model, so it sends no `Authorization`
//! header unless `KB_OCR_API_KEY` is set, and it never falls back to
//! `OPENROUTER_API_KEY`. Answers are cached by the composition root's
//! `kb_cache::CachedExtractor`, keyed by the extended
//! [`ExtractConfig::cache_fingerprint`](crate::ExtractConfig::cache_fingerprint),
//! so a repeat ingest of the same PDF pays no model call.

use std::time::Duration;

use base64::Engine as _;
use futures::stream::{self, StreamExt};
use kb_ports::{
    ExtractionResult, Extractor, PageBlock, PageBlockKind, PageImage, PageText, PortError,
};
use serde::Deserialize;
use serde_json::json;

use crate::ExtractError;
use crate::chain::ScannedPageOcr;
use crate::config::ExtractConfig;
use crate::detect::{SupportedFileType, detect_file_type};
use crate::page_render::render_pdf_page_range;
use crate::pdfium::{get_pdf_page_count, get_pdfium};

/// The model that serves Desk and Studio (`docs/kb-tiers.md`).
pub const DEFAULT_OCR_MODEL: &str = "PaddlePaddle/PaddleOCR-VL-1.6";
/// PaddleOCR-VL's task prefix for plain text recognition.
pub const DEFAULT_OCR_PROMPT: &str = "OCR:";
/// Rendering resolution. 150 dpi is the low end of what an OCR model wants: at
/// 96 the small print of a scanned page starts to blur, and the request only
/// carries one page.
pub const DEFAULT_OCR_DPI: u32 = 150;
/// One page request at a time in flight, on the shared machine's terms.
pub const DEFAULT_OCR_CONCURRENCY: u32 = 2;
/// A 150 dpi page through a model on CPU can take a minute; a GPU serves it in
/// seconds, so the timeout only fires on a stall.
pub const DEFAULT_OCR_TIMEOUT_MS: u64 = 120_000;
/// One page of dense text plus a table is a few thousand tokens.
pub const DEFAULT_OCR_MAX_TOKENS: u32 = 4096;

/// JPEG quality of the page images sent for OCR. Higher than the page arm's
/// display thumbnail: OCR reads glyph edges, not charts.
const OCR_JPEG_QUALITY: u8 = 85;
/// One initial attempt plus three retries on 429/5xx and connection errors.
const MAX_ATTEMPTS: u32 = 4;

/// Settings for [`PageOcrExtractor`]. The defaults are [`ExtractConfig`]'s.
#[derive(Debug, Clone)]
pub struct PageOcrSettings {
    /// Full URL of `/v1/chat/completions`, e.g. `http://127.0.0.1:8000/v1/chat/completions`.
    pub base_url: String,
    pub model: String,
    /// Sent as `Authorization: Bearer …` only when non-empty.
    pub api_key: Option<String>,
    /// Task prefix (`OCR:`).
    pub prompt: String,
    pub dpi: u32,
    pub concurrency: u32,
    pub request_timeout: Duration,
    pub max_tokens: u32,
}

impl Default for PageOcrSettings {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: DEFAULT_OCR_MODEL.to_string(),
            api_key: None,
            prompt: DEFAULT_OCR_PROMPT.to_string(),
            dpi: DEFAULT_OCR_DPI,
            concurrency: DEFAULT_OCR_CONCURRENCY,
            request_timeout: Duration::from_millis(DEFAULT_OCR_TIMEOUT_MS),
            max_tokens: DEFAULT_OCR_MAX_TOKENS,
        }
    }
}

impl PageOcrSettings {
    /// The settings a resolved [`ExtractConfig`] describes.
    #[must_use]
    pub fn from_extract_config(cfg: &ExtractConfig) -> Self {
        let key = cfg.ocr_api_key.trim();
        Self {
            base_url: cfg.ocr_base_url.trim().to_string(),
            model: cfg.ocr_model.clone(),
            api_key: (!key.is_empty()).then(|| key.to_string()),
            prompt: cfg.ocr_prompt.clone(),
            dpi: cfg.ocr_dpi,
            concurrency: cfg.ocr_concurrency,
            request_timeout: cfg.ocr_timeout,
            max_tokens: cfg.ocr_max_tokens,
        }
    }
}

/// OCR for scanned PDF pages and image uploads against a local model.
#[derive(Debug, Clone)]
pub struct PageOcrExtractor {
    client: reqwest::Client,
    settings: PageOcrSettings,
}

impl PageOcrExtractor {
    /// # Errors
    ///
    /// When `KB_OCR_BASE_URL` is empty (the adapter is off) or the HTTP client
    /// cannot be built.
    pub fn new(settings: PageOcrSettings) -> Result<Self, ExtractError> {
        if settings.base_url.trim().is_empty() {
            return Err(ExtractError::Unsupported(
                "PageOcrExtractor requires KB_OCR_BASE_URL — the full URL of the \
                 local /v1/chat/completions endpoint"
                    .to_string(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| ExtractError::Container(format!("local OCR HTTP client failed: {e}")))?;
        Ok(Self { client, settings })
    }

    /// The endpoint this adapter calls, for startup logs and `/health`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.settings.base_url
    }

    /// The model name this adapter asks for.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.settings.model
    }

    /// OCR every page of a PDF.
    ///
    /// # Errors
    ///
    /// When pdfium is missing, the PDF cannot be loaded, a page cannot be
    /// rendered, or a page still fails after its retries. The message names the
    /// page so the job's reason is actionable.
    pub async fn extract_pdf(
        &self,
        pdf_bytes: &[u8],
        filename: &str,
    ) -> Result<ExtractionResult, ExtractError> {
        let pdfium = get_pdfium()?;
        let page_count = get_pdf_page_count(pdfium, pdf_bytes)?;
        if page_count == 0 {
            return Err(ExtractError::Container(format!(
                "local OCR: \"{filename}\" contains 0 pages"
            )));
        }

        // Render and OCR in windows rather than rendering all pages up front: a
        // 900-page scan at 150 dpi is ~300 MB of JPEGs, and the pages after the
        // window are not needed yet. Rendering reloads the document once per
        // window, not once per page.
        let pages_per_window = page_count.min(self.settings.concurrency.max(1) * 4);
        let mut texts = Vec::with_capacity(page_count as usize);
        let mut first = 0u32;
        while first < page_count {
            let count = pages_per_window.min(page_count - first);
            let bytes = pdf_bytes.to_vec();
            let dpi = self.settings.dpi;
            let pages = tokio::task::spawn_blocking(move || {
                render_pdf_page_range(&bytes, first, count, dpi, OCR_JPEG_QUALITY)
            })
            .await
            .map_err(|e| ExtractError::Container(format!("local OCR render task: {e}")))??;
            texts.extend(
                self.ocr_pages(&pages, filename, first as usize, page_count as usize)
                    .await?,
            );
            first += count;
        }

        Ok(paged_result(texts, "pdf"))
    }

    /// OCR one image upload (`png`, `jpg`, `tif`): a document of one page.
    ///
    /// # Errors
    ///
    /// When the request still fails after its retries or the answer carries no
    /// text field.
    pub async fn extract_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        filename: &str,
    ) -> Result<ExtractionResult, ExtractError> {
        if image_bytes.is_empty() {
            return Err(ExtractError::Container(format!(
                "local OCR: \"{filename}\" is empty"
            )));
        }
        let page = PageImage {
            page: 0,
            mime: if mime_type.is_empty() {
                "image/png".to_string()
            } else {
                mime_type.to_string()
            },
            bytes: image_bytes.to_vec(),
            text: String::new(),
        };
        let text = self
            .ocr_one(&page, 0)
            .await
            .map_err(|e| ExtractError::Container(format!("page 1 of 1: {e}")))?;
        Ok(paged_result(vec![text], "image"))
    }

    /// OCR rendered pages with bounded concurrency, in page order.
    ///
    /// `first_page` is the 0-based number of `pages[0]` in the document and
    /// `document_pages` its total, so an error names the page the operator sees
    /// in a PDF viewer.
    async fn ocr_pages(
        &self,
        pages: &[PageImage],
        filename: &str,
        first_page: usize,
        document_pages: usize,
    ) -> Result<Vec<String>, ExtractError> {
        let concurrency = self.settings.concurrency.max(1) as usize;
        let results: Vec<Result<String, ExtractError>> = stream::iter(0..pages.len())
            .map(|index| async move {
                let page_number = first_page + index + 1;
                let page = &pages[index];
                if page.bytes.is_empty() {
                    return Err(ExtractError::Container(format!(
                        "page {page_number} of {document_pages} could not be rendered"
                    )));
                }
                self.ocr_one(page, page_number).await.map_err(|e| {
                    ExtractError::Container(format!(
                        "page {page_number} of {document_pages} (\"{filename}\"): {e}"
                    ))
                })
            })
            // `buffered` polls up to `concurrency` futures at once and yields
            // their results in input order, so page n stays page n.
            .buffered(concurrency)
            .collect()
            .await;

        let mut texts = Vec::with_capacity(results.len());
        for result in results {
            texts.push(result?);
        }
        Ok(texts)
    }

    /// One page: one chat request, retried on 429/5xx and connection errors.
    async fn ocr_one(&self, page: &PageImage, page_index: usize) -> Result<String, ExtractError> {
        let body = serde_json::to_vec(&self.request_body(page))
            .map_err(|e| ExtractError::Container(format!("local OCR request body: {e}")))?;
        let mut last = String::new();

        for attempt in 1..=MAX_ATTEMPTS {
            let mut request = self
                .client
                .post(&self.settings.base_url)
                .header("content-type", "application/json")
                .body(body.clone());
            if let Some(key) = self.settings.api_key.as_deref() {
                request = request.bearer_auth(key);
            }

            let response = match request.send().await {
                Ok(response) => response,
                Err(e) => {
                    last = format!("request failed: {e}");
                    if attempt < MAX_ATTEMPTS {
                        backoff(attempt).await;
                        continue;
                    }
                    break;
                }
            };

            let status = response.status().as_u16();
            let bytes = match response.bytes().await {
                Ok(bytes) => bytes,
                Err(e) => {
                    last = format!("response body: {e}");
                    if attempt < MAX_ATTEMPTS {
                        backoff(attempt).await;
                        continue;
                    }
                    break;
                }
            };

            if !(200..300).contains(&status) {
                let message: String = String::from_utf8_lossy(&bytes).chars().take(300).collect();
                // 429 and 5xx are the server saying "later"; anything else is
                // our request being wrong and a retry only wastes the page.
                if (status == 429 || status >= 500) && attempt < MAX_ATTEMPTS {
                    tracing::warn!(
                        page = page_index + 1,
                        attempt,
                        status,
                        "local OCR transient error; backing off"
                    );
                    last = format!("http {status}: {message}");
                    backoff(attempt).await;
                    continue;
                }
                return Err(ExtractError::Container(format!(
                    "local OCR http {status}: {message}"
                )));
            }

            return parse_content(&bytes);
        }

        Err(ExtractError::Container(format!(
            "local OCR failed after {MAX_ATTEMPTS} attempts: {last}"
        )))
    }

    /// The OpenAI chat body: the image part first, then the task prefix.
    ///
    /// PaddleOCR-VL reads the image before the prompt; a text part before the
    /// image makes some builds answer in prose instead of doing the task.
    fn request_body(&self, page: &PageImage) -> serde_json::Value {
        let image = base64::engine::general_purpose::STANDARD.encode(&page.bytes);
        let data_url = format!("data:{};base64,{image}", page.mime);
        json!({
            "model": self.settings.model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": data_url}},
                    {"type": "text", "text": self.settings.prompt},
                ],
            }],
            "temperature": 0,
            "max_tokens": self.settings.max_tokens,
        })
    }
}

/// Exponential backoff: 200 ms, 400 ms, 800 ms, capped at 2 s.
async fn backoff(attempt: u32) {
    let millis = 200u64.saturating_mul(1 << attempt.saturating_sub(1).min(4));
    tokio::time::sleep(Duration::from_millis(millis.min(2_000))).await;
}

fn parse_content(bytes: &[u8]) -> Result<String, ExtractError> {
    let response: ChatResponse = serde_json::from_slice(bytes)
        .map_err(|e| ExtractError::Container(format!("local OCR invalid JSON: {e}")))?;
    response
        .choices
        .and_then(|mut choices| choices.pop())
        .and_then(|choice| choice.message)
        .and_then(|message| message.content)
        .ok_or_else(|| ExtractError::Container("local OCR response had no message content".into()))
}

/// `content` is the non-empty pages joined by a blank line, which is exactly
/// the shape `kb_core::chunk::content_is_pages` recognises: each page is
/// chunked on its own and every chunk carries its true page.
fn paged_result(texts: Vec<String>, file_type: &str) -> ExtractionResult {
    let mut content_pages = Vec::new();
    let mut page_map = Vec::new();
    let mut pages_blocks = Vec::new();

    for (index, raw) in texts.into_iter().enumerate() {
        let text = raw.trim().to_string();
        #[allow(clippy::cast_possible_truncation)]
        let page = index as u32;
        if text.is_empty() {
            pages_blocks.push(Vec::new());
            continue;
        }
        content_pages.push(text.clone());
        page_map.push(PageText {
            page,
            text: text.clone(),
        });
        pages_blocks.push(vec![PageBlock {
            kind: PageBlockKind::Text,
            id: None,
            text: Some(text),
        }]);
    }

    ExtractionResult {
        content: content_pages.join("\n\n"),
        file_type: file_type.to_string(),
        used_ocr: true,
        figures: Vec::new(),
        pages_blocks: Some(pages_blocks),
        page_map: if page_map.is_empty() {
            None
        } else {
            Some(page_map)
        },
        error: None,
    }
}

#[async_trait::async_trait]
impl Extractor for PageOcrExtractor {
    async fn extract_with_options(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
        _options: kb_ports::ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        match detect_file_type(filename) {
            Some(SupportedFileType::Pdf) => self
                .extract_pdf(file_bytes, filename)
                .await
                .map_err(PortError::from),
            Some(SupportedFileType::Image) => {
                let mime = if mime_type.is_empty() {
                    image_mime(filename).unwrap_or("image/png")
                } else {
                    mime_type
                };
                self.extract_image(file_bytes, mime, filename)
                    .await
                    .map_err(PortError::from)
            }
            // `detect_file_type` accepts the uploader's image list; `.tif` is
            // not on it, but the adapter can still read one when it is handed
            // straight to it (a scanner export, a test).
            _ => match image_mime(filename) {
                Some(mime) => self
                    .extract_image(file_bytes, mime, filename)
                    .await
                    .map_err(PortError::from),
                None => Err(PortError::from(ExtractError::Unsupported(format!(
                    "PageOcrExtractor reads PDFs and images, not \"{filename}\""
                )))),
            },
        }
    }
}

#[async_trait::async_trait]
impl ScannedPageOcr for PageOcrExtractor {
    async fn ocr_scanned_pdf(
        &self,
        pdf_bytes: &[u8],
        filename: &str,
    ) -> Result<ExtractionResult, ExtractError> {
        self.extract_pdf(pdf_bytes, filename).await
    }

    async fn ocr_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        filename: &str,
    ) -> Result<ExtractionResult, ExtractError> {
        self.extract_image(image_bytes, mime_type, filename).await
    }
}

/// The mime implied by an image file name, including `.tif`, which the
/// uploader's extension list does not accept.
fn image_mime(filename: &str) -> Option<&'static str> {
    let lower = filename.to_ascii_lowercase();
    let mime = if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".tif") || lower.ends_with(".tiff") {
        "image/tiff"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".bmp") {
        "image/bmp"
    } else if lower.ends_with(".heic") {
        "image/heic"
    } else {
        return None;
    };
    Some(mime)
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Option<Vec<ChatChoice>>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatMessage>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::{Json, Router, routing::post};

    use super::*;

    /// What the mock saw: every request body and its headers, in arrival order.
    #[derive(Default)]
    struct Seen {
        bodies: Vec<serde_json::Value>,
        authorization: Vec<Option<String>>,
    }

    type Shared = Arc<Mutex<Seen>>;

    /// The mock's state: what it has seen, how many calls it has answered, and
    /// how it should fail the first `fail_first` of them.
    #[derive(Clone)]
    struct MockState {
        seen: Shared,
        calls: Arc<Mutex<usize>>,
        fail_first: usize,
        fail_status: u16,
    }

    impl MockState {
        /// Record the request and return this call's number. Synchronous on
        /// purpose: no `MutexGuard` may live across the handler's await.
        fn record(&self, headers: &HeaderMap, body: &serde_json::Value) -> usize {
            let mut seen = self.seen.lock().unwrap();
            seen.authorization.push(
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
            );
            seen.bodies.push(body.clone());
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        }
    }

    /// A mock `/v1/chat/completions` that answers `PAGE-<n>` per distinct image
    /// and fails the first `fail_first` requests with `fail_status`.
    async fn mock(fail_first: usize, fail_status: u16) -> (String, Shared, Arc<Mutex<usize>>) {
        async fn handle(
            State(state): State<MockState>,
            headers: HeaderMap,
            Json(body): Json<serde_json::Value>,
        ) -> (axum::http::StatusCode, Json<serde_json::Value>) {
            let this_call = state.record(&headers, &body);

            if this_call <= state.fail_first {
                return (
                    axum::http::StatusCode::from_u16(state.fail_status).unwrap(),
                    Json(json!({"error": "nope"})),
                );
            }

            // The page's identity is its data URL: every rendered page differs.
            let url = body
                .pointer("/messages/0/content/0/image_url/url")
                .and_then(|u| u.as_str())
                .unwrap_or_default();
            let page = page_tag(url);
            // Answer page 0 last, so a test that keeps page order despite
            // out-of-order answers fails if the code sorts by completion
            // instead of by page.
            let delay = if page == 0 { 80 } else { 5 };
            tokio::time::sleep(Duration::from_millis(delay)).await;
            (
                axum::http::StatusCode::OK,
                Json(json!({
                    "choices": [{"message": {"role": "assistant", "content": format!("PAGE-{page}")}}]
                })),
            )
        }

        let seen: Shared = Arc::default();
        let calls: Arc<Mutex<usize>> = Arc::default();
        let app = Router::new()
            .route("/v1/chat/completions", post(handle))
            .with_state(MockState {
                seen: Arc::clone(&seen),
                calls: Arc::clone(&calls),
                fail_first,
                fail_status,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/v1/chat/completions"), seen, calls)
    }

    /// A page image whose bytes end with `n`, so the data URL differs per page
    /// and the mock can tell which page it is answering.
    fn page_image(n: u32) -> PageImage {
        PageImage {
            page: n,
            mime: "image/jpeg".to_string(),
            bytes: vec![0xFF, 0xD8, n as u8],
            text: String::new(),
        }
    }

    fn page_tag(url: &str) -> u32 {
        // `page_image` ends its bytes with the page number, so decoding the
        // data URL and reading the last byte says which page was sent.
        let payload = url.rsplit(',').next().unwrap_or_default();
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .ok()
            .and_then(|bytes| bytes.last().copied())
            .map_or(0, u32::from)
    }

    fn settings(url: String) -> PageOcrSettings {
        PageOcrSettings {
            base_url: url,
            concurrency: 2,
            request_timeout: Duration::from_secs(5),
            ..PageOcrSettings::default()
        }
    }

    #[tokio::test]
    async fn sends_the_image_first_then_the_task_prefix_at_temperature_zero() {
        let (url, seen, _calls) = mock(0, 500).await;
        let extractor = PageOcrExtractor::new(settings(url)).unwrap();
        let texts = extractor
            .ocr_pages(&[page_image(0)], "scan.pdf", 0, 1)
            .await
            .unwrap();
        assert_eq!(texts, vec!["PAGE-0".to_string()]);

        let seen = seen.lock().unwrap();
        let body = &seen.bodies[0];
        assert_eq!(body["model"], DEFAULT_OCR_MODEL);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["max_tokens"], DEFAULT_OCR_MAX_TOKENS);
        let content = body
            .pointer("/messages/0/content")
            .and_then(|c| c.as_array())
            .expect("content parts");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "image_url");
        assert!(
            content[0]
                .pointer("/image_url/url")
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .starts_with("data:image/jpeg;base64,")
        );
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "OCR:");
        assert_eq!(
            seen.authorization[0], None,
            "a local endpoint gets no Authorization header"
        );
    }

    #[tokio::test]
    async fn pages_keep_document_order_when_answers_arrive_out_of_order() {
        let (url, _seen, _calls) = mock(0, 500).await;
        let extractor = PageOcrExtractor::new(settings(url)).unwrap();
        let pages: Vec<PageImage> = (0..4).map(page_image).collect();
        let texts = extractor
            .ocr_pages(&pages, "scan.pdf", 0, pages.len())
            .await
            .unwrap();
        assert_eq!(
            texts,
            vec![
                "PAGE-0".to_string(),
                "PAGE-1".to_string(),
                "PAGE-2".to_string(),
                "PAGE-3".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn the_authorization_header_is_sent_only_when_a_key_is_set() {
        let (url, seen, _calls) = mock(0, 500).await;
        let extractor = PageOcrExtractor::new(PageOcrSettings {
            api_key: Some("local-key".to_string()),
            ..settings(url)
        })
        .unwrap();
        extractor
            .ocr_pages(&[page_image(0)], "scan.pdf", 0, 1)
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.authorization[0].as_deref(),
            Some("Bearer local-key"),
            "an explicit KB_OCR_API_KEY is sent (a local vLLM --api-key works)"
        );
    }

    #[tokio::test]
    async fn retries_a_503_then_succeeds() {
        let (url, _seen, calls) = mock(1, 503).await;
        let extractor = PageOcrExtractor::new(settings(url)).unwrap();
        let texts = extractor
            .ocr_pages(&[page_image(0)], "scan.pdf", 0, 1)
            .await
            .unwrap();
        assert_eq!(texts, vec!["PAGE-0".to_string()]);
        assert_eq!(*calls.lock().unwrap(), 2, "one retry after the 503");
    }

    #[tokio::test]
    async fn a_page_that_keeps_failing_fails_the_extraction_with_its_number() {
        let (url, _seen, _calls) = mock(usize::MAX, 500).await;
        let extractor = PageOcrExtractor::new(settings(url)).unwrap();
        let err = extractor
            .ocr_pages(&[page_image(0), page_image(1)], "scan.pdf", 0, 2)
            .await
            .expect_err("a lost page must fail the document");
        let message = err.to_string();
        assert!(message.contains("page 1 of 2"), "{message}");
        assert!(message.contains("scan.pdf"), "{message}");
        assert!(message.contains("local OCR"), "{message}");
    }

    #[tokio::test]
    async fn a_4xx_is_not_retried() {
        let (url, _seen, calls) = mock(usize::MAX, 400).await;
        let extractor = PageOcrExtractor::new(settings(url)).unwrap();
        assert!(
            extractor
                .ocr_pages(&[page_image(0)], "scan.pdf", 0, 1)
                .await
                .is_err()
        );
        assert_eq!(*calls.lock().unwrap(), 1, "a bad request is not repeated");
    }

    #[tokio::test]
    async fn an_image_upload_is_one_page() {
        let (url, seen, _calls) = mock(0, 500).await;
        let extractor = PageOcrExtractor::new(settings(url)).unwrap();
        let out = extractor
            .extract_image(b"\x89PNG\r\n\x1a\n", "image/png", "scan.png")
            .await
            .unwrap();
        assert!(out.content.starts_with("PAGE-"), "{}", out.content);
        assert_eq!(out.file_type, "image");
        assert!(out.used_ocr);
        assert_eq!(out.page_map.as_ref().unwrap()[0].page, 0);
        let seen = seen.lock().unwrap();
        assert!(
            seen.bodies[0]
                .pointer("/messages/0/content/0/image_url/url")
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .starts_with("data:image/png;base64,")
        );
    }

    #[tokio::test]
    async fn pages_become_page_map_and_page_joined_content() {
        let (url, _seen, _calls) = mock(0, 500).await;
        let extractor = PageOcrExtractor::new(settings(url)).unwrap();
        let pages: Vec<PageImage> = (0..2).map(page_image).collect();
        let texts = extractor
            .ocr_pages(&pages, "scan.pdf", 0, pages.len())
            .await
            .unwrap();
        let result = paged_result(texts, "pdf");
        assert_eq!(result.content, "PAGE-0\n\nPAGE-1");
        let map = result.page_map.as_ref().unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!((map[0].page, map[1].page), (0, 1));
        assert_eq!(map[1].text, "PAGE-1");
        assert_eq!(result.pages_blocks.as_ref().unwrap().len(), 2);
        // The paged chunker only engages when content *is* the pages.
        let pairs: Vec<(u32, String)> = map.iter().map(|p| (p.page, p.text.clone())).collect();
        assert!(kb_core::chunk::content_is_pages(&result.content, &pairs));
    }

    #[test]
    fn an_empty_base_url_is_off_not_a_broken_adapter() {
        assert!(PageOcrExtractor::new(PageOcrSettings::default()).is_err());
    }

    #[tokio::test]
    async fn a_real_two_page_pdf_keeps_both_pages_and_their_numbers() {
        if crate::pdfium::get_pdfium().is_err() {
            eprintln!("libpdfium not found; skipping");
            return;
        }
        // The repository's only multi-page PDF fixture; nothing is downloaded.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kb-eval/fixtures/tiny/corpus/mission.pdf");
        let Ok(pdf) = std::fs::read(&path) else {
            eprintln!("{} not found; skipping", path.display());
            return;
        };
        let (url, _seen, _calls) = mock(0, 500).await;
        let extractor = PageOcrExtractor::new(PageOcrSettings {
            dpi: 72,
            ..settings(url)
        })
        .unwrap();
        let out = extractor.extract_pdf(&pdf, "mission.pdf").await.unwrap();

        let map = out.page_map.as_ref().expect("a page map");
        assert_eq!(map.len(), 2, "the fixture has two pages");
        assert_eq!((map[0].page, map[1].page), (0, 1), "0-based, in order");
        assert!(out.used_ocr);
        assert_eq!(out.file_type, "pdf");
        let pairs: Vec<(u32, String)> = map.iter().map(|p| (p.page, p.text.clone())).collect();
        assert!(
            kb_core::chunk::content_is_pages(&out.content, &pairs),
            "content must be exactly the pages so every chunk keeps its page"
        );
    }

    #[tokio::test]
    async fn a_page_that_could_not_be_rendered_is_an_error_not_a_blank_page() {
        // `render_pdf_page_range` returns empty bytes for a page it could not
        // draw; sending that would ask the model to OCR nothing.
        let pages = [PageImage {
            page: 0,
            mime: "image/jpeg".to_string(),
            bytes: Vec::new(),
            text: String::new(),
        }];
        let extractor = PageOcrExtractor::new(settings(
            "http://127.0.0.1:1/v1/chat/completions".to_string(),
        ))
        .unwrap();
        let err = extractor
            .ocr_pages(&pages, "scan.pdf", 0, 1)
            .await
            .expect_err("an unrendered page fails the document");
        assert!(err.to_string().contains("could not be rendered"), "{err}");
    }

    #[test]
    fn an_image_extension_supplies_a_missing_mime() {
        assert_eq!(image_mime("a.JPG"), Some("image/jpeg"));
        assert_eq!(image_mime("a.tif"), Some("image/tiff"));
        assert_eq!(image_mime("a.png"), Some("image/png"));
        assert_eq!(image_mime("a.pdf"), None);
        assert_eq!(image_mime("a.bin"), None);
    }

    #[tokio::test]
    async fn a_tif_handed_straight_to_the_adapter_is_one_page() {
        let (url, seen, _calls) = mock(0, 500).await;
        let extractor = PageOcrExtractor::new(settings(url)).unwrap();
        let out = extractor
            .extract_with_options(
                "scan.tif",
                "",
                b"II*\x00",
                kb_ports::ExtractionOptions::default(),
            )
            .await
            .unwrap();
        assert!(out.content.starts_with("PAGE-"), "{}", out.content);
        let seen = seen.lock().unwrap();
        assert!(
            seen.bodies[0]
                .pointer("/messages/0/content/0/image_url/url")
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .starts_with("data:image/tiff;base64,"),
            "the extension supplies the mime"
        );
    }

    #[tokio::test]
    async fn the_extractor_port_reads_an_image_upload() {
        let (url, _seen, _calls) = mock(0, 500).await;
        let extractor = PageOcrExtractor::new(settings(url)).unwrap();
        let out = extractor
            .extract_with_options(
                "scan.png",
                "",
                b"\x89PNG",
                kb_ports::ExtractionOptions::default(),
            )
            .await
            .unwrap();
        assert!(out.content.starts_with("PAGE-"));
    }
}
