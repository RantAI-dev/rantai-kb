//! Xiaomi-OCR-0 as an OCR provider: one page, the vendor's recipe.
//!
//! The model is a small vision-language model that reads a page and answers
//! with markdown: prose, LaTeX, and OTSL — a compact table markup — for
//! tables. The answer is not ready to index: it has to be postprocessed, and
//! the postprocessing is the vendor's, ported to `kb-core`
//! (`kb_core::xiaomi_ocr`) so that a page reads the same here as it does in
//! the vendor's own pipeline.
//!
//! The recipe, in the order the vendor runs it:
//!
//! 1. render the page at 150 dpi as a PNG, scaled so its longest edge is at
//!    most 2048 px — the model's input bound;
//! 2. ask the model for the whole page, with the document prompt **before**
//!    the image, at temperature 0, `top_p` 0.8 and thinking off;
//! 3. if the answer looks like a loop ([`analyze_repetition_default`]), ask
//!    again with a presence penalty of 1.5 and keep the second answer only if
//!    it is not a loop either;
//! 4. cut a truncated repeated tail, normalise the math delimiters and render
//!    the tables.
//!
//! **Layout fallback.** The whole-page answer sometimes misses a table the
//! page clearly has. When a layout detector is configured
//! (`KB_OCR_LAYOUT_URL`), the provider asks it where the page's regions are;
//! if the detector says the page has a table and the whole-page answer has
//! none, the page is read again region by region and the regions are merged
//! into one page of markdown. The rule is the one the reference pipeline used:
//! a page is only re-read when the two disagree about a table.
//!
//! A layout detector that fails does not fail the page: the whole-page answer
//! is kept and the page is reported as degraded (`metadata.ocr.layoutFailed`),
//! so an operator sees that the layout path did not run.
//!
//! **Ported from** `evaluation_pipeline/e2e_img2md.py` and
//! `evaluation_pipeline/twostage_img2md.py` of `xiaomi-research/Xiaomi-OCR-0`
//! (Apache-2.0). See `docs/model-licences.md`.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures::stream::{self, StreamExt};
use kb_core::xiaomi_ocr::document::{TableFormat, finalize_document_format};
use kb_core::xiaomi_ocr::region::{LayoutRegion, merge_page_to_markdown, region_is_kept};
use kb_core::xiaomi_ocr::repetition::analyze_repetition_default;
use kb_ports::{LayoutBox, LayoutDetector, PageLayout};
use serde::Deserialize;
use serde_json::json;

use crate::ExtractError;
use crate::config::{ExtractConfig, OcrTableFormat};
use crate::ocr_router::{OcrCallError, OcrPageAnswer, OcrPageProvider, OcrPageRead, OcrTransport};
use crate::page_render::PageRenderSpec;

/// The vendor's prompt for a whole page.
pub const DOCUMENT_PROMPT: &str = "Extract all information from the main body of the document \
image and represent it in markdown format, ignoring headers and footers. Tables should be \
expressed in OTSL format, formulas in the document should be represented using LATEX format, and \
the parsing should be organized according to the reading order.";

/// The vendor's prompt for a region of text.
pub const TEXT_PROMPT: &str = "Extract the text in the image.";

/// The vendor's prompt for a table region.
pub const TABLE_PROMPT: &str = "Parse the table in the image into OTSL.";

/// The vendor's prompt for a formula region.
pub const FORMULA_PROMPT: &str =
    "Identify the formula in the image and represent it using LATEX format.";

/// The resolution the vendor renders a page at.
pub const DEFAULT_XIAOMI_DPI: u32 = 150;
/// The model's input bound: the longest edge of the page image.
pub const XIAOMI_MAX_EDGE: u32 = 2048;
/// How many tokens a page may use.
pub const DEFAULT_XIAOMI_MAX_TOKENS: u32 = 12_288;
/// The presence penalty of the first attempt: pure greedy.
pub const PRIMARY_PRESENCE_PENALTY: f32 = 0.0;
/// The presence penalty of the repetition retry.
pub const RETRY_PRESENCE_PENALTY: f32 = 1.5;
/// How many regions of one page are read at the same time.
///
/// A page holds one slot of the OCR pool while it is read, so the regions of
/// that page are read by this many requests at once: the model serves them in
/// parallel, and the page finishes sooner than the regions read one by one.
pub const REGION_CONCURRENCY: usize = 8;

/// Settings for [`XiaomiOcrProvider`].
#[derive(Clone)]
pub struct XiaomiOcrSettings {
    /// Full URL of `/v1/chat/completions`.
    pub base_url: String,
    /// The served model name.
    pub model: String,
    /// Sent as `Authorization: Bearer …` only when non-empty.
    pub api_key: Option<String>,
    /// Resolution of the page images.
    pub dpi: u32,
    /// How long one request may take.
    pub request_timeout: Duration,
    /// How many tokens a page may use.
    pub max_tokens: u32,
    /// How a table is written into the page's markdown.
    pub table_format: OcrTableFormat,
    /// The layout detector, when one is configured.
    pub layout: Option<Arc<dyn LayoutDetector>>,
}

impl std::fmt::Debug for XiaomiOcrSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XiaomiOcrSettings")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("dpi", &self.dpi)
            .field("max_tokens", &self.max_tokens)
            .field("table_format", &self.table_format)
            .field("layout", &self.layout.is_some())
            .finish()
    }
}

impl Default for XiaomiOcrSettings {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            api_key: None,
            dpi: DEFAULT_XIAOMI_DPI,
            request_timeout: Duration::from_secs(600),
            max_tokens: DEFAULT_XIAOMI_MAX_TOKENS,
            table_format: OcrTableFormat::Markdown,
            layout: None,
        }
    }
}

impl XiaomiOcrSettings {
    /// The settings a resolved [`ExtractConfig`] describes.
    #[must_use]
    pub fn from_extract_config(cfg: &ExtractConfig) -> Self {
        let key = cfg.ocr_api_key.trim();
        Self {
            base_url: cfg.ocr_base_url.trim().to_string(),
            model: cfg.ocr_model.clone(),
            api_key: (!key.is_empty()).then(|| key.to_string()),
            dpi: cfg.ocr_dpi,
            request_timeout: cfg.ocr_timeout,
            max_tokens: cfg.ocr_max_tokens,
            table_format: cfg.ocr_table_format,
            layout: None,
        }
    }

    /// How a table is written, in the port's own words.
    fn table_format(&self) -> TableFormat {
        match self.table_format {
            OcrTableFormat::Markdown => TableFormat::Markdown,
            OcrTableFormat::Html => TableFormat::Html,
        }
    }
}

/// The Xiaomi-OCR-0 provider.
#[derive(Debug, Clone)]
pub struct XiaomiOcrProvider {
    client: reqwest::Client,
    settings: XiaomiOcrSettings,
    /// What else changes the page's answer, and so belongs in the cache key.
    cache_tag: String,
}

impl XiaomiOcrProvider {
    /// Build the provider.
    ///
    /// # Errors
    ///
    /// When `KB_OCR_BASE_URL` is empty (the adapter is off) or the HTTP client
    /// cannot be built.
    pub fn new(settings: XiaomiOcrSettings) -> Result<Self, ExtractError> {
        if settings.base_url.trim().is_empty() {
            return Err(ExtractError::Unsupported(
                "XiaomiOcrProvider requires KB_OCR_BASE_URL — the full URL of the \
                 local /v1/chat/completions endpoint"
                    .to_string(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| ExtractError::Container(format!("xiaomi OCR HTTP client failed: {e}")))?;
        let cache_tag = format!(
            "recipe=xiaomi;tables={};layout={}",
            settings.table_format.as_str(),
            if settings.layout.is_some() {
                "on"
            } else {
                "off"
            }
        );
        Ok(Self {
            client,
            settings,
            cache_tag,
        })
    }

    /// The endpoint this provider calls.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.settings.base_url
    }

    /// The model this provider asks for.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.settings.model
    }

    /// One chat request: the prompt, then the image.
    ///
    /// The prompt comes **before** the image: the vendor's builds answer in
    /// prose when the image comes first.
    async fn ask(
        &self,
        image_bytes: &[u8],
        mime: &str,
        prompt: &str,
        presence_penalty: f32,
    ) -> Result<Answer, OcrCallError> {
        let image = base64::engine::general_purpose::STANDARD.encode(image_bytes);
        let data_url = format!("data:{mime};base64,{image}");
        let body = json!({
            "model": self.settings.model,
            "temperature": 0.0,
            "top_p": 0.8,
            "presence_penalty": presence_penalty,
            "repetition_penalty": 1.0,
            "max_tokens": self.settings.max_tokens,
            "chat_template_kwargs": {"enable_thinking": false},
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {"url": data_url}},
                ],
            }],
        });
        let payload = serde_json::to_vec(&body)
            .map_err(|e| OcrCallError::permanent(format!("xiaomi OCR request body: {e}")))?;

        let mut request = self
            .client
            .post(&self.settings.base_url)
            .header("content-type", "application/json")
            .body(payload);
        if let Some(key) = self.settings.api_key.as_deref() {
            request = request.bearer_auth(key);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(e) => return Err(OcrCallError::transient(format!("request failed: {e}"))),
        };
        let status = response.status().as_u16();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return Err(OcrCallError::transient(format!("response body: {e}"))),
        };
        if !(200..300).contains(&status) {
            let message: String = String::from_utf8_lossy(&bytes).chars().take(300).collect();
            if status == 429 || status >= 500 {
                return Err(OcrCallError::transient(format!(
                    "xiaomi OCR http {status}: {message}"
                )));
            }
            return Err(OcrCallError::permanent(format!(
                "xiaomi OCR http {status}: {message}"
            )));
        }
        parse_answer(&bytes)
    }

    /// Ask for one image, and ask again with a presence penalty when the
    /// answer looks like a loop. The second answer is adopted only when it is
    /// not a loop either — the vendor's `--retry-repetitive`, which is the
    /// same rule for a whole page and for a cropped region.
    async fn ask_with_retry(
        &self,
        image_bytes: &[u8],
        mime: &str,
        prompt: &str,
    ) -> Result<String, OcrCallError> {
        let first = self
            .ask(image_bytes, mime, prompt, PRIMARY_PRESENCE_PENALTY)
            .await?;
        let mut raw = first.text;
        let analysis = analyze_repetition_default(&raw, first.finish_reason.as_deref());
        if analysis.suspicious {
            tracing::warn!(
                reasons = ?analysis.reasons,
                "xiaomi OCR answer looks repetitive; retrying with a presence penalty"
            );
            match self
                .ask(image_bytes, mime, prompt, RETRY_PRESENCE_PENALTY)
                .await
            {
                Ok(retry) => {
                    let retry_analysis =
                        analyze_repetition_default(&retry.text, retry.finish_reason.as_deref());
                    if !retry_analysis.suspicious && !retry.text.trim().is_empty() {
                        raw = retry.text;
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err.message, "xiaomi OCR retry failed; keeping the first answer");
                }
            }
        }
        Ok(raw)
    }

    /// Read a page whole, and read it again region by region when the layout
    /// says the page has a table the whole-page answer missed.
    async fn read_page(
        &self,
        image_bytes: &[u8],
        mime: &str,
    ) -> Result<OcrPageAnswer, OcrCallError> {
        let raw = self
            .ask_with_retry(image_bytes, mime, DOCUMENT_PROMPT)
            .await?;

        let page = finalize_document_format(&raw, self.settings.table_format());

        let Some(detector) = self.settings.layout.as_deref() else {
            return Ok(OcrPageAnswer::text(page));
        };

        // The layout detector is only asked when the whole-page answer has no
        // table: a page that already has one needs nothing from it.
        if answer_has_table(&raw) {
            return Ok(OcrPageAnswer::text(page));
        }

        let layout = match detector.detect(image_bytes, mime).await {
            Ok(layout) => layout,
            Err(err) => {
                // The page keeps the whole-page answer, and the report says the
                // layout path failed: a silent fallback would look like a page
                // with no regions.
                tracing::warn!(error = %err.to_string(), "xiaomi OCR layout detection failed");
                return Ok(OcrPageAnswer {
                    text: page,
                    layout: false,
                    layout_failed: true,
                });
            }
        };

        if !layout
            .regions
            .iter()
            .any(|region| region.task_type == "table")
        {
            return Ok(OcrPageAnswer {
                text: page,
                layout: true,
                layout_failed: false,
            });
        }

        match self.read_regions(image_bytes, &layout).await {
            Ok(text) if !text.trim().is_empty() => Ok(OcrPageAnswer {
                text,
                layout: true,
                layout_failed: false,
            }),
            Ok(_) => Ok(OcrPageAnswer {
                text: page,
                layout: true,
                layout_failed: false,
            }),
            Err(err) => {
                tracing::warn!(error = %err.message, "xiaomi OCR region read failed; keeping the whole-page answer");
                Ok(OcrPageAnswer {
                    text: page,
                    layout: true,
                    layout_failed: false,
                })
            }
        }
    }

    /// Read a page region by region and merge the regions into one page.
    async fn read_regions(
        &self,
        image_bytes: &[u8],
        layout: &PageLayout,
    ) -> Result<String, OcrCallError> {
        let wanted: Vec<(usize, &LayoutBox)> = layout
            .regions
            .iter()
            .enumerate()
            .filter(|(_, region)| region_is_kept(&region.label))
            .collect();
        if wanted.is_empty() {
            return Ok(String::new());
        }

        let bytes = image_bytes.to_vec();
        // The crop keeps the region's place in the **whole** layout, not in the
        // filtered list: a page's furniture (a header, a page number) is not
        // read, but the regions after it still have to land on their own
        // places, or the page is assembled from the wrong texts.
        let boxes: Vec<(usize, LayoutBox)> = wanted
            .iter()
            .map(|(index, region)| (*index, (*region).clone()))
            .collect();
        let crops = tokio::task::spawn_blocking(move || crop_regions(&bytes, &boxes))
            .await
            .map_err(|e| OcrCallError::transient(format!("region crop task: {e}")))?
            .map_err(|e| OcrCallError::permanent(format!("region crop: {e}")))?;

        let results: Vec<(usize, Result<String, OcrCallError>)> =
            stream::iter(crops.into_iter().map(|(index, crop, region)| async move {
                let prompt = prompt_for(&region);
                let answer = self.ask_with_retry(&crop, "image/png", prompt).await;
                (index, answer)
            }))
            .buffered(REGION_CONCURRENCY)
            .collect()
            .await;

        let mut merged: Vec<LayoutRegion> = layout
            .regions
            .iter()
            .enumerate()
            .map(|(index, region)| LayoutRegion {
                index,
                label: region.label.clone(),
                bbox: region.bbox_2d.to_vec(),
                content: String::new(),
            })
            .collect();

        for (index, result) in results {
            let Ok(text) = result else { continue };
            let Some(region) = merged.get_mut(index) else {
                continue;
            };
            tracing::debug!(
                index,
                label = %region.label,
                task_type = %layout.regions[index].task_type,
                chars = text.chars().count(),
                head = %text.chars().take(60).collect::<String>(),
                "xiaomi OCR region read"
            );
            // The vendor renders a table by its **task**, not by its label.
            region.content = if layout
                .regions
                .get(index)
                .is_some_and(|region| region.task_type == "table")
            {
                self.render_table(&text)
            } else {
                text
            };
        }

        Ok(merge_page_to_markdown(&merged))
    }

    /// Render an OTSL table the way the page's tables are rendered.
    fn render_table(&self, otsl: &str) -> String {
        match self.settings.table_format() {
            TableFormat::Html => kb_core::xiaomi_ocr::otsl::otsl_to_html(otsl),
            TableFormat::Markdown => kb_core::xiaomi_ocr::otsl::otsl_to_markdown(otsl),
        }
    }
}

#[async_trait::async_trait]
impl OcrPageProvider for XiaomiOcrProvider {
    fn id(&self) -> &str {
        "xiaomi"
    }

    fn model(&self) -> &str {
        &self.settings.model
    }

    fn prompt(&self) -> &str {
        DOCUMENT_PROMPT
    }

    fn dpi(&self) -> u32 {
        self.settings.dpi
    }

    fn image_spec(&self) -> PageRenderSpec {
        PageRenderSpec {
            dpi: self.settings.dpi,
            format: crate::page_render::PageImageFormat::Png,
            max_edge: XIAOMI_MAX_EDGE,
        }
    }

    fn cache_tag(&self) -> &str {
        &self.cache_tag
    }

    fn transport(&self) -> OcrTransport {
        OcrTransport::Image
    }

    async fn read_pdf(
        &self,
        _pdf_bytes: &[u8],
        _include_figures: bool,
    ) -> Result<Vec<OcrPageRead>, OcrCallError> {
        Err(OcrCallError::permanent(
            "the xiaomi recipe reads one page image at a time; it does not take a document",
        ))
    }

    async fn read_image(
        &self,
        image_bytes: &[u8],
        mime: &str,
    ) -> Result<OcrPageAnswer, OcrCallError> {
        self.read_page(image_bytes, mime).await
    }
}

/// The prompt a region is read with.
fn prompt_for(region: &LayoutBox) -> &'static str {
    match region.task_type.as_str() {
        "table" => TABLE_PROMPT,
        "formula" => FORMULA_PROMPT,
        _ => TEXT_PROMPT,
    }
}

/// Cut the regions out of the page image.
///
/// The boxes are normalised to 0–1000, so the crop follows the image the
/// detector saw. A region that is empty or outside the page is skipped. Each
/// crop carries the region's own place in the page's layout, so the answer can
/// be put back where it came from.
fn crop_regions(
    image_bytes: &[u8],
    regions: &[(usize, LayoutBox)],
) -> Result<Vec<(usize, Vec<u8>, LayoutBox)>, String> {
    let image = image::load_from_memory(image_bytes).map_err(|e| format!("decode page: {e}"))?;
    let (width, height) = (image.width(), image.height());
    let mut crops = Vec::new();
    for (index, region) in regions.iter() {
        let bbox = region.bbox_2d;
        let x1 = (bbox[0] / 1000.0 * f64::from(width)).max(0.0) as u32;
        let y1 = (bbox[1] / 1000.0 * f64::from(height)).max(0.0) as u32;
        let x2 = (bbox[2] / 1000.0 * f64::from(width)).max(0.0) as u32;
        let y2 = (bbox[3] / 1000.0 * f64::from(height)).max(0.0) as u32;
        let x1 = x1.min(width);
        let y1 = y1.min(height);
        let x2 = x2.min(width);
        let y2 = y2.min(height);
        if x2 <= x1 || y2 <= y1 {
            continue;
        }
        // `crop_imm` takes the origin and the **size** of the crop; the box's
        // bottom-right corner is a position, not a size. Reading it as a size
        // would cut a quarter of the page for every region, and the model
        // would answer with the page instead of the region.
        let cropped = image.crop_imm(x1, y1, x2 - x1, y2 - y1);
        let mut bytes = Vec::new();
        cropped
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .map_err(|e| format!("encode region: {e}"))?;
        crops.push((*index, bytes, region.clone()));
    }
    Ok(crops)
}

#[async_trait::async_trait]
impl kb_ports::Extractor for XiaomiOcrProvider {
    async fn extract_with_options(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
        _options: kb_ports::ExtractionOptions,
    ) -> Result<kb_ports::ExtractionResult, kb_ports::PortError> {
        use crate::chain::ScannedPageOcr as _;
        match crate::detect::detect_file_type(filename) {
            Some(crate::detect::SupportedFileType::Pdf) => self
                .ocr_scanned_pdf(file_bytes, filename)
                .await
                .map_err(kb_ports::PortError::from),
            _ => {
                let mime = if mime_type.is_empty() {
                    "image/png"
                } else {
                    mime_type
                };
                self.ocr_image(file_bytes, mime, filename)
                    .await
                    .map_err(kb_ports::PortError::from)
            }
        }
    }
}

/// How many pages are rendered at a time when a whole scanned PDF is read.
///
/// The pages are rendered, read and dropped one window at a time: a 900-page
/// scan at 150 dpi is hundreds of megabytes of PNG, and the pages after the
/// window are not needed yet.
const RENDER_WINDOW: u32 = 8;

#[async_trait::async_trait]
impl crate::chain::ScannedPageOcr for XiaomiOcrProvider {
    async fn ocr_scanned_pdf(
        &self,
        pdf_bytes: &[u8],
        filename: &str,
    ) -> Result<kb_ports::ExtractionResult, ExtractError> {
        let page_count =
            crate::pdfium::get_pdf_page_count(crate::pdfium::get_pdfium()?, pdf_bytes)?;
        if page_count == 0 {
            return Err(ExtractError::Container(format!(
                "xiaomi OCR: \"{filename}\" contains 0 pages"
            )));
        }
        let mut texts = Vec::with_capacity(page_count as usize);
        let mut first = 0u32;
        while first < page_count {
            let count = RENDER_WINDOW.min(page_count - first);
            let indices: Vec<u32> = (first..first + count).collect();
            let bytes = pdf_bytes.to_vec();
            let spec = self.image_spec();
            let pages = tokio::task::spawn_blocking(move || {
                crate::page_render::render_pdf_page_indices_spec(&bytes, &indices, spec)
            })
            .await
            .map_err(|e| ExtractError::Container(format!("xiaomi OCR render task: {e}")))??;
            for page in pages {
                let answer = self
                    .read_image(&page.bytes, &page.mime)
                    .await
                    .map_err(|e| ExtractError::Container(e.message))?;
                texts.push(answer.text);
            }
            first += count;
        }
        Ok(crate::page_ocr::paged_result(texts, "pdf"))
    }

    async fn ocr_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        filename: &str,
    ) -> Result<kb_ports::ExtractionResult, ExtractError> {
        if image_bytes.is_empty() {
            return Err(ExtractError::Container(format!(
                "xiaomi OCR: \"{filename}\" is empty"
            )));
        }
        let mime = if mime_type.is_empty() {
            "image/png"
        } else {
            mime_type
        };
        let answer = self
            .read_image(image_bytes, mime)
            .await
            .map_err(|e| ExtractError::Container(e.message))?;
        Ok(crate::page_ocr::paged_result(vec![answer.text], "image"))
    }
}

/// Whether a whole-page answer carries a table, decided the way the measured
/// pipeline decided it: on the HTML rendering of the answer, whatever
/// `KB_OCR_TABLE_FORMAT` the page is finally written in. Testing the final
/// text for `<table` missed every markdown table, so with the default format
/// each table page went to the layout detector and lost its whole-page table
/// to a region re-read.
fn answer_has_table(raw: &str) -> bool {
    finalize_document_format(raw, TableFormat::Html)
        .to_lowercase()
        .contains("<table")
}

/// One answer from the chat endpoint.
#[derive(Debug, Deserialize)]
struct Answer {
    text: String,
    finish_reason: Option<String>,
}
/// The chat endpoint's answer.
#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<String>,
}

/// Read the answer out of a chat response.
fn parse_answer(bytes: &[u8]) -> Result<Answer, OcrCallError> {
    let mut response: ChatResponse = serde_json::from_slice(bytes)
        .map_err(|e| OcrCallError::permanent(format!("xiaomi OCR invalid JSON: {e}")))?;
    let choice = response
        .choices
        .pop()
        .ok_or_else(|| OcrCallError::permanent("xiaomi OCR response had no choices"))?;
    let message = choice
        .message
        .ok_or_else(|| OcrCallError::permanent("xiaomi OCR response had no message"))?;
    let text = message
        .content
        .ok_or_else(|| OcrCallError::permanent("xiaomi OCR response had no content"))?;
    Ok(Answer {
        text: text.trim().to_string(),
        finish_reason: choice.finish_reason,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use kb_ports::PortError;

    /// A layout detector that answers with a fixed layout.
    struct FakeLayout {
        layout: PageLayout,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl LayoutDetector for FakeLayout {
        async fn detect(&self, _image: &[u8], _mime: &str) -> Result<PageLayout, PortError> {
            if self.fail {
                return Err(PortError::Unavailable("layout is down".to_string()));
            }
            Ok(self.layout.clone())
        }
    }

    fn settings() -> XiaomiOcrSettings {
        XiaomiOcrSettings {
            base_url: "http://127.0.0.1:8192/v1/chat/completions".to_string(),
            model: "xiaomi-ocr-0".to_string(),
            api_key: Some("kb-eval-vlm-2026".to_string()),
            ..XiaomiOcrSettings::default()
        }
    }

    #[test]
    fn the_provider_speaks_the_vendors_recipe() {
        let provider = XiaomiOcrProvider::new(settings()).expect("provider");
        assert_eq!(provider.id(), "xiaomi");
        assert_eq!(provider.model(), "xiaomi-ocr-0");
        assert_eq!(provider.dpi(), 150);
        assert_eq!(provider.prompt(), DOCUMENT_PROMPT);
        assert_eq!(provider.transport(), OcrTransport::Image);
        let spec = provider.image_spec();
        assert_eq!(spec.dpi, 150);
        assert_eq!(spec.max_edge, XIAOMI_MAX_EDGE);
        assert_eq!(spec.format, crate::page_render::PageImageFormat::Png);
    }

    #[test]
    fn the_cache_key_says_which_recipe_and_table_format_were_used() {
        let mut cfg = settings();
        cfg.table_format = OcrTableFormat::Html;
        let provider = XiaomiOcrProvider::new(cfg).expect("provider");
        assert_eq!(provider.cache_tag(), "recipe=xiaomi;tables=html;layout=off");

        let mut cfg = settings();
        cfg.layout = Some(Arc::new(FakeLayout {
            layout: PageLayout::default(),
            fail: false,
        }));
        let provider = XiaomiOcrProvider::new(cfg).expect("provider");
        assert_eq!(
            provider.cache_tag(),
            "recipe=xiaomi;tables=markdown;layout=on"
        );
    }

    #[test]
    fn a_table_is_seen_whichever_format_the_page_is_written_in() {
        let raw = "Intro\n\n<fcel>Milestone<fcel>Date<nl><fcel>Start<fcel>2001<nl>\n\nAfter";
        assert!(answer_has_table(raw));
        assert!(
            !finalize_document_format(raw, TableFormat::Markdown).contains("<table"),
            "the markdown page carries no <table> tag, so the test cannot look for one"
        );
        assert!(!answer_has_table("Just a paragraph of text."));
    }

    #[test]
    fn an_empty_endpoint_is_refused() {
        let mut cfg = settings();
        cfg.base_url = String::new();
        assert!(XiaomiOcrProvider::new(cfg).is_err());
    }

    #[test]
    fn a_region_is_read_with_the_prompt_of_its_kind() {
        let table = LayoutBox {
            label: "table".to_string(),
            score: 0.9,
            bbox_2d: [0.0, 0.0, 100.0, 100.0],
            task_type: "table".to_string(),
        };
        let formula = LayoutBox {
            task_type: "formula".to_string(),
            ..table.clone()
        };
        let text = LayoutBox {
            task_type: "text".to_string(),
            ..table.clone()
        };
        assert_eq!(prompt_for(&table), TABLE_PROMPT);
        assert_eq!(prompt_for(&formula), FORMULA_PROMPT);
        assert_eq!(prompt_for(&text), TEXT_PROMPT);
    }

    #[test]
    fn a_region_outside_the_page_is_not_cut() {
        // The vendor hands an empty region a one-pixel crop and still asks the
        // model about it. We skip it instead: a blank pixel costs a model call
        // and can only produce noise. No region in the 1403-page corpus is
        // empty, so this is a corner, not a behaviour change.
        let image = image::DynamicImage::new_rgb8(100, 50);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("png");
        let regions = vec![
            (
                0usize,
                LayoutBox {
                    label: "text".to_string(),
                    score: 0.5,
                    bbox_2d: [0.0, 0.0, 500.0, 500.0],
                    task_type: "text".to_string(),
                },
            ),
            (
                1usize,
                LayoutBox {
                    label: "text".to_string(),
                    score: 0.5,
                    bbox_2d: [1000.0, 1000.0, 1100.0, 1100.0],
                    task_type: "text".to_string(),
                },
            ),
        ];
        let crops = crop_regions(&bytes, &regions).expect("crops");
        assert_eq!(crops.len(), 1, "the region outside the page is skipped");
        assert_eq!(
            crops[0].0, 0,
            "a crop keeps its place in the page's whole layout"
        );
    }

    #[test]
    fn a_crop_is_the_size_of_the_region_and_not_of_the_page() {
        // The bug this pins: `crop_imm` takes the origin and the size, so
        // handing it the box's bottom-right corner cuts a quarter of the page
        // for every region — and the model answers with the page, over and
        // over, instead of with the region.
        let image = image::DynamicImage::new_rgb8(100, 50);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("png");
        let regions = vec![(
            0usize,
            LayoutBox {
                label: "text".to_string(),
                score: 0.5,
                bbox_2d: [200.0, 400.0, 600.0, 800.0],
                task_type: "text".to_string(),
            },
        )];
        let crops = crop_regions(&bytes, &regions).expect("crops");
        assert_eq!(crops.len(), 1);
        let crop = image::load_from_memory(&crops[0].1).expect("crop");
        assert_eq!(
            (crop.width(), crop.height()),
            (40, 20),
            "the crop is the region's own box: 20..60 by 20..40"
        );
    }

    #[test]
    fn a_crop_keeps_the_index_of_the_region_it_came_from() {
        // The bug this pins: the regions that are read are a subset of the
        // page's regions, and each answer has to land on its own region. An
        // index relative to the subset puts a page's text in its header.
        let image = image::DynamicImage::new_rgb8(100, 50);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("png");
        let box_of = |x: f64| LayoutBox {
            label: "text".to_string(),
            score: 0.5,
            bbox_2d: [x, 0.0, x + 100.0, 100.0],
            task_type: "text".to_string(),
        };
        // Region 1 is the header (not read), region 3 is body text.
        let regions = vec![(1usize, box_of(0.0)), (3usize, box_of(100.0))];
        let crops = crop_regions(&bytes, &regions).expect("crops");
        let indices: Vec<usize> = crops.iter().map(|(index, _, _)| *index).collect();
        assert_eq!(indices, vec![1, 3]);
    }
}
