//! Vision reranking of page images over OpenAI-compatible chat completions.
//!
//! One request per query carries the query and the top page images as data
//! URLs, and asks a cheap vision model for a 0-10 relevance score per page as
//! JSON. The prompt and the request shape are the ones measured offline on
//! ViDoRe v3 HR (`kb-eval/scripts/vision_rerank.py`): qwen3.7-flash at top 20
//! lifted page nDCG@10 from 0.711 to 0.727 held-out
//! (`docs/eval-ab/vidore3-hr-en-image`).
//!
//! This adapter only asks and parses. Blending the model's order with the
//! current one, the deadline, and the visible degradation when anything fails
//! live in the engine; caching lives in `kb-cache`.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use kb_ports::{PageImage, PageVisionScorer, PortError};
use serde_json::json;

/// Default endpoint (`KB_PAGE_VISION_RERANK_BASE_URL`).
pub const DEFAULT_VISION_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Version of the prompt below. Part of the disk-cache key, so a prompt change
/// never reuses an old answer.
pub const VISION_PROMPT_VERSION: &str = "v1";

/// Total attempts per query: one retry, because this is on the search path.
const MAX_ATTEMPTS: u32 = 2;

/// Settings for [`HttpPageVisionScorer`]. Defaults live in `kb-server`.
#[derive(Debug, Clone)]
pub struct VisionScorerSettings {
    /// Full chat-completions endpoint.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub request_timeout: Duration,
    /// Send `reasoning: {"enabled": false}`. Thinking models such as
    /// qwen3.8-flash otherwise spend the whole token budget before writing the
    /// JSON (`docs/eval-ab/vidore3-hr-en-image`).
    pub no_reasoning: bool,
}

/// The instruction block, verbatim from `vision_rerank.py` with `{n}` filled.
#[must_use]
pub fn instructions(n: usize) -> String {
    format!(
        "You are ranking document pages for a search query. You are given the query and {n} page \
         images, numbered 1 to {n} in the order shown. Score each page 0-10 for how well it answers \
         the query (10 = directly answers it, including from a chart or table; 0 = unrelated). \
         Reply with JSON only: {{\"scores\": [s1, s2, ..., s{n}]}}."
    )
}

/// Vision scorer over an OpenAI-compatible chat endpoint.
pub struct HttpPageVisionScorer {
    client: reqwest::Client,
    settings: VisionScorerSettings,
}

impl HttpPageVisionScorer {
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    pub fn new(settings: VisionScorerSettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest builder: {e}")))?;
        Ok(Self { client, settings })
    }

    /// The request body, exactly as `vision_rerank.py` sends it.
    #[must_use]
    pub fn build_body(&self, query: &str, pages: &[PageImage]) -> serde_json::Value {
        let n = pages.len();
        let mut content = vec![json!({
            "type": "text",
            "text": format!("{}\n\nQuery: {query}", instructions(n)),
        })];
        for (i, page) in pages.iter().enumerate() {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&page.bytes);
            content.push(json!({"type": "text", "text": format!("Page {}:", i + 1)}));
            content.push(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{};base64,{encoded}", page.mime)},
            }));
        }
        let mut body = json!({
            "model": self.settings.model,
            "messages": [{"role": "user", "content": content}],
            "temperature": 0,
            "max_tokens": 300,
            "response_format": {"type": "json_object"},
            "usage": {"include": true},
        });
        if self.settings.no_reasoning {
            body["reasoning"] = json!({"enabled": false});
        }
        body
    }

    /// Pull the first `{...}` object out of a model reply and take `scores`.
    ///
    /// Greedy from the first `{` to the last `}`, as the Python script's
    /// `re.search(r"\{.*\}", text, re.S)`: a model that wraps the JSON in prose
    /// still answers.
    ///
    /// # Errors
    ///
    /// No JSON object, no `scores` array, a non-numeric score, or a count other
    /// than one per page.
    pub fn parse_scores(raw: &str, expected: usize) -> Result<Vec<f32>, PortError> {
        let start = raw
            .find('{')
            .ok_or_else(|| PortError::Malformed("vision rerank: no JSON object in reply".into()))?;
        let end = raw
            .rfind('}')
            .filter(|end| *end > start)
            .ok_or_else(|| PortError::Malformed("vision rerank: no JSON object in reply".into()))?;
        let value: serde_json::Value = serde_json::from_str(&raw[start..=end])
            .map_err(|e| PortError::Malformed(format!("vision rerank: reply is not JSON: {e}")))?;
        let scores = value
            .get("scores")
            .and_then(|s| s.as_array())
            .ok_or_else(|| PortError::Malformed("vision rerank: reply has no scores".into()))?;
        let parsed: Option<Vec<f32>> = scores
            .iter()
            .map(|v| v.as_f64().map(|n| n as f32))
            .collect();
        let parsed = parsed
            .ok_or_else(|| PortError::Malformed("vision rerank: score is not a number".into()))?;
        if parsed.len() != expected {
            return Err(PortError::Malformed(format!(
                "vision rerank: {} scores for {expected} pages",
                parsed.len()
            )));
        }
        Ok(parsed)
    }

    /// Whether a failed attempt is worth exactly one retry: transport errors,
    /// 429 and 5xx. A 4xx other than 429 is the request's fault and will fail
    /// the same way again. (Every error that reaches the retry sleep below is
    /// already one of the retryable ones; 4xx returns early.)
    fn retryable_status(status: u16) -> bool {
        status == 429 || status >= 500
    }
}

#[async_trait]
impl PageVisionScorer for HttpPageVisionScorer {
    async fn score(&self, query: &str, pages: &[PageImage]) -> Result<Vec<f32>, PortError> {
        if pages.is_empty() {
            return Ok(Vec::new());
        }
        let body = self.build_body(query, pages);
        let mut last: Option<PortError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            let mut req = self.client.post(&self.settings.base_url).json(&body);
            if let Some(key) = &self.settings.api_key {
                req = req.bearer_auth(key);
            }
            match req.send().await {
                Err(e) => {
                    last = Some(PortError::Unavailable(format!(
                        "vision rerank request: {e}"
                    )));
                }
                Ok(res) => {
                    let status = res.status().as_u16();
                    let bytes = match res.bytes().await {
                        Ok(b) => b,
                        Err(e) => {
                            last = Some(PortError::Unavailable(format!("vision rerank body: {e}")));
                            continue;
                        }
                    };
                    if !(200..300).contains(&status) {
                        let snippet: String =
                            String::from_utf8_lossy(&bytes).chars().take(300).collect();
                        let err = PortError::Unavailable(format!(
                            "vision rerank http {status}: {snippet}"
                        ));
                        if !Self::retryable_status(status) {
                            return Err(err);
                        }
                        last = Some(err);
                    } else {
                        let reply: serde_json::Value =
                            serde_json::from_slice(&bytes).map_err(|e| {
                                PortError::Malformed(format!(
                                    "vision rerank: response is not JSON: {e}"
                                ))
                            })?;
                        let text = reply
                            .pointer("/choices/0/message/content")
                            .and_then(|c| c.as_str())
                            .unwrap_or_default();
                        return Self::parse_scores(text, pages.len());
                    }
                }
            }
            if attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        Err(last
            .unwrap_or_else(|| PortError::Unavailable("vision rerank: no attempt was made".into())))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, extract::State, routing::post};

    use super::*;

    type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

    fn page(n: u32, byte: u8) -> PageImage {
        PageImage {
            page: n,
            mime: "image/jpeg".into(),
            bytes: vec![byte; 3],
            text: String::new(),
        }
    }

    fn settings(url: String, no_reasoning: bool) -> VisionScorerSettings {
        VisionScorerSettings {
            base_url: url,
            model: "qwen/qwen3.7-flash".into(),
            api_key: Some("k".into()),
            request_timeout: Duration::from_secs(5),
            no_reasoning,
        }
    }

    /// Answers with two 0-10 scores and records the request body.
    async fn fake() -> (String, Seen) {
        async fn handle(
            State(seen): State<Seen>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            seen.lock().unwrap().push(body);
            Json(json!({"choices": [{"message": {"content": "{\"scores\": [7, 3]}"}}]}))
        }
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route("/v1/chat/completions", post(handle))
            .with_state(Arc::clone(&seen));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/v1/chat/completions"), seen)
    }

    #[tokio::test]
    async fn the_request_is_the_measured_shape_with_images_in_order() {
        let (url, seen) = fake().await;
        let scorer = HttpPageVisionScorer::new(settings(url, false)).unwrap();
        let scores = scorer
            .score("what is the revenue?", &[page(0, 1), page(1, 2)])
            .await
            .unwrap();
        assert_eq!(scores, vec![7.0, 3.0]);

        let body = seen.lock().unwrap()[0].clone();
        assert_eq!(body["model"], "qwen/qwen3.7-flash");
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["max_tokens"], 300);
        assert_eq!(body["response_format"], json!({"type": "json_object"}));
        assert!(
            body.get("reasoning").is_none(),
            "reasoning is only sent when asked for: {body}"
        );
        let content = body["messages"][0]["content"].as_array().unwrap();
        // Text block (instructions + query), then label/image pairs in order.
        assert_eq!(content.len(), 5);
        assert_eq!(content[0]["type"], "text");
        let prompt = content[0]["text"].as_str().unwrap();
        assert!(prompt.contains("You are ranking document pages for a search query."));
        assert!(prompt.contains("Query: what is the revenue?"));
        assert!(prompt.contains("numbered 1 to 2 in the order shown"));
        assert_eq!(content[1], json!({"type": "text", "text": "Page 1:"}));
        assert_eq!(content[2]["type"], "image_url");
        assert!(
            content[2]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
        assert_eq!(content[3], json!({"type": "text", "text": "Page 2:"}));
        assert_ne!(
            content[2]["image_url"]["url"], content[4]["image_url"]["url"],
            "each page keeps its own bytes, in the order given"
        );
    }

    #[tokio::test]
    async fn the_reasoning_flag_is_sent_only_when_set() {
        let (url, seen) = fake().await;
        let scorer = HttpPageVisionScorer::new(settings(url, true)).unwrap();
        scorer.score("q", &[page(0, 1), page(1, 2)]).await.unwrap();
        let body = seen.lock().unwrap()[0].clone();
        assert_eq!(body["reasoning"], json!({"enabled": false}));
    }

    #[test]
    fn a_score_count_other_than_one_per_page_is_an_error() {
        let err = HttpPageVisionScorer::parse_scores(r#"{"scores": [5, 6]}"#, 3).unwrap_err();
        assert!(err.to_string().contains("2 scores for 3 pages"), "{err}");
        let err = HttpPageVisionScorer::parse_scores(r#"{"scores": []}"#, 1).unwrap_err();
        assert!(err.to_string().contains("0 scores for 1 pages"), "{err}");
    }

    #[test]
    fn json_wrapped_in_prose_is_parsed() {
        let scores = HttpPageVisionScorer::parse_scores(
            "Here is my ranking:\n{\"scores\": [10, 0.5, 3]}\nHope that helps.",
            3,
        )
        .unwrap();
        assert_eq!(scores, vec![10.0, 0.5, 3.0]);
    }

    #[test]
    fn a_reply_without_json_is_an_error_not_an_empty_ranking() {
        assert!(HttpPageVisionScorer::parse_scores("no json here", 2).is_err());
        assert!(HttpPageVisionScorer::parse_scores(r#"{"ranking": []}"#, 2).is_err());
    }
}
