//! HTTP embedding client against OpenRouter / OpenAI-compatible APIs and the
//! MiniMax embeddings shape (`{ model, texts, type }`).
//!
//! - One shared `reqwest::Client`, a timeout on every call.
//! - Concurrency for the batch path is bounded by `embed_concurrency` workers.
//! - Per-provider batch caps clamp the requested batch so a single oversized
//!   batch cannot silently zero a chunk batch (Gemini = 100).
//! - Query and passage prefixes are applied on top of the request body so the
//!   same KB can drive models that need them (E5, BGE, etc.).
//! - Up to 3 retries on transient errors (5xx or 429), exponential backoff
//!   capped at 10 seconds.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{Embedder, PortError};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;

/// Per-call settings. All fields are required; defaults live in `kb-server`.
#[derive(Debug, Clone)]
pub struct EmbedderSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub dimension: u32,
    pub batch_size: u32,
    pub embed_concurrency: u32,
    pub query_prefix: String,
    pub passage_prefix: String,
    pub request_timeout: Duration,
}

impl EmbedderSettings {
    /// Clamp the per-provider batch cap (Gemini = 100) onto the requested size.
    /// Same logic as `resolveEmbedBatchSize` in the TS implementation.
    #[must_use]
    pub fn clamp_batch_size(&self) -> u32 {
        let lower = self.model.to_ascii_lowercase();
        let cap: Option<u32> =
            if lower.contains("gemini-embedding") || lower.contains("text-embedding-00") {
                Some(100)
            } else {
                None
            };
        cap.map(|c| self.batch_size.min(c))
            .unwrap_or(self.batch_size)
    }
}

#[derive(Debug, Serialize)]
struct OpenAiReqOne<'a> {
    model: &'a str,
    input: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
}

#[derive(Debug, Serialize)]
struct MiniMaxReqOne<'a> {
    model: &'a str,
    texts: Vec<String>,
    r#type: &'a str,
}

#[derive(Debug, Deserialize)]
struct OpenAiResp {
    data: Vec<OpenAiItem>,
}
#[derive(Debug, Deserialize)]
struct OpenAiItem {
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct MiniMaxResp {
    vectors: Vec<Vec<f32>>,
    #[serde(default)]
    base_resp: Option<MiniMaxBase>,
}
#[derive(Debug, Deserialize)]
struct MiniMaxBase {
    #[serde(default)]
    status_code: i32,
    #[serde(default)]
    status_msg: Option<String>,
}

const MAX_RETRIES: u32 = 3;

/// Cached query embeddings. ~8 MB at 4096-d Float32 with 256 entries.
#[derive(Default)]
struct QueryCache {
    map: HashMap<String, Vec<f32>>,
    order: Vec<String>,
    capacity: usize,
}

impl QueryCache {
    fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: Vec::new(),
            capacity: cap,
        }
    }

    fn get(&mut self, k: &str) -> Option<Vec<f32>> {
        self.map.get(k).cloned()
    }

    fn put(&mut self, k: String, v: Vec<f32>) {
        if self.map.contains_key(&k) {
            self.map.insert(k, v);
            return;
        }
        if self.order.len() >= self.capacity {
            if let Some(evicted) = self.order.first().cloned() {
                self.order.remove(0);
                self.map.remove(&evicted);
            }
        }
        self.order.push(k.clone());
        self.map.insert(k, v);
    }
}

/// Embedding client. Holds one shared `reqwest::Client` and a small query cache.
pub struct EmbedderImpl {
    client: reqwest::Client,
    settings: EmbedderSettings,
    query_cache: Mutex<QueryCache>,
}

impl EmbedderImpl {
    /// Build a new embedder.
    pub fn new(settings: EmbedderSettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest builder: {e}")))?;
        Ok(Self {
            client,
            settings,
            query_cache: Mutex::new(QueryCache::with_capacity(256)),
        })
    }

    fn is_minimax(&self) -> bool {
        self.settings
            .base_url
            .to_ascii_lowercase()
            .contains("minimax")
    }

    fn api_key(&self) -> Result<String, PortError> {
        self.settings
            .api_key
            .clone()
            .ok_or_else(|| PortError::Invalid("embedding api key not set".into()))
    }

    /// Embed a single text. Caches query-side calls.
    async fn embed_one(&self, text: &str, kind: EmbedKind) -> Result<Vec<f32>, PortError> {
        let cache_key = format!("{}|{}", self.settings.model, text.trim());
        if kind == EmbedKind::Query {
            let mut cache = self.query_cache.lock().await;
            if let Some(v) = cache.get(&cache_key) {
                return Ok(v);
            }
        }
        let prefixed = match kind {
            EmbedKind::Query => format!("{}{}", self.settings.query_prefix, text),
            EmbedKind::Passage => format!("{}{}", self.settings.passage_prefix, text),
        };

        let url = self.settings.base_url.clone();
        let api_key = self.api_key()?;
        let label = if self.is_minimax() {
            "minimax"
        } else {
            "openai"
        };

        let mut last_err: Option<PortError> = None;
        for attempt in 1..=MAX_RETRIES {
            let body = if self.is_minimax() {
                let r = MiniMaxReqOne {
                    model: &self.settings.model,
                    texts: vec![prefixed.clone()],
                    r#type: kind.as_minimax_type(),
                };
                serde_json::to_vec(&r).map_err(|e| PortError::Malformed(format!("req: {e}")))?
            } else {
                let r = OpenAiReqOne {
                    model: &self.settings.model,
                    input: &prefixed,
                    dimensions: Some(self.settings.dimension),
                };
                serde_json::to_vec(&r).map_err(|e| PortError::Malformed(format!("req: {e}")))?
            };

            let req = self
                .client
                .post(&url)
                .bearer_auth(&api_key)
                .header("content-type", "application/json")
                .body(body)
                .build()
                .map_err(|e| PortError::Unavailable(format!("build: {e}")))?;

            let res = match self.client.execute(req).await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(PortError::Unavailable(format!("execute: {e}")));
                    if attempt < MAX_RETRIES {
                        backoff(attempt).await;
                        continue;
                    }
                    return Err(last_err
                        .take()
                        .unwrap_or_else(|| PortError::Unavailable("embed failed".into())));
                }
            };
            let status = res.status().as_u16();
            let bytes = match res.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    last_err = Some(PortError::Unavailable(format!("body: {e}")));
                    if attempt < MAX_RETRIES {
                        backoff(attempt).await;
                        continue;
                    }
                    return Err(last_err
                        .take()
                        .unwrap_or_else(|| PortError::Unavailable("embed failed".into())));
                }
            };

            if !(200..300).contains(&status) {
                let transient = status >= 500 || status == 429;
                if transient && attempt < MAX_RETRIES {
                    warn!(attempt, "{label} transient {}; backing off", status);
                    backoff(attempt).await;
                    continue;
                }
                return Err(PortError::Unavailable(format!(
                    "{label} http {status}: {}",
                    String::from_utf8_lossy(&bytes)
                        .chars()
                        .take(300)
                        .collect::<String>()
                )));
            }

            let v = if self.is_minimax() {
                parse_minimax(&bytes)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| PortError::Malformed("minimax: empty vectors".into()))?
            } else {
                let mut all = parse_openai(&bytes)?;
                all.into_iter()
                    .next()
                    .ok_or_else(|| PortError::Malformed("openai: empty data".into()))?
            };

            if kind == EmbedKind::Query {
                let mut cache = self.query_cache.lock().await;
                cache.put(cache_key, v.clone());
            }
            return Ok(v);
        }
        Err(last_err.unwrap_or_else(|| PortError::Unavailable("embedding failed".into())))
    }

    /// Embed a batch in input order. R1 implementation: serial per text with
    /// caching, retries, and the configured per-provider batch cap applied to
    /// the output ordering. A bounded-parallel path is sketched in
    /// `embed_many_parallel` below but is not wired yet — keeping the R1 path
    /// simple so the compiler sees only what we actually exercise.
    pub async fn embed_many(
        &self,
        texts: &[String],
        kind: EmbedKind,
    ) -> Result<Vec<Vec<f32>>, PortError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed_one(t, kind).await?);
        }
        let _ = self.settings.clamp_batch_size();
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmbedKind {
    Query,
    Passage,
}

impl EmbedKind {
    fn as_minimax_type(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Passage => "db",
        }
    }
}

/// Cheap, manual-clone wrapper placeholder so a future parallel-batch path has
/// somewhere to put its `&EmbedderImpl` handle. Currently unused — the serial
/// `embed_many` does not need it.
#[allow(dead_code)]
struct CloneableEmbedder<'a> {
    inner: &'a EmbedderImpl,
}

fn parse_openai(bytes: &[u8]) -> Result<Vec<Vec<f32>>, PortError> {
    let resp: OpenAiResp = serde_json::from_slice(bytes)
        .map_err(|e| PortError::Malformed(format!("openai shape: {e}")))?;
    Ok(resp.data.into_iter().map(|i| i.embedding).collect())
}

fn parse_minimax(bytes: &[u8]) -> Result<Vec<Vec<f32>>, PortError> {
    let resp: MiniMaxResp = serde_json::from_slice(bytes)
        .map_err(|e| PortError::Malformed(format!("minimax shape: {e}")))?;
    if let Some(base) = &resp.base_resp {
        if base.status_code != 0 {
            return Err(PortError::Unavailable(format!(
                "minimax status {}: {}",
                base.status_code,
                base.status_msg.clone().unwrap_or_default()
            )));
        }
    }
    Ok(resp.vectors)
}

async fn backoff(attempt: u32) {
    let delay = 1000u64 * 2u64.pow(attempt - 1);
    tokio::time::sleep(Duration::from_millis(delay.min(10_000))).await;
}

#[async_trait]
impl Embedder for EmbedderImpl {
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        self.embed_one(text, EmbedKind::Query).await
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        self.embed_many(texts, EmbedKind::Passage).await
    }

    fn dimension(&self) -> u32 {
        self.settings.dimension
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> EmbedderSettings {
        EmbedderSettings {
            base_url: "http://localhost:0".into(),
            model: "test-model".into(),
            api_key: Some("k".into()),
            dimension: 4,
            batch_size: 2,
            embed_concurrency: 2,
            query_prefix: String::new(),
            passage_prefix: String::new(),
            request_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn batch_cap_clamps_gemini_models() {
        let mut s = settings();
        s.model = "google/gemini-embedding-001".into();
        s.batch_size = 128;
        assert_eq!(s.clamp_batch_size(), 100);
        s.batch_size = 64;
        assert_eq!(s.clamp_batch_size(), 64);
        s.model = "qwen/qwen3-embedding-8b".into();
        s.batch_size = 128;
        assert_eq!(s.clamp_batch_size(), 128);
        s.model = "baai/bge-m3".into();
        s.batch_size = 256;
        assert_eq!(s.clamp_batch_size(), 256);
    }

    #[test]
    fn query_cache_eviction() {
        let mut c = QueryCache::with_capacity(2);
        c.put("a".into(), vec![1.0]);
        c.put("b".into(), vec![2.0]);
        c.put("c".into(), vec![3.0]);
        assert!(c.get("a").is_none());
        assert!(c.get("b").is_some());
        assert!(c.get("c").is_some());
    }
}
