//! [`PageEmbedder`] over an OpenAI-compatible `/embeddings` endpoint that takes
//! images: OpenRouter's multimodal shape, measured with
//! `voyageai/voyage-multimodal-3.5` (`docs/eval-ab/vidore3-hr-en-image`).
//!
//! A page goes in as `{"content": [{"type": "image_url", "image_url": {"url":
//! "data:<mime>;base64,..."}}]}`, a query as a plain string, and
//! `input_type` is `document` or `query` so models that embed the two sides
//! differently (Voyage) are asked the right question.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use futures::stream::{self, StreamExt, TryStreamExt};
use kb_ports::{PageEmbedder, PageImage, PortError};
use serde_json::json;
use tracing::warn;

use crate::embedder::{backoff, parse_openai_ordered};

const MAX_RETRIES: u32 = 4;

/// Settings for [`HttpPageEmbedder`]. Defaults live in `kb-server`.
#[derive(Debug, Clone)]
pub struct PageEmbedderSettings {
    /// Full endpoint, e.g. `https://openrouter.ai/api/v1/embeddings`.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub dimension: u32,
    /// Images per request.
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

fn image_input(page: &PageImage) -> serde_json::Value {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&page.bytes);
    json!({"content": [{"type": "image_url", "image_url": {"url": format!("data:{};base64,{b64}", page.mime)}}]})
}

#[async_trait]
impl PageEmbedder for HttpPageEmbedder {
    async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        let batch = self.settings.batch_size.max(1) as usize;
        // Owned request bodies, so the stream holds no borrow of `pages`.
        let bodies: Vec<(serde_json::Value, usize)> = pages
            .chunks(batch)
            .map(|chunk| {
                (
                    json!({
                        "model": self.settings.model,
                        "input": chunk.iter().map(image_input).collect::<Vec<_>>(),
                        "input_type": "document",
                    }),
                    chunk.len(),
                )
            })
            .collect();
        let results: Vec<Vec<Vec<f32>>> = stream::iter(bodies)
            .map(|(body, n)| async move { self.post(&body, n).await })
            .buffered(self.settings.concurrency.max(1) as usize)
            .try_collect()
            .await?;
        Ok(results.into_iter().flatten().collect())
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        let body = json!({"model": self.settings.model, "input": [text], "input_type": "query"});
        self.post(&body, 1)
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

    /// Echoes one vector per input: `[n, is_image, 0]` where n is the input's
    /// position across all requests, so order and kind can be checked.
    async fn fake() -> (String, Seen) {
        async fn handle(
            State(seen): State<Seen>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            let inputs = body["input"].as_array().cloned().unwrap_or_default();
            let data: Vec<serde_json::Value> = inputs
                .iter()
                .enumerate()
                .map(|(i, inp)| {
                    let is_image = f32::from(u8::from(inp.get("content").is_some()));
                    let tag = inp
                        .pointer("/content/0/image_url/url")
                        .and_then(|u| u.as_str())
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
}
