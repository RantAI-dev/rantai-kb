//! Extraction pipeline configuration.
//!
//! Mirrors `src/lib/rag/config.ts` and `src/lib/ocr/config.ts`.
//! Loads extractor endpoints, model IDs, timeouts, and API keys from environment
//! variables with identical names and defaults.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How PDF pages reach OCR (`KB_OCR_MODE`, ING-6).
///
/// `Auto` is the default: the effective mode is `Hybrid` when an OCR provider
/// is configured and `Off` otherwise, so an installation with no OCR endpoint
/// behaves exactly as it did before the router existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OcrMode {
    /// No page goes to OCR; the local text layer is used and reported.
    Off,
    /// `Auto` sentinel: `Hybrid` when a provider is configured, else `Off`.
    #[default]
    Auto,
    /// Per page: a scan goes to OCR, a text page keeps its local text.
    Hybrid,
    /// Every PDF page goes to OCR; local text is that page's fallback.
    Full,
}

impl OcrMode {
    /// Parse the `KB_OCR_MODE` value. Unknown text is a configuration error,
    /// not a silent fallback: an operator who typed `ful` should hear about it.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "hybrid" => Some(Self::Hybrid),
            "full" => Some(Self::Full),
            "auto" | "" => Some(Self::Auto),
            _ => None,
        }
    }

    /// The name written into `metadata.ocr.mode`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Hybrid | Self::Auto => "hybrid",
            Self::Full => "full",
        }
    }
}

/// Which OCR recipe the local chat endpoint is served with.
///
/// The two recipes are different shapes, not different prompts: PaddleOCR-VL
/// answers one page with the task prefix (`OCR:`) and no postprocessing,
/// while Xiaomi-OCR-0 answers with markdown plus OTSL table markup that has to
/// be turned into text before it is worth indexing. Which one is in use is a
/// property of the model behind `KB_OCR_BASE_URL`, so it is configuration, not
/// a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OcrRecipe {
    /// PaddleOCR-VL (and PP-OCRv6): one page, one prompt, the answer is the
    /// text.
    #[default]
    Paddle,
    /// Xiaomi-OCR-0: one page, the document prompt, and an answer that needs
    /// the vendor's postprocessing.
    Xiaomi,
}

impl OcrRecipe {
    /// Parse the `KB_OCR_RECIPE` value. Unknown text is a configuration error
    /// rather than a silent fallback: an operator who typed `xiaomi-o` should
    /// hear about it.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "paddle" | "paddleocr" | "paddleocr-vl" => Some(Self::Paddle),
            "xiaomi" | "xiaomi-ocr" | "xiaomi-ocr-0" => Some(Self::Xiaomi),
            _ => None,
        }
    }

    /// The name written into the cache fingerprint and the startup log.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Paddle => "paddle",
            Self::Xiaomi => "xiaomi",
        }
    }
}

/// How a table is written into the page's markdown.
///
/// `Markdown` is a pipe table, which the table parser reads; `Html` is the
/// vendor's own rendering, which keeps a cell's row and column spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OcrTableFormat {
    /// A markdown pipe table; a spanning cell is repeated in each position.
    #[default]
    Markdown,
    /// An HTML `<table>`, with `rowspan` and `colspan`.
    Html,
}

impl OcrTableFormat {
    /// Parse the `KB_OCR_TABLE_FORMAT` value. Unknown text is a configuration
    /// error rather than a silent fallback.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "markdown" | "md" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            _ => None,
        }
    }

    /// The name written into the cache fingerprint and the startup log.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Html => "html",
        }
    }
}

/// Default for `KB_OCR_SCAN_AREA`: the share of a page raster images must
/// cover before a page is a scan candidate.
pub const DEFAULT_OCR_SCAN_AREA: f32 = 0.90;
/// Default for `KB_OCR_SCAN_TEXT`: visible characters a page may carry and
/// still count as a scan.
pub const DEFAULT_OCR_SCAN_TEXT: u32 = 50;
/// Default for `KB_OCR_BATCH`: pages per sub-PDF request to a PDF OCR provider.
pub const DEFAULT_OCR_BATCH: u32 = 8;
/// Default for `KB_OCR_MAX_PAGES`: pages per document that may go to OCR.
pub const DEFAULT_OCR_MAX_PAGES: u32 = 300;
/// Default for `KB_OCR_BREAKER`: consecutive provider failures that open it.
pub const DEFAULT_OCR_BREAKER: u32 = 3;
/// Default for `KB_OCR_BREAKER_COOLDOWN_S`: how long the breaker stays open.
pub const DEFAULT_OCR_BREAKER_COOLDOWN_S: u64 = 60;

/// Configuration for document text extraction, layout parsing, and vision OCR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractConfig {
    /// Primary extractor sentinel or model ID (`"smart"`, `"unpdf"`, `"mineru"`, `"hybrid"`, or model ID).
    pub primary: String,
    /// Fallback extractor sentinel (`"unpdf"`, `"mineru"`, or model ID).
    pub fallback: String,
    /// Model/sentinel smart router falls through to on scans or tables.
    pub smart_fallback: String,
    /// Base URL for the on-prem MinerU sidecar (e.g. "http://localhost:8100").
    pub mineru_base_url: String,
    /// Timeout for the MinerU sidecar.
    pub mineru_timeout: Duration,
    /// API key for hosted MinerU API.
    pub mineru_api_key: String,
    /// Base URL for hosted MinerU API (default "https://mineru.net").
    pub mineru_api_base: String,
    /// Language hint for hosted MinerU API (default "id").
    pub mineru_api_lang: String,
    /// Timeout for hosted MinerU API polling.
    pub mineru_api_timeout: Duration,
    /// API key for Mistral OCR.
    pub mistral_ocr_key: String,
    /// Base URL for Mistral OCR (default "https://api.mistral.ai").
    pub mistral_ocr_base: String,
    /// Model for Mistral OCR (default "mistral-ocr-latest").
    pub mistral_ocr_model: String,
    /// Order of layout extractors to attempt for scanned PDFs (default "sidecar,mineru-api,mistral").
    pub layout_extractor_order: Vec<String>,
    /// Base URL for vision-LLM extraction (default "https://openrouter.ai/api/v1/chat/completions").
    pub vision_base_url: String,
    /// Explicit API key for vision-LLM extraction (falls back to OPENROUTER_API_KEY).
    pub vision_api_key: String,
    /// Fallback OpenRouter API key.
    pub openrouter_api_key: String,
    /// Vision model used for final OCR fallback and image OCR (default "google/gemini-2.5-flash").
    pub ocr_fallback_model: String,
    /// Full URL of a local, OpenAI-compatible `/v1/chat/completions` endpoint
    /// that OCRs one page image per request (`KB_OCR_BASE_URL`). Empty turns
    /// the local OCR adapter off, which is the default.
    pub ocr_base_url: String,
    /// Model name the local OCR endpoint serves (default "PaddlePaddle/PaddleOCR-VL-1.6").
    pub ocr_model: String,
    /// Optional bearer token for the local OCR endpoint. Never filled from
    /// `OPENROUTER_API_KEY`: a paid key must not reach a loopback/LAN URL.
    pub ocr_api_key: String,
    /// Task prefix sent as the text part (default "OCR:", PaddleOCR-VL's task selector).
    pub ocr_prompt: String,
    /// Which recipe the endpoint behind `KB_OCR_BASE_URL` speaks (`KB_OCR_RECIPE`).
    pub ocr_recipe: OcrRecipe,
    /// How a table is written into the page's markdown (`KB_OCR_TABLE_FORMAT`).
    pub ocr_table_format: OcrTableFormat,
    /// Full URL of the layout sidecar (`KB_OCR_LAYOUT_URL`). Empty turns the
    /// layout path off, which is the default: a page is read whole.
    pub ocr_layout_url: String,
    /// Rendering resolution for OCR page images (default 150).
    pub ocr_dpi: u32,
    /// Page requests in flight (default 2).
    pub ocr_concurrency: u32,
    /// Timeout for one page request (default 120 s).
    pub ocr_timeout: Duration,
    /// `max_tokens` for one page response (default 4096).
    pub ocr_max_tokens: u32,
    /// How PDF pages reach OCR (`KB_OCR_MODE`). `Auto` = hybrid when a provider
    /// is configured, off otherwise (ING-6).
    pub ocr_mode: OcrMode,
    /// Share of a page raster images must cover for it to be a scan (0.90).
    pub ocr_scan_area: f32,
    /// Visible characters a page may carry and still count as a scan (50).
    pub ocr_scan_text: u32,
    /// Pages per sub-PDF request to a PDF OCR provider (8).
    pub ocr_batch: u32,
    /// Pages per document that may go to OCR (300).
    pub ocr_max_pages: u32,
    /// Consecutive provider failures that open the process-wide breaker (3).
    pub ocr_breaker: u32,
    /// How long the breaker stays open (60 s).
    pub ocr_breaker_cooldown: Duration,
    /// Path to libpdfium.so dynamic library.
    pub pdfium_dynamic_lib_path: Option<String>,
}

/// Bump when a change to this crate alters extraction output for the same
/// file and configuration, so `KB_CACHE_DIR` entries from before are not reused.
///
/// Not bumped for the DEC-4 outline: a bump would discard cached OCR, some of
/// it paid. An entry without an outline gets one at ingest through
/// `Extractor::outline`, a free bookmark/heading read.
pub const EXTRACT_CACHE_EPOCH: u32 = 2;

impl ExtractConfig {
    /// What a cached extraction depends on: which parsers run, in which order,
    /// with which models, and which of them are reachable at all (a missing key
    /// drops a parser from the chain). Secrets are reduced to "set / not set";
    /// rotating a key does not invalidate the cache.
    #[must_use]
    pub fn cache_fingerprint(&self) -> String {
        let set = |s: &str| if s.trim().is_empty() { 0 } else { 1 };
        format!(
            "epoch={EXTRACT_CACHE_EPOCH}|ver={}|primary={}|fallback={}|smart={}|\
             mineru_url={}|mineru_api={}:{}:{}|mistral={}:{}|order={}|vision={}:{}:{}|ocr={}|\
             local_ocr={}:{}:{}:{}:{}:{}:{}|mode={}:{}:{}:{}:{}:{}|recipe={}:{}|layout={}",
            env!("CARGO_PKG_VERSION"),
            self.primary,
            self.fallback,
            self.smart_fallback,
            self.mineru_base_url,
            set(&self.mineru_api_key),
            self.mineru_api_base,
            self.mineru_api_lang,
            set(&self.mistral_ocr_key),
            self.mistral_ocr_model,
            self.layout_extractor_order.join(","),
            self.vision_base_url,
            set(&self.vision_api_key),
            set(&self.openrouter_api_key),
            self.ocr_fallback_model,
            self.ocr_base_url.trim(),
            set(&self.ocr_api_key),
            self.ocr_model,
            self.ocr_prompt,
            self.ocr_dpi,
            self.ocr_concurrency,
            self.ocr_max_tokens,
            self.ocr_mode.as_str(),
            self.ocr_scan_area,
            self.ocr_scan_text,
            self.ocr_batch,
            self.ocr_max_pages,
            self.ocr_breaker,
            self.ocr_recipe.as_str(),
            self.ocr_table_format.as_str(),
            self.ocr_layout_url.trim(),
        )
    }
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            primary: "smart".to_string(),
            fallback: "unpdf".to_string(),
            smart_fallback: "openai/gpt-4.1-nano".to_string(),
            mineru_base_url: String::new(),
            mineru_timeout: Duration::from_secs(20 * 60),
            mineru_api_key: String::new(),
            mineru_api_base: "https://mineru.net".to_string(),
            mineru_api_lang: "id".to_string(),
            mineru_api_timeout: Duration::from_secs(10 * 60),
            mistral_ocr_key: String::new(),
            mistral_ocr_base: "https://api.mistral.ai".to_string(),
            mistral_ocr_model: "mistral-ocr-latest".to_string(),
            layout_extractor_order: vec![
                "sidecar".to_string(),
                "mineru-api".to_string(),
                "mistral".to_string(),
            ],
            vision_base_url: "https://openrouter.ai/api/v1/chat/completions".to_string(),
            vision_api_key: String::new(),
            openrouter_api_key: String::new(),
            ocr_fallback_model: "google/gemini-2.5-flash".to_string(),
            ocr_base_url: String::new(),
            ocr_model: "PaddlePaddle/PaddleOCR-VL-1.6".to_string(),
            ocr_api_key: String::new(),
            ocr_prompt: "OCR:".to_string(),
            ocr_recipe: OcrRecipe::Paddle,
            ocr_table_format: OcrTableFormat::Markdown,
            ocr_layout_url: String::new(),
            ocr_dpi: 150,
            ocr_concurrency: 2,
            ocr_timeout: Duration::from_secs(120),
            ocr_max_tokens: 4096,
            ocr_mode: OcrMode::Auto,
            ocr_scan_area: DEFAULT_OCR_SCAN_AREA,
            ocr_scan_text: DEFAULT_OCR_SCAN_TEXT,
            ocr_batch: DEFAULT_OCR_BATCH,
            ocr_max_pages: DEFAULT_OCR_MAX_PAGES,
            ocr_breaker: DEFAULT_OCR_BREAKER,
            ocr_breaker_cooldown: Duration::from_secs(DEFAULT_OCR_BREAKER_COOLDOWN_S),
            pdfium_dynamic_lib_path: None,
        }
    }
}

impl ExtractConfig {
    /// Load configuration from an environment variable lookup closure.
    #[must_use]
    pub fn from_lookup<F>(mut lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        let default = Self::default();

        let openrouter_api_key = lookup("OPENROUTER_API_KEY").unwrap_or_default();
        let vision_api_key = lookup("KB_EXTRACT_VISION_API_KEY")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| openrouter_api_key.clone());

        let mineru_timeout = lookup("KB_EXTRACT_MINERU_TIMEOUT_MS")
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(default.mineru_timeout, Duration::from_millis);

        let mineru_api_timeout = lookup("KB_MINERU_API_TIMEOUT_MS")
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(default.mineru_api_timeout, Duration::from_millis);

        let layout_extractor_order = lookup("KB_LAYOUT_EXTRACTOR_ORDER")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or(default.layout_extractor_order);

        let ocr_base_url = lookup("KB_OCR_BASE_URL").unwrap_or(default.ocr_base_url);
        let ocr_api_key = lookup("KB_OCR_API_KEY").unwrap_or(default.ocr_api_key);

        // An unknown mode is a mistake worth hearing about, but it must not
        // stop the server from starting: `Auto` is the safe reading.
        let ocr_mode = match lookup("KB_OCR_MODE") {
            Some(raw) => OcrMode::parse(&raw).unwrap_or_else(|| {
                tracing::warn!(
                    value = %raw,
                    "KB_OCR_MODE must be off, hybrid or full; using the default"
                );
                default.ocr_mode
            }),
            None => default.ocr_mode,
        };

        // The recipe and the table format are the same kind of mistake: a
        // value nobody serves must not silently become the default.
        let ocr_recipe = match lookup("KB_OCR_RECIPE") {
            Some(raw) => OcrRecipe::parse(&raw).unwrap_or_else(|| {
                tracing::warn!(
                    value = %raw,
                    "KB_OCR_RECIPE must be paddle or xiaomi; using the default"
                );
                default.ocr_recipe
            }),
            None => default.ocr_recipe,
        };
        let ocr_table_format = match lookup("KB_OCR_TABLE_FORMAT") {
            Some(raw) => OcrTableFormat::parse(&raw).unwrap_or_else(|| {
                tracing::warn!(
                    value = %raw,
                    "KB_OCR_TABLE_FORMAT must be markdown or html; using the default"
                );
                default.ocr_table_format
            }),
            None => default.ocr_table_format,
        };

        // An empty value the operator wrote by hand means "use the default",
        // not "send an empty model name" — except for the two that switch the
        // adapter: the URL (off when empty) and the key (no header when empty).
        let non_empty_or = |value: Option<String>, fallback: String| {
            value.filter(|s| !s.trim().is_empty()).unwrap_or(fallback)
        };

        Self {
            primary: lookup("KB_EXTRACT_PRIMARY").unwrap_or(default.primary),
            fallback: lookup("KB_EXTRACT_FALLBACK").unwrap_or(default.fallback),
            smart_fallback: lookup("KB_EXTRACT_SMART_FALLBACK").unwrap_or(default.smart_fallback),
            mineru_base_url: lookup("KB_EXTRACT_MINERU_BASE_URL")
                .unwrap_or(default.mineru_base_url),
            mineru_timeout,
            mineru_api_key: lookup("KB_MINERU_API_KEY").unwrap_or(default.mineru_api_key),
            mineru_api_base: lookup("KB_MINERU_API_BASE").unwrap_or(default.mineru_api_base),
            mineru_api_lang: lookup("KB_MINERU_API_LANG").unwrap_or(default.mineru_api_lang),
            mineru_api_timeout,
            mistral_ocr_key: lookup("KB_MISTRAL_OCR_KEY").unwrap_or(default.mistral_ocr_key),
            mistral_ocr_base: lookup("KB_MISTRAL_OCR_BASE").unwrap_or(default.mistral_ocr_base),
            mistral_ocr_model: lookup("KB_MISTRAL_OCR_MODEL").unwrap_or(default.mistral_ocr_model),
            layout_extractor_order,
            vision_base_url: lookup("KB_EXTRACT_VISION_BASE_URL")
                .unwrap_or(default.vision_base_url),
            vision_api_key,
            openrouter_api_key,
            ocr_fallback_model: lookup("OCR_FALLBACK_MODEL").unwrap_or(default.ocr_fallback_model),
            ocr_base_url: ocr_base_url.trim().to_string(),
            ocr_model: non_empty_or(lookup("KB_OCR_MODEL"), default.ocr_model),
            ocr_api_key: ocr_api_key.trim().to_string(),
            ocr_prompt: non_empty_or(lookup("KB_OCR_PROMPT"), default.ocr_prompt),
            ocr_recipe,
            ocr_table_format,
            ocr_layout_url: lookup("KB_OCR_LAYOUT_URL")
                .unwrap_or(default.ocr_layout_url)
                .trim()
                .to_string(),
            ocr_dpi: lookup("KB_OCR_DPI")
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|dpi| *dpi > 0)
                .unwrap_or(default.ocr_dpi),
            ocr_concurrency: lookup("KB_OCR_CONCURRENCY")
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(default.ocr_concurrency),
            // A dense page under a full batch takes minutes on Xiaomi-OCR-0;
            // the vendor waits 600 s. Timing out earlier turns a slow page into
            // retries, an open breaker and lost pages.
            ocr_timeout: lookup("KB_OCR_TIMEOUT_MS")
                .and_then(|s| s.parse::<u64>().ok())
                .map_or(
                    match ocr_recipe {
                        OcrRecipe::Xiaomi => Duration::from_secs(600),
                        _ => default.ocr_timeout,
                    },
                    Duration::from_millis,
                ),
            ocr_max_tokens: lookup("KB_OCR_MAX_TOKENS")
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|n| *n > 0)
                // Xiaomi-OCR-0 writes a dense page as one answer; at 4096 tokens
                // a long table page is cut off. Its own default applies unless
                // the operator set one.
                .unwrap_or(match ocr_recipe {
                    OcrRecipe::Xiaomi => crate::xiaomi_ocr::DEFAULT_XIAOMI_MAX_TOKENS,
                    _ => default.ocr_max_tokens,
                }),
            ocr_mode,
            // A share below 1 % or above 100 % is an operator typo; clamp it
            // rather than making every page a scan or none.
            ocr_scan_area: lookup("KB_OCR_SCAN_AREA")
                .and_then(|s| s.parse::<f32>().ok())
                .filter(|a| a.is_finite())
                .map_or(default.ocr_scan_area, |a| a.clamp(0.01, 1.0)),
            ocr_scan_text: lookup("KB_OCR_SCAN_TEXT")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(default.ocr_scan_text),
            ocr_batch: lookup("KB_OCR_BATCH")
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(default.ocr_batch),
            ocr_max_pages: lookup("KB_OCR_MAX_PAGES")
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(default.ocr_max_pages),
            ocr_breaker: lookup("KB_OCR_BREAKER")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(default.ocr_breaker),
            ocr_breaker_cooldown: lookup("KB_OCR_BREAKER_COOLDOWN_S")
                .and_then(|s| s.parse::<u64>().ok())
                .map_or(default.ocr_breaker_cooldown, Duration::from_secs),
            pdfium_dynamic_lib_path: lookup("PDFIUM_DYNAMIC_LIB_PATH")
                .or(default.pdfium_dynamic_lib_path),
        }
    }

    /// Load configuration from real environment variables.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Whether any OCR provider is configured: the local chat adapter
    /// (`KB_OCR_BASE_URL`) or Mistral (`KB_MISTRAL_OCR_KEY`).
    #[must_use]
    pub fn has_ocr_provider(&self) -> bool {
        !self.ocr_base_url.trim().is_empty() || !self.mistral_ocr_key.trim().is_empty()
    }

    /// The mode that actually runs: `Auto` reads as `Hybrid` when a provider is
    /// configured and `Off` otherwise, so the default changes nothing for an
    /// installation that never set one up.
    #[must_use]
    pub fn resolved_ocr_mode(&self) -> OcrMode {
        match self.ocr_mode {
            OcrMode::Auto if self.has_ocr_provider() => OcrMode::Hybrid,
            OcrMode::Auto => OcrMode::Off,
            explicit => explicit,
        }
    }

    /// Effective API key for vision calls (falls back to OpenRouter API key).
    #[must_use]
    pub fn effective_vision_api_key(&self) -> &str {
        if !self.vision_api_key.is_empty() {
            &self.vision_api_key
        } else {
            &self.openrouter_api_key
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn local_ocr_is_off_by_default_and_keeps_the_documented_defaults() {
        let cfg = ExtractConfig::from_lookup(|_| None);
        assert!(cfg.ocr_base_url.is_empty(), "no URL means no adapter");
        assert_eq!(cfg.ocr_model, "PaddlePaddle/PaddleOCR-VL-1.6");
        assert!(cfg.ocr_api_key.is_empty());
        assert_eq!(cfg.ocr_prompt, "OCR:");
        assert_eq!(cfg.ocr_dpi, 150);
        assert_eq!(cfg.ocr_concurrency, 2);
        assert_eq!(cfg.ocr_timeout, Duration::from_millis(120_000));
        assert_eq!(cfg.ocr_max_tokens, 4096);
        assert_eq!(
            cfg.layout_extractor_order,
            vec!["sidecar", "mineru-api", "mistral"]
        );
    }

    #[test]
    fn every_ocr_setting_is_read_from_its_documented_name() {
        let vars = [
            (
                "KB_OCR_BASE_URL",
                "http://127.0.0.1:8000/v1/chat/completions",
            ),
            ("KB_OCR_MODEL", "PaddlePaddle/PaddleOCR-VL-1.6"),
            ("KB_OCR_API_KEY", "local-key"),
            ("KB_OCR_PROMPT", "Table Recognition:"),
            ("KB_OCR_DPI", "200"),
            ("KB_OCR_CONCURRENCY", "4"),
            ("KB_OCR_TIMEOUT_MS", "30000"),
            ("KB_OCR_MAX_TOKENS", "2048"),
        ];
        let cfg = ExtractConfig::from_lookup(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        });
        assert_eq!(
            cfg.ocr_base_url,
            "http://127.0.0.1:8000/v1/chat/completions"
        );
        assert_eq!(cfg.ocr_model, "PaddlePaddle/PaddleOCR-VL-1.6");
        assert_eq!(cfg.ocr_api_key, "local-key");
        assert_eq!(cfg.ocr_prompt, "Table Recognition:");
        assert_eq!(cfg.ocr_dpi, 200);
        assert_eq!(cfg.ocr_concurrency, 4);
        assert_eq!(cfg.ocr_timeout, Duration::from_millis(30_000));
        assert_eq!(cfg.ocr_max_tokens, 2048);
    }

    #[test]
    fn the_xiaomi_recipe_has_its_own_token_budget_unless_one_is_set() {
        let unset = ExtractConfig::from_lookup(|name| match name {
            "KB_OCR_RECIPE" => Some("xiaomi".to_string()),
            _ => None,
        });
        assert_eq!(
            unset.ocr_max_tokens,
            crate::xiaomi_ocr::DEFAULT_XIAOMI_MAX_TOKENS
        );
        let set = ExtractConfig::from_lookup(|name| match name {
            "KB_OCR_RECIPE" => Some("xiaomi".to_string()),
            "KB_OCR_MAX_TOKENS" => Some("2048".to_string()),
            _ => None,
        });
        assert_eq!(set.ocr_max_tokens, 2048);
        assert_eq!(unset.ocr_timeout, Duration::from_secs(600));
    }

    #[test]
    fn the_ocr_key_never_falls_back_to_the_hosted_openrouter_key() {
        let cfg = ExtractConfig::from_lookup(|name| match name {
            "OPENROUTER_API_KEY" => Some("sk-paid".to_string()),
            "KB_OCR_BASE_URL" => Some("http://127.0.0.1:8000/v1/chat/completions".to_string()),
            _ => None,
        });
        assert!(
            cfg.ocr_api_key.is_empty(),
            "a paid key must never be sent to a loopback/LAN endpoint"
        );
        // The extraction fingerprint counts a secret only as set / not set.
        assert!(!cfg.cache_fingerprint().contains("sk-paid"));
    }

    #[test]
    fn ocr_settings_move_the_extraction_cache_fingerprint() {
        let base = ExtractConfig::default();
        let mut changed = base.clone();
        changed.ocr_prompt = "Table Recognition:".to_string();
        assert_ne!(base.cache_fingerprint(), changed.cache_fingerprint());

        let mut url = base.clone();
        url.ocr_base_url = "http://127.0.0.1:8000/v1/chat/completions".to_string();
        assert_ne!(base.cache_fingerprint(), url.cache_fingerprint());

        let mut dpi = base.clone();
        dpi.ocr_dpi = 200;
        assert_ne!(base.cache_fingerprint(), dpi.cache_fingerprint());
    }

    #[test]
    fn the_ocr_offload_settings_read_their_documented_names() {
        let vars = [
            ("KB_OCR_MODE", "full"),
            ("KB_OCR_SCAN_AREA", "0.75"),
            ("KB_OCR_SCAN_TEXT", "12"),
            ("KB_OCR_BATCH", "4"),
            ("KB_OCR_MAX_PAGES", "40"),
            ("KB_OCR_BREAKER", "5"),
            ("KB_OCR_BREAKER_COOLDOWN_S", "15"),
        ];
        let cfg = ExtractConfig::from_lookup(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        });
        assert_eq!(cfg.ocr_mode, OcrMode::Full);
        assert!((cfg.ocr_scan_area - 0.75).abs() < 1e-6);
        assert_eq!(cfg.ocr_scan_text, 12);
        assert_eq!(cfg.ocr_batch, 4);
        assert_eq!(cfg.ocr_max_pages, 40);
        assert_eq!(cfg.ocr_breaker, 5);
        assert_eq!(cfg.ocr_breaker_cooldown, Duration::from_secs(15));
    }

    #[test]
    fn the_ocr_offload_defaults_are_the_documented_ones() {
        let cfg = ExtractConfig::from_lookup(|_| None);
        assert_eq!(cfg.ocr_mode, OcrMode::Auto);
        assert!((cfg.ocr_scan_area - 0.90).abs() < 1e-6);
        assert_eq!(cfg.ocr_scan_text, 50);
        assert_eq!(cfg.ocr_batch, 8);
        assert_eq!(cfg.ocr_max_pages, 300);
        assert_eq!(cfg.ocr_breaker, 3);
        assert_eq!(cfg.ocr_breaker_cooldown, Duration::from_secs(60));
    }

    #[test]
    fn auto_is_off_without_a_provider_and_hybrid_with_one() {
        let mut cfg = ExtractConfig::default();
        assert_eq!(cfg.resolved_ocr_mode(), OcrMode::Off);
        cfg.ocr_base_url = "http://127.0.0.1:8000/v1/chat/completions".to_string();
        assert_eq!(cfg.resolved_ocr_mode(), OcrMode::Hybrid);
        cfg.ocr_base_url.clear();
        cfg.mistral_ocr_key = "k".to_string();
        assert_eq!(cfg.resolved_ocr_mode(), OcrMode::Hybrid);
        // An explicit mode is never overridden by Auto's rule.
        cfg.ocr_mode = OcrMode::Off;
        assert_eq!(cfg.resolved_ocr_mode(), OcrMode::Off);
    }

    #[test]
    fn an_unknown_mode_falls_back_to_auto_instead_of_stopping_the_server() {
        let cfg = ExtractConfig::from_lookup(|name| match name {
            "KB_OCR_MODE" => Some("ful".to_string()),
            _ => None,
        });
        assert_eq!(cfg.ocr_mode, OcrMode::Auto);
        assert_eq!(OcrMode::parse("FULL"), Some(OcrMode::Full));
        assert_eq!(OcrMode::parse(" hybrid "), Some(OcrMode::Hybrid));
        assert_eq!(OcrMode::parse("maybe"), None);
    }

    #[test]
    fn the_offload_tunables_move_the_cache_fingerprint() {
        let base = ExtractConfig::default();
        for mutate in [
            |c: &mut ExtractConfig| c.ocr_mode = OcrMode::Full,
            |c: &mut ExtractConfig| c.ocr_scan_area = 0.5,
            |c: &mut ExtractConfig| c.ocr_scan_text = 5,
            |c: &mut ExtractConfig| c.ocr_batch = 2,
            |c: &mut ExtractConfig| c.ocr_max_pages = 10,
            |c: &mut ExtractConfig| c.ocr_breaker = 9,
        ] {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert_ne!(base.cache_fingerprint(), changed.cache_fingerprint());
        }
    }
}
