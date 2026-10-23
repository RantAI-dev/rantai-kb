//! [`PageEmbedder`] over an OpenAI-compatible `/embeddings` endpoint that takes
//! images. Two request shapes share this adapter:
//!
//! * [`PageEmbedFormat::OpenRouter`] — OpenRouter's multimodal shape, measured
//!   with `voyageai/voyage-multimodal-3.5`
//!   (`docs/eval-ab/vidore3-hr-en-image`). A page goes in as `{"content":
//!   [{"type": "image_url", "image_url": {"url": "data:<mime>;base64,..."}}]}`,
//!   a query as a plain string, and `input_type` is `document` or `query` so
//!   models that embed the two sides differently (Voyage) are asked the right
//!   question.
//! * [`PageEmbedFormat::VllmChat`] — vLLM's Chat Embeddings API: a page is one
//!   conversation in `messages`, with the page text (when present) followed by
//!   the image, and vLLM returns one vector for the whole conversation. There
//!   is no `input_type`; the model's chat template carries the instruction.
//!   This is how the local tiers serve `Qwen/Qwen3-VL-Embedding-2B`
//!   (`docs/kb-tiers.md`).

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use futures::stream::{self, StreamExt, TryStreamExt};
use kb_ports::{PageEmbedder, PageImage, PortError};
use serde_json::json;
use tracing::warn;

use crate::embedder::{backoff, parse_openai_ordered};

const MAX_RETRIES: u32 = 4;

/// How a page or query is put on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageEmbedFormat {
    /// OpenRouter multimodal `/embeddings` (`input` + `input_type`).
    #[default]
    OpenRouter,
    /// vLLM Chat Embeddings (`messages`, one page per request, no `input_type`).
    VllmChat,
}

/// Settings for [`HttpPageEmbedder`]. Defaults live in `kb-server`.
#[derive(Debug, Clone)]
pub struct PageEmbedderSettings {
    /// Full endpoint, e.g. `https://openrouter.ai/api/v1/embeddings`.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub dimension: u32,
    /// Request shape (`KB_PAGE_EMBEDDING_FORMAT`).
    pub format: PageEmbedFormat,
    /// Images per request. Ignored by [`PageEmbedFormat::VllmChat`], which
    /// sends one page per request (`concurrency` still runs them in parallel).
    pub batch_size: u32,
    /// Requests in flight per `embed_pages` call.
    pub concurrency: u32,
    pub request_timeout: Duration,
}

/// HTTP page embedder.
pub struct HttpPageEmbedder {
    client: reqwest::Client,
    settings: PageEmbedderSettings,
}

impl HttpPageEmbedder {
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    pub fn new(settings: PageEmbedderSettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest builder: {e}")))?;
        Ok(Self { client, settings })
    }

    async fn post(
        &self,
        body: &serde_json::Value,
        expected: usize,
    ) -> Result<Vec<Vec<f32>>, PortError> {
        let body =
            serde_json::to_vec(body).map_err(|e| PortError::Malformed(format!("req: {e}")))?;
        let mut last: Option<PortError> = None;
        for attempt in 1..=MAX_RETRIES {
            let mut req = self
                .client
                .post(&self.settings.base_url)
                .header("content-type", "application/json")
                .body(body.clone());
            if let Some(key) = &self.settings.api_key {
                req = req.bearer_auth(key);
            }
            let res = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    last = Some(PortError::Unavailable(format!("page embed: {e}")));
                    backoff(attempt).await;
                    continue;
                }
            };
            let status = res.status().as_u16();
            let bytes = match res.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    last = Some(PortError::Unavailable(format!("page embed body: {e}")));
                    backoff(attempt).await;
                    continue;
                }
            };
            if !(200..300).contains(&status) {
                let msg: String = String::from_utf8_lossy(&bytes).chars().take(300).collect();
                if (status >= 500 || status == 429) && attempt < MAX_RETRIES {
                    warn!(
                        attempt,
                        status, "page embedder transient error; backing off"
                    );
                    last = Some(PortError::Unavailable(format!("http {status}: {msg}")));
                    backoff(attempt).await;
                    continue;
                }
                return Err(PortError::Unavailable(format!(
                    "page embed http {status}: {msg}"
                )));
            }
            let vecs = parse_openai_ordered(&bytes, expected)?;
            let want = self.settings.dimension as usize;
            if let Some(bad) = vecs.iter().find(|v| v.len() != want) {
                return Err(PortError::Malformed(format!(
                    "page embedder returned a {}-d vector, expected {want}",
                    bad.len()
                )));
            }
            return Ok(vecs);
        }
        Err(last.unwrap_or_else(|| PortError::Unavailable("page embed failed".into())))
    }
}

fn page_content(page: &PageImage) -> Vec<serde_json::Value> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&page.bytes);
    let image = json!({"type": "image_url", "image_url": {"url": format!("data:{};base64,{b64}", page.mime)}});
    if page.text.trim().is_empty() {
        vec![image]
    } else {
        vec![json!({"type": "text", "text": page.text}), image]
    }
}

fn openrouter_page_input(page: &PageImage) -> serde_json::Value {
    json!({ "content": page_content(page) })
}

impl HttpPageEmbedder {
    /// Page request bodies and how many vectors each must return.
    fn page_bodies(&self, pages: &[PageImage]) -> Vec<(serde_json::Value, usize)> {
        match self.settings.format {
            PageEmbedFormat::OpenRouter => {
                let batch = self.settings.batch_size.max(1) as usize;
                pages
                    .chunks(batch)
                    .map(|chunk| {
                        (
                            json!({
                                "model": self.settings.model,
                                "input": chunk.iter().map(openrouter_page_input).collect::<Vec<_>>(),
                                "input_type": "document",
                            }),
                            chunk.len(),
                        )
                    })
                    .collect()
            }
            PageEmbedFormat::VllmChat => pages
                .iter()
                .map(|page| {
                    (
                        json!({
                            "model": self.settings.model,
                            "messages": [{"role": "user", "content": page_content(page)}],
                            "encoding_format": "float",
                        }),
                        1,
                    )
                })
                .collect(),
        }
    }

    fn query_body(&self, text: &str) -> serde_json::Value {
        match self.settings.format {
            PageEmbedFormat::OpenRouter => {
                json!({"model": self.settings.model, "input": [text], "input_type": "query"})
            }
            PageEmbedFormat::VllmChat => json!({
                "model": self.settings.model,
                "messages": [{"role": "user", "content": [{"type": "text", "text": text}]}],
                "encoding_format": "float",
            }),
        }
    }
}

#[async_trait]
impl PageEmbedder for HttpPageEmbedder {
    async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        let bodies = self.page_bodies(pages);
        let results: Vec<Vec<Vec<f32>>> = stream::iter(bodies)
            .map(|(body, n)| async move { self.post(&body, n).await })
            .buffered(self.settings.concurrency.max(1) as usize)
            .try_collect()
            .await?;
        Ok(results.into_iter().flatten().collect())
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        self.post(&self.query_body(text), 1)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| PortError::Malformed("page embedder: empty response".into()))
    }

    fn dimension(&self) -> u32 {
        self.settings.dimension
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, extract::State, routing::post};

    use super::*;

    type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

    /// Echoes one vector per input: `[tag, is_image, 0]`, where `tag` is the
    /// data-URL length (so it grows with the image and orders pages) and
    /// `is_image` says whether the content carried an image part. Handles both
    /// request shapes: `input` (OpenRouter) and `messages` (vLLM chat).
    async fn fake() -> (String, Seen) {
        async fn handle(
            State(seen): State<Seen>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            let contents: Vec<Vec<serde_json::Value>> =
                if let Some(inputs) = body["input"].as_array() {
                    inputs
                        .iter()
                        .map(|inp| {
                            inp.get("content")
                                .and_then(|c| c.as_array())
                                .cloned()
                                .unwrap_or_default()
                        })
                        .collect()
                } else {
                    body.pointer("/messages/0/content")
                        .and_then(|c| c.as_array())
                        .cloned()
                        .into_iter()
                        .collect()
                };
            let data: Vec<serde_json::Value> = contents
                .iter()
                .enumerate()
                .map(|(i, parts)| {
                    let is_image =
                        f32::from(u8::from(parts.iter().any(|p| p["type"] == "image_url")));
                    let tag = parts
                        .iter()
                        .find_map(|p| p.pointer("/image_url/url").and_then(|u| u.as_str()))
                        .map_or(0.0, |u| u.len() as f32);
                    json!({"index": i, "embedding": [tag, is_image, 0.0]})
                })
                .collect();
            seen.lock().unwrap().push(body);
            Json(json!({"data": data}))
        }
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route("/v1/embeddings", post(handle))
            .with_state(Arc::clone(&seen));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/v1/embeddings"), seen)
    }

    fn settings(url: String, dim: u32) -> PageEmbedderSettings {
        PageEmbedderSettings {
            base_url: url,
            model: "voyageai/voyage-multimodal-3.5".into(),
            api_key: Some("k".into()),
            dimension: dim,
            format: PageEmbedFormat::OpenRouter,
            batch_size: 2,
            concurrency: 2,
            request_timeout: Duration::from_secs(5),
        }
    }

    fn vllm_settings(url: String, dim: u32) -> PageEmbedderSettings {
        PageEmbedderSettings {
            base_url: url,
            model: "Qwen/Qwen3-VL-Embedding-2B".into(),
            api_key: None,
            dimension: dim,
            format: PageEmbedFormat::VllmChat,
            batch_size: 2,
            concurrency: 2,
            request_timeout: Duration::from_secs(5),
        }
    }

    fn page(n: u32, len: usize) -> PageImage {
        PageImage {
            page: n,
            mime: "image/jpeg".into(),
            bytes: vec![7; len],
            text: String::new(),
        }
    }

    #[tokio::test]
    async fn pages_go_as_image_documents_in_order_across_batches() {
        let (url, seen) = fake().await;
        let e = HttpPageEmbedder::new(settings(url, 3)).unwrap();
        let pages: Vec<PageImage> = (0..5).map(|i| page(i, 3 * (i as usize + 1))).collect();
        let v = e.embed_pages(&pages).await.unwrap();
        assert_eq!(v.len(), 5);
        // The data URL grows with the image, so the first component orders them.
        assert!(v.windows(2).all(|w| w[0][0] < w[1][0]), "{v:?}");
        assert!(v.iter().all(|x| x[1] == 1.0));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "batches of 2");
        assert!(seen.iter().all(|b| b["input_type"] == "document"));
        let url = seen[0]["input"][0]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
    }

    #[tokio::test]
    async fn a_page_with_text_goes_as_text_then_image() {
        let (url, seen) = fake().await;
        let e = HttpPageEmbedder::new(settings(url, 3)).unwrap();
        let mut p = page(0, 3);
        p.text = "Table 2: wages by country".into();
        e.embed_pages(&[p, page(1, 3)]).await.unwrap();
        let seen = seen.lock().unwrap();
        let first = &seen[0]["input"][0]["content"];
        assert_eq!(first[0]["type"], "text");
        assert_eq!(first[0]["text"], "Table 2: wages by country");
        assert_eq!(first[1]["type"], "image_url");
        assert_eq!(
            seen[0]["input"][1]["content"].as_array().unwrap().len(),
            1,
            "no text, image only"
        );
    }

    #[tokio::test]
    async fn a_query_goes_as_text_with_the_query_input_type() {
        let (url, seen) = fake().await;
        let e = HttpPageEmbedder::new(settings(url, 3)).unwrap();
        let v = e.embed_query("wages in 2024").await.unwrap();
        assert_eq!(v[1], 0.0);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0]["input_type"], "query");
        assert_eq!(seen[0]["input"][0], "wages in 2024");
    }

    #[tokio::test]
    async fn a_wrong_dimension_is_an_error() {
        let (url, _) = fake().await;
        let e = HttpPageEmbedder::new(settings(url, 1024)).unwrap();
        assert!(e.embed_query("x").await.is_err());
    }

    #[tokio::test]
    async fn vllm_pages_go_one_message_request_each_in_order() {
        let (url, seen) = fake().await;
        let e = HttpPageEmbedder::new(vllm_settings(url, 3)).unwrap();
        // `batch_size` is 2, but the vLLM shape cannot batch conversations.
        let pages: Vec<PageImage> = (0..5).map(|i| page(i, 3 * (i as usize + 1))).collect();
        let v = e.embed_pages(&pages).await.unwrap();
        assert_eq!(v.len(), 5);
        // The data URL grows with the image, so the first component orders them.
        assert!(v.windows(2).all(|w| w[0][0] < w[1][0]), "{v:?}");
        assert!(v.iter().all(|x| x[1] == 1.0));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 5, "one request per page");
        for body in seen.iter() {
            assert!(body.get("input").is_none());
            assert!(body.get("input_type").is_none());
            assert_eq!(body["encoding_format"], "float");
            assert_eq!(body["messages"].as_array().unwrap().len(), 1);
            assert_eq!(body["messages"][0]["role"], "user");
            assert_eq!(
                body["messages"][0]["content"].as_array().unwrap().len(),
                1,
                "no page text, image only"
            );
            assert_eq!(body["messages"][0]["content"][0]["type"], "image_url");
            assert!(
                body["messages"][0]["content"][0]["image_url"]["url"]
                    .as_str()
                    .unwrap()
                    .starts_with("data:image/jpeg;base64,")
            );
        }
    }

    #[tokio::test]
    async fn vllm_a_page_with_text_puts_text_then_image_in_one_message() {
        let (url, seen) = fake().await;
        let e = HttpPageEmbedder::new(vllm_settings(url, 3)).unwrap();
        let mut p = page(0, 3);
        p.text = "Table 2: wages by country".into();
        e.embed_pages(&[p, page(1, 3)]).await.unwrap();
        let seen = seen.lock().unwrap();
        let with_text = seen
            .iter()
            .find(|b| b.pointer("/messages/0/content/0/type") == Some(&json!("text")))
            .expect("the page with text");
        let content = with_text["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["text"], "Table 2: wages by country");
        assert_eq!(content[1]["type"], "image_url");
        let image_only = seen
            .iter()
            .find(|b| b["messages"][0]["content"].as_array().unwrap().len() == 1)
            .expect("the page without text");
        assert_eq!(image_only["messages"][0]["content"][0]["type"], "image_url");
    }

    #[tokio::test]
    async fn vllm_a_query_is_one_text_message_without_input_type() {
        let (url, seen) = fake().await;
        let e = HttpPageEmbedder::new(vllm_settings(url, 3)).unwrap();
        let v = e.embed_query("wages in 2024").await.unwrap();
        assert_eq!(v[1], 0.0);
        let seen = seen.lock().unwrap();
        assert!(seen[0].get("input").is_none());
        assert!(seen[0].get("input_type").is_none());
        assert_eq!(seen[0]["encoding_format"], "float");
        assert_eq!(
            seen[0]["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "wages in 2024"}]}])
        );
    }

    #[tokio::test]
    async fn vllm_a_wrong_dimension_is_an_error() {
        let (url, _) = fake().await;
        let e = HttpPageEmbedder::new(vllm_settings(url, 1024)).unwrap();
        assert!(e.embed_query("x").await.is_err());
        assert!(e.embed_pages(&[page(0, 3)]).await.is_err());
    }
}
