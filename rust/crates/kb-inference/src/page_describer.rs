//! Page visual descriptions ("captions") over OpenAI-compatible chat
//! completions.
//!
//! One request per page carries the page image as a data URL and asks a vision
//! model to describe only what the page *draws* — charts, tables, diagrams,
//! maps, photos — never its running text. The description is used at ingest in
//! two independent places ([`kb_ports::PageDescriber`]): appended to the stored
//! page text that the page reranker reads, and embedded into the text index as
//! a figure chunk, so chart content becomes searchable as text.
//!
//! This adapter only asks and parses; the per-page timeout, the bounded
//! concurrency and the best-effort counting live in the engine; caching lives
//! in `kb-cache`.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use kb_ports::{PageDescriber, PageImage, PortError};
use serde_json::json;

/// Default endpoint (`KB_PAGE_CAPTION_BASE_URL`).
pub const DEFAULT_PAGE_CAPTION_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Version of [`PAGE_DESCRIBE_PROMPT`]. Part of the disk-cache key, so a prompt
/// change never reuses an old answer.
pub const PAGE_DESCRIBE_PROMPT_VERSION: &str = "v1";

/// The instruction sent with every page. Deliberately narrow: only visuals,
/// echoing any title, axis labels and numbers so a query can match them, in the
/// page's own language, and a single sentinel for a page with no visuals.
pub const PAGE_DESCRIBE_PROMPT: &str = "Describe only the charts, graphs, tables, diagrams, maps and photos on this page: what each shows, its title, axis labels, key numbers, categories and the main trend or comparison. Do not repeat the page's running text. Write plain sentences in the language of the page. If the page has no such visual content, reply exactly NONE.";

/// Total attempts per page: one retry, so a transient 5xx does not cost a page
/// its caption. A 4xx other than 429 is the request's fault and is not retried.
const MAX_ATTEMPTS: u32 = 2;

/// Settings for [`HttpPageDescriber`]. Defaults live in `kb-server`.
#[derive(Debug, Clone)]
pub struct PageDescriberSettings {
    /// Full chat-completions endpoint.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub request_timeout: Duration,
    /// Upper bound on the reply (`KB_PAGE_CAPTION_MAX_TOKENS`).
    pub max_tokens: u32,
    /// Send `reasoning: {"enabled": false}`. Thinking models otherwise spend the
    /// whole token budget before writing the description.
    pub no_reasoning: bool,
}

/// What a reply means: a description, or nothing worth describing.
///
/// `NONE` (trimmed, case-insensitive) is `None`; anything else is the trimmed
/// description. The HTTP adapter never calls this with an empty reply: that is
/// an error there, not an answer. A model that says "NONE." gets `Some`, which
/// is the honest reading — the sentinel is exact.
#[must_use]
pub fn parse_description(raw: &str) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("NONE") {
        None
    } else {
        Some(text.to_string())
    }
}

/// Page describer over an OpenAI-compatible chat endpoint.
pub struct HttpPageDescriber {
    client: reqwest::Client,
    settings: PageDescriberSettings,
}

impl HttpPageDescriber {
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    pub fn new(settings: PageDescriberSettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest builder: {e}")))?;
        Ok(Self { client, settings })
    }

    /// The request body: the prompt, then the one page image.
    #[must_use]
    pub fn build_body(&self, page: &PageImage) -> serde_json::Value {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&page.bytes);
        let content = vec![
            json!({"type": "text", "text": PAGE_DESCRIBE_PROMPT}),
            json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{};base64,{encoded}", page.mime)},
            }),
        ];
        let mut body = json!({
            "model": self.settings.model,
            "messages": [{"role": "user", "content": content}],
            "temperature": 0,
            "max_tokens": self.settings.max_tokens,
        });
        if self.settings.no_reasoning {
            body["reasoning"] = json!({"enabled": false});
        }
        body
    }

    /// Whether a failed attempt is worth exactly one retry: transport errors,
    /// 429 and 5xx. A 4xx other than 429 is the request's fault.
    fn retryable_status(status: u16) -> bool {
        status == 429 || status >= 500
    }
}

#[async_trait]
impl PageDescriber for HttpPageDescriber {
    async fn describe(&self, page: &PageImage) -> Result<Option<String>, PortError> {
        let body = self.build_body(page);
        let mut last: Option<PortError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            let mut req = self.client.post(&self.settings.base_url).json(&body);
            if let Some(key) = &self.settings.api_key {
                req = req.bearer_auth(key);
            }
            match req.send().await {
                Err(e) => {
                    last = Some(PortError::Unavailable(format!(
                        "page describe request: {e}"
                    )));
                }
                Ok(res) => {
                    let status = res.status().as_u16();
                    let bytes = match res.bytes().await {
                        Ok(b) => b,
                        Err(e) => {
                            last = Some(PortError::Unavailable(format!("page describe body: {e}")));
                            continue;
                        }
                    };
                    if !(200..300).contains(&status) {
                        let snippet: String =
                            String::from_utf8_lossy(&bytes).chars().take(300).collect();
                        let err = PortError::Unavailable(format!(
                            "page describe http {status}: {snippet}"
                        ));
                        if !Self::retryable_status(status) {
                            return Err(err);
                        }
                        last = Some(err);
                    } else {
                        let reply: serde_json::Value =
                            serde_json::from_slice(&bytes).map_err(|e| {
                                PortError::Malformed(format!(
                                    "page describe: response is not JSON: {e}"
                                ))
                            })?;
                        let text = reply
                            .pointer("/choices/0/message/content")
                            .and_then(|c| c.as_str())
                            .unwrap_or_default();
                        // An empty reply is a failure, not "no visuals": a
                        // thinking model that spends `max_tokens` on reasoning
                        // returns no text (finish_reason "length"), and reading
                        // that as `None` once cached every page of ViDoRe HR as
                        // having nothing to describe. Only an explicit NONE is.
                        if text.trim().is_empty() {
                            let finish = reply
                                .pointer("/choices/0/finish_reason")
                                .and_then(|c| c.as_str())
                                .unwrap_or("unknown");
                            return Err(PortError::Malformed(format!(
                                "page describe: empty reply (finish_reason {finish}); \
                                 a thinking model may need KB_PAGE_CAPTION_NO_REASONING=true"
                            )));
                        }
                        return Ok(parse_description(text));
                    }
                }
            }
            if attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        Err(last
            .unwrap_or_else(|| PortError::Unavailable("page describe: no attempt was made".into())))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};

    use super::*;

    type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

    fn page(byte: u8) -> PageImage {
        PageImage {
            page: 3,
            mime: "image/jpeg".into(),
            bytes: vec![byte; 4],
            text: String::new(),
        }
    }

    fn settings(url: String, no_reasoning: bool) -> PageDescriberSettings {
        PageDescriberSettings {
            base_url: url,
            model: "qwen/qwen3.7-flash".into(),
            api_key: Some("k".into()),
            request_timeout: Duration::from_secs(5),
            max_tokens: 300,
            no_reasoning,
        }
    }

    /// Answers with `reply` and records every request body.
    async fn fake(reply: &'static str) -> (String, Seen) {
        async fn handle(
            State((seen, reply)): State<(Seen, &'static str)>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            seen.lock().unwrap().push(body);
            Json(json!({"choices": [{"message": {"content": reply}}]}))
        }
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route("/v1/chat/completions", post(handle))
            .with_state((Arc::clone(&seen), reply));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/v1/chat/completions"), seen)
    }

    /// Always fails with `status`.
    async fn failing(status: StatusCode) -> String {
        async fn handle(State(status): State<StatusCode>) -> StatusCode {
            status
        }
        let app = Router::new()
            .route("/v1/chat/completions", post(handle))
            .with_state(status);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/v1/chat/completions")
    }

    #[tokio::test]
    async fn the_request_is_one_image_plus_the_prompt() {
        let (url, seen) = fake("A bar chart of revenue by quarter.").await;
        let describer = HttpPageDescriber::new(settings(url, false)).unwrap();
        let description = describer.describe(&page(1)).await.unwrap();
        assert_eq!(
            description.as_deref(),
            Some("A bar chart of revenue by quarter.")
        );

        let body = seen.lock().unwrap()[0].clone();
        assert_eq!(body["model"], "qwen/qwen3.7-flash");
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["max_tokens"], 300);
        assert!(
            body.get("reasoning").is_none(),
            "reasoning is only sent when asked for: {body}"
        );
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "one prompt and one image: {content:?}");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], PAGE_DESCRIBE_PROMPT);
        assert_eq!(content[1]["type"], "image_url");
        assert!(
            content[1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
    }

    #[tokio::test]
    async fn the_reasoning_flag_and_max_tokens_follow_the_settings() {
        let (url, seen) = fake("A chart.").await;
        let mut cfg = settings(url, true);
        cfg.max_tokens = 123;
        let describer = HttpPageDescriber::new(cfg).unwrap();
        describer.describe(&page(1)).await.unwrap();
        let body = seen.lock().unwrap()[0].clone();
        assert_eq!(body["reasoning"], json!({"enabled": false}));
        assert_eq!(body["max_tokens"], 123);
    }

    #[tokio::test]
    async fn a_none_reply_is_no_description() {
        for (url, _) in [
            fake("NONE").await,
            fake(" none \n").await,
            fake("None").await,
        ] {
            let describer = HttpPageDescriber::new(settings(url, false)).unwrap();
            assert_eq!(describer.describe(&page(1)).await.unwrap(), None);
        }
    }

    /// A thinking model that runs out of tokens returns no text; that must be a
    /// visible failure (and never cached), not "this page has no visuals".
    #[tokio::test]
    async fn an_empty_reply_is_an_error_not_no_visuals() {
        for reply in ["", "  \n "] {
            let (url, _) = fake(reply).await;
            let describer = HttpPageDescriber::new(settings(url, false)).unwrap();
            let err = describer.describe(&page(1)).await.unwrap_err();
            assert!(err.to_string().contains("empty reply"), "{err}");
        }
    }

    #[test]
    fn a_none_sentinel_is_recognised_after_trimming_and_case_folding() {
        assert_eq!(parse_description("NONE"), None);
        assert_eq!(parse_description("  none  "), None);
        assert_eq!(parse_description(""), None);
        assert_eq!(parse_description("   \n "), None);
        assert_eq!(
            parse_description(" A pie chart. "),
            Some("A pie chart.".to_string())
        );
    }

    #[tokio::test]
    async fn an_http_error_is_an_error_not_an_empty_description() {
        let url = failing(StatusCode::BAD_REQUEST).await;
        let describer = HttpPageDescriber::new(settings(url, false)).unwrap();
        let err = describer.describe(&page(1)).await.unwrap_err();
        assert!(err.to_string().contains("http 400"), "{err}");
    }

    #[tokio::test]
    async fn a_server_error_is_retried_then_reported() {
        let url = failing(StatusCode::INTERNAL_SERVER_ERROR).await;
        let describer = HttpPageDescriber::new(settings(url, false)).unwrap();
        let err = describer.describe(&page(1)).await.unwrap_err();
        assert!(err.to_string().contains("http 500"), "{err}");
    }
}
