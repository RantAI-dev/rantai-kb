//! Vision-language model figure gate veto client.
//!
//! Port of `src/lib/rag/figure-gate.ts`. Evaluates candidate figures one by one
//! using an OpenAI-compatible vision completion endpoint.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use kb_core::{GateConfig, PROMPT_STRICT, parse_verdict};
use kb_ports::{BlobStore, FigureGate, FigureGateCandidate, PortError};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Vision-language model figure gate implementation.
pub struct VlmFigureGate {
    config: GateConfig,
    blob_store: Arc<dyn BlobStore>,
    client: reqwest::Client,
    endpoint: String,
}

impl VlmFigureGate {
    /// Create a new VLM figure gate.
    #[must_use]
    pub fn new(config: GateConfig, blob_store: Arc<dyn BlobStore>) -> Self {
        let endpoint = format!("{}/chat/completions", config.base.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            blob_store,
            client,
            endpoint,
        }
    }

    /// Judge a single image crop against the query. Returns false on any failure.
    pub async fn judge(&self, query: &str, image_base64: &str) -> bool {
        let prompt = PROMPT_STRICT.replace("{Q}", query);
        let request_body = ChatCompletionRequest {
            model: &self.config.model,
            max_tokens: 8,
            messages: vec![ChatMessage {
                role: "user",
                content: vec![
                    ChatContent::ImageUrl {
                        image_url: ImageUrl {
                            url: format!("data:image/png;base64,{image_base64}"),
                        },
                    },
                    ChatContent::Text { text: &prompt },
                ],
            }],
        };

        let res = match self
            .client
            .post(&self.endpoint)
            .json(&request_body)
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => res,
            Ok(res) => {
                warn!("vlm figure gate call returned status {}", res.status());
                return false;
            }
            Err(e) => {
                warn!("vlm figure gate call failed: {e}");
                return false;
            }
        };

        let data: ChatCompletionResponse = match res.json().await {
            Ok(data) => data,
            Err(e) => {
                warn!("vlm figure gate body parse failed: {e}");
                return false;
            }
        };

        let reply = data
            .choices
            .and_then(|mut c| c.pop())
            .and_then(|c| c.message)
            .and_then(|m| m.content)
            .unwrap_or_default();

        parse_verdict(&reply)
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<ChatMessage<'a>>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: Vec<ChatContent<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ChatContent<'a> {
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
    #[serde(rename = "text")]
    Text { text: &'a str },
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Option<Vec<ChatChoice>>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChoiceMessage>,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[async_trait]
impl FigureGate for VlmFigureGate {
    async fn gate_figures(
        &self,
        query: &str,
        candidates: &[FigureGateCandidate],
    ) -> Result<Vec<String>, PortError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let seen: Vec<&FigureGateCandidate> =
            candidates.iter().take(self.config.top_n as usize).collect();

        let mut kept: Vec<String> = Vec::new();
        for c in seen {
            let bytes = match self.blob_store.download(&c.asset_key).await {
                Ok(b) => b,
                Err(e) => {
                    warn!("[RAG] figure gate: fetch failed for {}: {e}", c.asset_key);
                    continue;
                }
            };
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            if self.judge(query, &b64).await {
                kept.push(c.id.clone());
                if kept.len() >= self.config.max_keep as usize {
                    break;
                }
            }
        }

        Ok(kept)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct TestBlobStore(Mutex<Vec<u8>>);

    #[async_trait]
    impl BlobStore for TestBlobStore {
        async fn upload(
            &self,
            _key: &str,
            _body: Vec<u8>,
            _content_type: &str,
        ) -> Result<u64, PortError> {
            Ok(0)
        }
        async fn download(&self, _key: &str) -> Result<Vec<u8>, PortError> {
            Ok(self.0.lock().unwrap().clone())
        }
        async fn delete(&self, _key: &str) -> Result<(), PortError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn gate_figures_vetos_candidates_via_mock_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = socket.read(&mut buf).await;
                    let body = r#"{"choices":[{"message":{"content":"YA"}}]}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                });
            }
        });

        let config = GateConfig {
            base: format!("http://127.0.0.1:{port}"),
            model: "test-vlm".to_string(),
            top_n: 2,
            max_keep: 1,
            timeout_ms: 2000,
        };

        let blob_store = Arc::new(TestBlobStore(Mutex::new(b"fake-image-bytes".to_vec())));
        let gate = VlmFigureGate::new(config, blob_store);

        let candidates = vec![
            FigureGateCandidate {
                id: "fig-1".to_string(),
                asset_key: "assets/fig1.png".to_string(),
                caption: "Figure 1 diagram".to_string(),
            },
            FigureGateCandidate {
                id: "fig-2".to_string(),
                asset_key: "assets/fig2.png".to_string(),
                caption: "Figure 2 chart".to_string(),
            },
        ];

        let kept = gate
            .gate_figures("What is the water cycle?", &candidates)
            .await
            .unwrap();
        assert_eq!(kept, vec!["fig-1"]);
    }
}
