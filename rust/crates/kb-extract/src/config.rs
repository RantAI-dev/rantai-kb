//! Extraction pipeline configuration.
//!
//! Mirrors `src/lib/rag/config.ts` and `src/lib/ocr/config.ts`.
//! Loads extractor endpoints, model IDs, timeouts, and API keys from environment
//! variables with identical names and defaults.

use std::time::Duration;

use serde::{Deserialize, Serialize};

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
    /// Path to libpdfium.so dynamic library.
    pub pdfium_dynamic_lib_path: Option<String>,
}

/// Bump when a change to this crate alters extraction output for the same
/// file and configuration, so `KB_CACHE_DIR` entries from before are not reused.
pub const EXTRACT_CACHE_EPOCH: u32 = 1;

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
             mineru_url={}|mineru_api={}:{}:{}|mistral={}:{}|order={}|vision={}:{}:{}|ocr={}",
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
            pdfium_dynamic_lib_path: lookup("PDFIUM_DYNAMIC_LIB_PATH")
                .or(default.pdfium_dynamic_lib_path),
        }
    }

    /// Load configuration from real environment variables.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
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
