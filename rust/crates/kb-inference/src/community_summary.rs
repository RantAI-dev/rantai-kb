//! The chat client for graph community summaries (`DEC-5`).
//!
//! One HTTP call, one prompt, one summary. The caller (`kb-server communities
//! build`) owns everything that costs money: the prompt construction, the
//! `chars / 4` estimate, the `--max-usd` ceiling and the disk cache. This module
//! only speaks the provider's protocol, so a fake can stand in for it in tests
//! without a network.
//!
//! # Reused configuration, not a second one
//!
//! The client reads the same settings as the LLM entity extractor
//! (`ENTITY_EXTRACTION_LLM_MODEL`, `ENTITY_EXTRACTION_LLM_BASE_URL`,
//! `ENTITY_EXTRACTION_LLM_API_KEY`, falling back to `OPENROUTER_API_KEY`), so an
//! operator who has configured one chat model has configured this one too. The
//! build command prints the model and base URL before estimating.
//!
//! # Reasoning is off, deliberately
//!
//! `"reasoning": {"enabled": false}`. Reasoning tokens count against
//! `max_tokens`, and in the `EVAL-3` prototype deepseek-v4-flash spent the whole
//! 320-token budget thinking on 95 of 159 communities and returned no text. A
//! summary needs no chain of thought, and an empty answer is never cached and is
//! counted as a failure by the caller.

use std::time::Duration;

use async_trait::async_trait;
use kb_ports::PortError;
use serde_json::{Value, json};

use crate::llm_extractor::ExtractorSettings;

/// What the summariser needs. Derived from [`ExtractorSettings`] so the env
/// names and defaults cannot drift from the entity extractor's.
#[derive(Debug, Clone)]
pub struct CommunitySummarySettings {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl CommunitySummarySettings {
    /// The entity extractor's settings, with a summary-sized completion budget.
    ///
    /// `max_tokens` is the caller's (`--max-tokens`, default 320 as the
    /// prototype used); the extractor's 4,000 is a JSON-document budget, not a
    /// 120–180-word summary budget.
    #[must_use]
    pub fn from_extractor(settings: &ExtractorSettings, max_tokens: u32) -> Self {
        Self {
            api_key: settings.api_key.clone(),
            base_url: settings.base_url.clone(),
            model: settings.model.clone(),
            max_tokens,
            temperature: 0.0,
            timeout: settings.timeout,
            max_retries: settings.max_retries,
        }
    }

    /// Read the same environment the entity extractor reads.
    #[must_use]
    pub fn from_lookup<F>(lookup: F, max_tokens: u32) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let extractor = ExtractorSettings::from_lookup(lookup);
        Self::from_extractor(&extractor, max_tokens)
    }
}

/// A chat model that writes one community summary per prompt.
///
/// `Ok(None)` means the provider answered with no text (a reasoning model that
/// spent its budget thinking, or an empty completion). That is not a transport
/// error — retrying it wastes money — but it is not a summary either, so the
/// caller counts it as a failure and never caches it.
#[async_trait]
pub trait CommunitySummariser: Send + Sync {
    /// The model id the summaries are keyed by.
    fn model(&self) -> &str;

    /// One summary for one prompt.
    async fn summarise(&self, prompt: &str) -> Result<Option<String>, PortError>;
}

/// OpenRouter-compatible chat completions.
pub struct LlmCommunitySummariser {
    settings: CommunitySummarySettings,
    client: reqwest::Client,
}

impl LlmCommunitySummariser {
    pub fn new(settings: CommunitySummarySettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest client: {e}")))?;
        Ok(Self { settings, client })
    }

    /// The request body, in one place so a test can pin the two properties that
    /// cost money when they are wrong: reasoning off, temperature 0.
    #[must_use]
    pub fn request_body(&self, prompt: &str) -> Value {
        json!({
            "model": self.settings.model,
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": self.settings.max_tokens,
            "temperature": self.settings.temperature,
            "reasoning": { "enabled": false },
        })
    }
}

#[async_trait]
impl CommunitySummariser for LlmCommunitySummariser {
    fn model(&self) -> &str {
        &self.settings.model
    }

    async fn summarise(&self, prompt: &str) -> Result<Option<String>, PortError> {
        let url = format!(
            "{}/chat/completions",
            self.settings.base_url.trim_end_matches('/')
        );
        let body = self.request_body(prompt);
        let mut last_error = String::new();
        for attempt in 0..self.settings.max_retries.max(1) {
            let mut request = self.client.post(&url).json(&body);
            if !self.settings.api_key.trim().is_empty() {
                request = request.bearer_auth(&self.settings.api_key);
            }
            request = request
                .header("HTTP-Referer", "https://rantai.dev")
                .header("X-Title", "RantAI KB community summaries");
            match request.send().await {
                Ok(response) if response.status().is_success() => {
                    let value: Value = response.json().await.map_err(|e| {
                        PortError::Malformed(format!("community summary JSON: {e}"))
                    })?;
                    let content = value
                        .get("choices")
                        .and_then(Value::as_array)
                        .and_then(|choices| choices.first())
                        .and_then(|choice| choice.get("message"))
                        .and_then(|message| message.get("content"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    return Ok(if content.is_empty() {
                        None
                    } else {
                        Some(content)
                    });
                }
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    last_error = format!("HTTP {status}: {text}");
                }
                Err(error) => {
                    last_error = error.to_string();
                }
            }
            if attempt + 1 < self.settings.max_retries.max(1) {
                tokio::time::sleep(Duration::from_secs(2 * (u64::from(attempt) + 1))).await;
            }
        }
        Err(PortError::Unavailable(format!(
            "community summary request failed after {} attempt(s): {last_error}",
            self.settings.max_retries.max(1)
        )))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn settings() -> CommunitySummarySettings {
        CommunitySummarySettings {
            api_key: "k".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "test/model".into(),
            max_tokens: 320,
            temperature: 0.0,
            timeout: Duration::from_millis(50),
            max_retries: 1,
        }
    }

    #[test]
    fn the_request_turns_reasoning_off_and_the_temperature_down() {
        let summariser = LlmCommunitySummariser::new(settings()).expect("client");
        let body = summariser.request_body("prompt");
        assert_eq!(body["reasoning"]["enabled"], false);
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["max_tokens"], 320);
        assert_eq!(body["model"], "test/model");
        assert_eq!(body["messages"][0]["content"], "prompt");
    }

    #[test]
    fn settings_are_read_through_the_extractors_env_names() {
        let settings = CommunitySummarySettings::from_lookup(
            |key| match key {
                "ENTITY_EXTRACTION_LLM_MODEL" => Some("deepseek/deepseek-v4-flash".into()),
                "ENTITY_EXTRACTION_LLM_BASE_URL" => Some("http://llm.local/v1/".into()),
                "OPENROUTER_API_KEY" => Some("sk-test".into()),
                _ => None,
            },
            320,
        );
        assert_eq!(settings.model, "deepseek/deepseek-v4-flash");
        assert_eq!(settings.base_url, "http://llm.local/v1");
        assert_eq!(settings.api_key, "sk-test");
        assert_eq!(settings.max_tokens, 320);
        assert_eq!(settings.temperature, 0.0);
    }
}
