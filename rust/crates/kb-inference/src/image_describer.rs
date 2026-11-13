//! Image readings over OpenAI-compatible chat completions (`ING-5`).
//!
//! One request per image asks a vision model for a JSON object
//! `{"kind", "description", "text"}`: what kind of picture it is, what it shows
//! in the words a searcher would use, and the text legible in it. The same
//! request works against OpenRouter, vLLM and llama.cpp's server.
//!
//! Models do not always follow the format, so the reply is parsed leniently
//! ([`parse_reading`]): a JSON object anywhere in the reply, a JSON object cut
//! off by the token limit (its complete fields are kept), or plain prose (taken
//! as the description). Only an empty reply is an error: reading it as "nothing
//! to see" would cache a lie.
//!
//! Rate limits are expected: a 429 is retried with backoff, honouring
//! `Retry-After`, because free and shared endpoints throttle per minute and an
//! ingest of a slide deck sends dozens of images in a row.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use kb_ports::{ImageDescriber, ImageReading, PortError, VisualKind};
use serde_json::json;

/// Version of [`IMAGE_READ_PROMPT`]; part of the cache key.
pub const IMAGE_READ_PROMPT_VERSION: &str = "v1";

/// The instruction, with `{language}` replaced by the configured language.
pub const IMAGE_READ_PROMPT: &str = r#"You are indexing an image so people can find it by describing it. Reply with only a JSON object, no other text:
{"kind": "...", "description": "...", "text": "..."}
kind: one of photo, diagram, chart, screenshot, table, document, map, logo, decorative, other.
description: at most 80 words, plain sentences naming what a searcher would mention: the main subjects and what they are doing, the setting, notable objects and colours; for a chart, diagram, table or map: its type, title, axes or labels, categories, key values and the main trend. Name a specific brand, place, person or species only if it is written or unmistakable in the image.
text: the most important legible words and numbers in the image, verbatim, at most 60 words; an empty string if there are none.
Write the description in {language}."#;

/// Settings for [`HttpImageDescriber`]. Defaults live in `kb-server`.
#[derive(Debug, Clone)]
pub struct ImageDescriberSettings {
    /// Full chat-completions endpoint.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub request_timeout: Duration,
    pub max_tokens: u32,
    /// Send `reasoning: {"enabled": false}` (OpenRouter thinking models).
    pub no_reasoning: bool,
    /// Language of the description (`KB_IMAGE_DESCRIBE_LANGUAGE`).
    pub language: String,
    /// Attempts per image on 429 / 5xx / transport errors.
    pub max_attempts: u32,
}

/// Image describer over an OpenAI-compatible chat endpoint.
pub struct HttpImageDescriber {
    client: reqwest::Client,
    settings: ImageDescriberSettings,
    prompt: String,
}

impl HttpImageDescriber {
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    pub fn new(settings: ImageDescriberSettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest builder: {e}")))?;
        let prompt = IMAGE_READ_PROMPT.replace("{language}", settings.language.trim());
        Ok(Self {
            client,
            settings,
            prompt,
        })
    }

    /// The request body: the instruction, then the image.
    #[must_use]
    pub fn build_body(&self, mime: &str, bytes: &[u8]) -> serde_json::Value {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut body = json!({
            "model": self.settings.model,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": self.prompt},
                {"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{encoded}")}},
            ]}],
            "temperature": 0,
            "max_tokens": self.settings.max_tokens,
        });
        if self.settings.no_reasoning {
            body["reasoning"] = json!({"enabled": false});
        }
        body
    }

    /// How long to wait before attempt `attempt + 1`: `Retry-After` when the
    /// server sent one (capped at a minute), else exponential from one second.
    fn backoff(attempt: u32, retry_after: Option<&str>) -> Duration {
        if let Some(secs) = retry_after.and_then(|v| v.trim().parse::<u64>().ok()) {
            return Duration::from_secs(secs.min(60));
        }
        Duration::from_millis(1_000u64.saturating_mul(1 << attempt.min(5)))
    }
}

#[async_trait]
impl ImageDescriber for HttpImageDescriber {
    async fn read(&self, mime: &str, bytes: &[u8]) -> Result<ImageReading, PortError> {
        let body = self.build_body(mime, bytes);
        let attempts = self.settings.max_attempts.max(1);
        let mut last: Option<PortError> = None;
        for attempt in 0..attempts {
            let mut req = self.client.post(&self.settings.base_url).json(&body);
            if let Some(key) = &self.settings.api_key {
                req = req.bearer_auth(key);
            }
            let mut retry_after = None;
            match req.send().await {
                Err(e) => last = Some(PortError::Unavailable(format!("image read request: {e}"))),
                Ok(res) => {
                    let status = res.status().as_u16();
                    retry_after = res
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    let raw = res
                        .bytes()
                        .await
                        .map_err(|e| PortError::Unavailable(format!("image read body: {e}")));
                    match raw {
                        Err(e) => last = Some(e),
                        Ok(raw) if (200..300).contains(&status) => {
                            return parse_response(&raw);
                        }
                        Ok(raw) => {
                            let snippet: String =
                                String::from_utf8_lossy(&raw).chars().take(300).collect();
                            let err = PortError::Unavailable(format!(
                                "image read http {status}: {snippet}"
                            ));
                            if status != 429 && status < 500 {
                                return Err(err);
                            }
                            last = Some(err);
                        }
                    }
                }
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(Self::backoff(attempt, retry_after.as_deref())).await;
            }
        }
        Err(last
            .unwrap_or_else(|| PortError::Unavailable("image read: no attempt was made".into())))
    }
}

/// The chat response → a reading.
fn parse_response(raw: &[u8]) -> Result<ImageReading, PortError> {
    let reply: serde_json::Value = serde_json::from_slice(raw)
        .map_err(|e| PortError::Malformed(format!("image read: response is not JSON: {e}")))?;
    let text = reply
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    parse_reading(text).ok_or_else(|| {
        let finish = reply
            .pointer("/choices/0/finish_reason")
            .and_then(|c| c.as_str())
            .unwrap_or("unknown");
        PortError::Malformed(format!(
            "image read: empty reply (finish_reason {finish}); a thinking model may need KB_IMAGE_DESCRIBE_NO_REASONING=true"
        ))
    })
}

/// A model's reply → a reading, as leniently as the reply allows.
///
/// `None` only when there is nothing at all: an empty reply, or a JSON object
/// whose description and text are both empty.
#[must_use]
pub fn parse_reading(reply: &str) -> Option<ImageReading> {
    let reply = reply.trim();
    if reply.is_empty() {
        return None;
    }
    let reading = match reply.find('{') {
        Some(start) => {
            let candidate = &reply[start..];
            let end = candidate.rfind('}').map_or(candidate.len(), |i| i + 1);
            match serde_json::from_str::<serde_json::Value>(&candidate[..end]) {
                Ok(v) => ImageReading {
                    kind: VisualKind::parse(v.get("kind").and_then(|k| k.as_str()).unwrap_or("")),
                    description: string_field(&v, "description"),
                    text: string_field(&v, "text"),
                },
                // Cut off by the token limit, or not quite JSON: keep the
                // fields that are complete.
                Err(_) => ImageReading {
                    kind: VisualKind::parse(&loose_field(candidate, "kind").unwrap_or_default()),
                    description: loose_field(candidate, "description").unwrap_or_default(),
                    text: loose_field(candidate, "text").unwrap_or_default(),
                },
            }
        }
        // Prose: the whole reply is the description.
        None => ImageReading {
            kind: VisualKind::Other,
            description: reply.to_string(),
            text: String::new(),
        },
    };
    if reading.description.trim().is_empty() && reading.text.trim().is_empty() {
        None
    } else {
        Some(ImageReading {
            kind: reading.kind,
            description: reading.description.trim().to_string(),
            text: reading.text.trim().to_string(),
        })
    }
}

fn string_field(v: &serde_json::Value, name: &str) -> String {
    match v.get(name) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// `"name": "value"` from text that may not be valid JSON; a value cut off at
/// the end is kept up to where it stops (it is still the model's words).
fn loose_field(text: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\"");
    let after_key = &text[text.find(&key)? + key.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
    let body = after_colon.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};

    use super::*;

    #[test]
    fn a_json_reply_is_read_field_by_field() {
        let r = parse_reading(
            r#"{"kind": "photo", "description": "A man plays tennis.", "text": "OLYMPUS"}"#,
        )
        .unwrap();
        assert_eq!(r.kind, VisualKind::Photo);
        assert_eq!(r.description, "A man plays tennis.");
        assert_eq!(r.text, "OLYMPUS");
    }

    #[test]
    fn json_inside_a_code_fence_or_prose_is_found() {
        let r = parse_reading(
            "Sure!\n```json\n{\"kind\":\"chart\",\"description\":\"Bars.\",\"text\":\"\"}\n```",
        )
        .unwrap();
        assert_eq!(r.kind, VisualKind::Chart);
        assert_eq!(r.description, "Bars.");
    }

    #[test]
    fn a_reply_cut_off_by_the_token_limit_keeps_what_was_written() {
        let r = parse_reading(
            r#"{"kind": "chart", "description": "A bar chart titled \"Mental health\" compares risk for disorders such as dysth"#,
        )
        .unwrap();
        assert_eq!(r.kind, VisualKind::Chart);
        assert_eq!(
            r.description,
            "A bar chart titled \"Mental health\" compares risk for disorders such as dysth"
        );
        assert_eq!(r.text, "");
    }

    #[test]
    fn prose_is_the_description_and_nothing_is_none() {
        let r = parse_reading("The clock is mounted on a wooden wall.").unwrap();
        assert_eq!(r.kind, VisualKind::Other);
        assert_eq!(r.description, "The clock is mounted on a wooden wall.");
        assert!(parse_reading("   ").is_none());
        assert!(parse_reading(r#"{"kind":"photo","description":"","text":""}"#).is_none());
    }

    #[test]
    fn a_text_field_given_as_a_list_is_joined() {
        let r =
            parse_reading(r#"{"kind":"document","description":"A sign.","text":["STOP","AHEAD"]}"#)
                .unwrap();
        assert_eq!(r.text, "STOP AHEAD");
    }

    #[test]
    fn retry_after_is_honoured_and_capped() {
        assert_eq!(
            HttpImageDescriber::backoff(0, Some("7")),
            Duration::from_secs(7)
        );
        assert_eq!(
            HttpImageDescriber::backoff(0, Some("600")),
            Duration::from_secs(60)
        );
        assert_eq!(HttpImageDescriber::backoff(2, None), Duration::from_secs(4));
    }

    type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

    fn settings(url: String) -> ImageDescriberSettings {
        ImageDescriberSettings {
            base_url: url,
            model: "m".into(),
            api_key: Some("k".into()),
            request_timeout: Duration::from_secs(5),
            max_tokens: 300,
            no_reasoning: false,
            language: "Indonesian".into(),
            max_attempts: 3,
        }
    }

    /// Answers 429 `fail_first` times (with Retry-After: 0), then `reply`.
    async fn fake(fail_first: usize, reply: &'static str) -> (String, Seen) {
        async fn handle(
            State((seen, fail_first, reply)): State<(Seen, usize, &'static str)>,
            Json(body): Json<serde_json::Value>,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            let n = {
                let mut s = seen.lock().unwrap();
                s.push(body);
                s.len()
            };
            if n <= fail_first {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "0")],
                    "slow down",
                )
                    .into_response();
            }
            Json(json!({"choices": [{"message": {"content": reply}}]})).into_response()
        }
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/chat/completions", post(handle))
            .with_state((seen.clone(), fail_first, reply));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/v1/chat/completions"), seen)
    }

    #[tokio::test]
    async fn a_rate_limited_call_is_retried_and_the_image_is_sent_once_per_attempt() {
        let (url, seen) = fake(2, r#"{"kind":"photo","description":"Two cats.","text":""}"#).await;
        let d = HttpImageDescriber::new(settings(url)).unwrap();
        let r = d.read("image/jpeg", b"\xff\xd8jpeg").await.unwrap();
        assert_eq!(r.description, "Two cats.");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        let content = &seen[0]["messages"][0]["content"];
        assert!(
            content[0]["text"]
                .as_str()
                .unwrap()
                .contains("in Indonesian.")
        );
        assert!(
            content[1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
    }

    #[tokio::test]
    async fn an_empty_reply_is_an_error_not_a_blank_reading() {
        let (url, _) = fake(0, "").await;
        let d = HttpImageDescriber::new(settings(url)).unwrap();
        let err = d.read("image/png", b"png").await.unwrap_err();
        assert!(err.to_string().contains("empty reply"), "{err}");
    }

    #[tokio::test]
    async fn a_client_error_is_not_retried() {
        async fn bad(State(seen): State<Seen>) -> (StatusCode, &'static str) {
            seen.lock().unwrap().push(json!({}));
            (StatusCode::BAD_REQUEST, "bad image")
        }
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/chat/completions", post(bad))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let d = HttpImageDescriber::new(settings(format!("http://{addr}/v1/chat/completions")))
            .unwrap();
        let err = d.read("image/png", b"png").await.unwrap_err();
        assert!(err.to_string().contains("400"), "{err}");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
