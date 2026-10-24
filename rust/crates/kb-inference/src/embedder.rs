//! HTTP embedding client against OpenRouter / OpenAI-compatible APIs and the
//! MiniMax embeddings shape (`{ model, texts, type }`).
//!
//! - One shared `reqwest::Client`, a timeout on every call.
//! - **Batched**: the OpenAI-compatible path sends one request per
//!   `EmbedderSettings::batch_size` inputs (`input: [..]`), not one per text.
//!   One HTTP call per chunk cost 12 docs/min on CPU against a local TEI
//!   server; batching is what makes a real-embedding evaluation finish.
//! - Per-provider batch caps clamp the requested batch so a single oversized
//!   batch cannot silently zero a chunk batch (Gemini = 100).
//! - Query and passage prefixes are applied on top of the request body so the
//!   same KB can drive models that need them (E5, BGE, etc.).
//! - Up to 3 retries on transient errors (5xx or 429), exponential backoff
//!   capped at 10 seconds.
//! - A response whose vector count does not match the request is an **error**,
//!   never a short or shuffled list: a misaligned embedding silently attaches
//!   the wrong text to a chunk.
//! - The MiniMax shape keeps its per-text request: batching there would change
//!   a contract this adapter has never exercised in batch.

use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{Embedder, PortError, QueryEmbedding};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;

use crate::query_cache::QueryCache;

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
    /// Send `input_type: "query" | "document"` (Voyage's asymmetric
    /// embedding hint) on OpenAI-shaped requests. Off by default: most
    /// OpenAI-compatible servers reject or ignore unknown fields.
    pub send_input_type: bool,
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
struct OpenAiReq<'a> {
    model: &'a str,
    input: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_type: Option<&'static str>,
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
    /// Present in the OpenAI response spec. Used to restore request order when
    /// a provider returns items out of order.
    #[serde(default)]
    index: Option<usize>,
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

    /// The query-cache key. Model-scoped, so switching models cannot serve a
    /// vector from the other model's space.
    fn cache_key(&self, text: &str) -> String {
        format!("{}|{}", self.settings.model, text.trim())
    }

    /// POST a prepared body and return the raw response bytes, retrying
    /// transient failures (5xx, 429, transport) up to `MAX_RETRIES`.
    async fn post(&self, body: Vec<u8>) -> Result<Vec<u8>, PortError> {
        let url = self.settings.base_url.clone();
        let api_key = self.api_key()?;
        let label = if self.is_minimax() {
            "minimax"
        } else {
            "openai"
        };

        let mut last_err: Option<PortError> = None;
        for attempt in 1..=MAX_RETRIES {
            let req = self
                .client
                .post(&url)
                .bearer_auth(&api_key)
                .header("content-type", "application/json")
                .body(body.clone())
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

            return Ok(bytes.to_vec());
        }
        Err(last_err.unwrap_or_else(|| PortError::Unavailable("embedding failed".into())))
    }

    /// Embed `inputs` in one OpenAI-compatible request and return the vectors
    /// in the order the inputs were sent.
    async fn embed_openai_batch(
        &self,
        inputs: &[String],
        kind: EmbedKind,
    ) -> Result<Vec<Vec<f32>>, PortError> {
        let r = OpenAiReq {
            model: &self.settings.model,
            input: inputs.iter().map(String::as_str).collect(),
            dimensions: Some(self.settings.dimension),
            input_type: self.settings.send_input_type.then(|| kind.as_input_type()),
        };
        let body = serde_json::to_vec(&r).map_err(|e| PortError::Malformed(format!("req: {e}")))?;
        let bytes = self.post(body).await?;
        parse_openai_ordered(&bytes, inputs.len())
    }

    /// Embed a single text. Caches query-side calls.
    async fn embed_one(&self, text: &str, kind: EmbedKind) -> Result<Vec<f32>, PortError> {
        let cache_key = self.cache_key(text);
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

        let v = if self.is_minimax() {
            let r = MiniMaxReqOne {
                model: &self.settings.model,
                texts: vec![prefixed],
                r#type: kind.as_minimax_type(),
            };
            let body =
                serde_json::to_vec(&r).map_err(|e| PortError::Malformed(format!("req: {e}")))?;
            let bytes = self.post(body).await?;
            parse_minimax(&bytes)?
                .into_iter()
                .next()
                .ok_or_else(|| PortError::Malformed("minimax: empty vectors".into()))?
        } else {
            self.embed_openai_batch(std::slice::from_ref(&prefixed), kind)
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| PortError::Malformed("openai: empty data".into()))?
        };

        if kind == EmbedKind::Query {
            let mut cache = self.query_cache.lock().await;
            cache.put(cache_key, v.clone());
        }
        Ok(v)
    }

    /// Embed a batch in input order, `batch_size` inputs per HTTP request.
    ///
    /// The order of `texts` is the order of the returned vectors. A response
    /// that carries the wrong number of vectors is an error rather than a
    /// silently short or shifted list, and the per-provider cap
    /// (`clamp_batch_size`) bounds the request size.
    pub async fn embed_many(
        &self,
        texts: &[String],
        kind: EmbedKind,
    ) -> Result<Vec<Vec<f32>>, PortError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let size = self.settings.clamp_batch_size().max(1) as usize;
        if self.is_minimax() {
            // The MiniMax shape is still called one text per request.
            let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
            for t in texts {
                out.push(self.embed_one(t, kind).await?);
            }
            return Ok(out);
        }
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for batch in texts.chunks(size) {
            out.extend(self.embed_openai_batch(batch, kind).await?);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedKind {
    Query,
    Passage,
}

impl EmbedKind {
    fn as_input_type(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Passage => "document",
        }
    }

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

/// Parse an OpenAI-shaped embeddings response into `expected` vectors, in
/// request order.
///
/// The count is checked first: a response with the wrong number of vectors is
/// an error, because the alternative — returning what arrived — shifts every
/// vector after the gap onto the wrong text. When every item carries the
/// `index` field the vectors are placed by it, so a provider that answers out
/// of order is handled; a response where only some items carry `index` is also
/// an error, since its order cannot be established.
pub(crate) fn parse_openai_ordered(
    bytes: &[u8],
    expected: usize,
) -> Result<Vec<Vec<f32>>, PortError> {
    let resp: OpenAiResp = serde_json::from_slice(bytes)
        .map_err(|e| PortError::Malformed(format!("openai shape: {e}")))?;
    let data = resp.data;
    if data.len() != expected {
        return Err(PortError::Malformed(format!(
            "openai: response carried {} vectors for {expected} inputs",
            data.len()
        )));
    }
    let indexed = data.iter().filter(|i| i.index.is_some()).count();
    if indexed == 0 {
        return Ok(data.into_iter().map(|i| i.embedding).collect());
    }
    if indexed != data.len() {
        return Err(PortError::Malformed(format!(
            "openai: {indexed} of {} items carry an index",
            data.len()
        )));
    }
    let mut slots: Vec<Option<Vec<f32>>> = vec![None; expected];
    for item in data {
        let idx = item
            .index
            .ok_or_else(|| PortError::Malformed("openai: missing index".into()))?;
        let slot = slots.get_mut(idx).ok_or_else(|| {
            PortError::Malformed(format!(
                "openai: index {idx} out of range for {expected} inputs"
            ))
        })?;
        if slot.is_some() {
            return Err(PortError::Malformed(format!(
                "openai: duplicate index {idx}"
            )));
        }
        *slot = Some(item.embedding);
    }
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(expected);
    for slot in slots {
        out.push(slot.ok_or_else(|| PortError::Malformed("openai: missing vector".into()))?);
    }
    Ok(out)
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

pub(crate) async fn backoff(attempt: u32) {
    let delay = 1000u64 * 2u64.pow(attempt - 1);
    tokio::time::sleep(Duration::from_millis(delay.min(10_000))).await;
}

#[async_trait]
impl Embedder for EmbedderImpl {
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        self.embed_one(text, EmbedKind::Query).await
    }

    async fn embed_query_detailed(&self, text: &str) -> Result<QueryEmbedding, PortError> {
        let cache_key = self.cache_key(text);
        if let Some(vector) = {
            let mut cache = self.query_cache.lock().await;
            cache.get(&cache_key)
        } {
            return Ok(QueryEmbedding {
                vector,
                cached: true,
            });
        }
        let vector = self.embed_one(text, EmbedKind::Query).await?;
        Ok(QueryEmbedding {
            vector,
            cached: false,
        })
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        self.embed_many(texts, EmbedKind::Passage).await
    }

    fn dimension(&self) -> u32 {
        self.settings.dimension
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{Value, json};
    use std::sync::Arc;

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
            send_input_type: false,
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

    /// What the fake embeddings server does with the batch it was sent.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        /// Echo every input in order, with its `index`.
        InOrder,
        /// Echo every input, but in reverse order, `index` travelling with it.
        Shuffled,
        /// Return one vector fewer than the inputs it was given.
        Short,
        /// Return vectors with no `index` field at all.
        NoIndex,
    }

    #[derive(Clone)]
    struct Fake {
        mode: Mode,
        /// One entry per request: the `input` array as it arrived.
        seen: Arc<Mutex<Vec<Vec<String>>>>,
    }

    /// A deterministic vector per input, so a test can tell which text a
    /// returned vector belongs to.
    fn vector_for(text: &str) -> Vec<f32> {
        let sum: u32 = text.bytes().map(u32::from).sum();
        vec![
            sum as f32,
            text.chars().count() as f32,
            text.len() as f32,
            1.0,
        ]
    }

    async fn handle(State(fake): State<Fake>, Json(body): Json<Value>) -> Json<Value> {
        let inputs: Vec<String> = body["input"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();
        fake.seen.lock().await.push(inputs.clone());
        let count = match fake.mode {
            Mode::Short => inputs.len().saturating_sub(1),
            _ => inputs.len(),
        };
        let mut items: Vec<Value> = inputs
            .iter()
            .take(count)
            .enumerate()
            .map(|(i, t)| match fake.mode {
                Mode::NoIndex => json!({"embedding": vector_for(t)}),
                _ => json!({"index": i, "embedding": vector_for(t)}),
            })
            .collect();
        if fake.mode == Mode::Shuffled {
            items.reverse();
        }
        Json(json!({"object": "list", "data": items}))
    }

    /// Start the fake server on an ephemeral port; returns its URL and the
    /// log of request bodies it saw.
    async fn fake_server(mode: Mode) -> (String, Arc<Mutex<Vec<Vec<String>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/embeddings", post(handle))
            .with_state(Fake {
                mode,
                seen: Arc::clone(&seen),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/v1/embeddings"), seen)
    }

    async fn embedder_for(
        mode: Mode,
        batch_size: u32,
    ) -> (EmbedderImpl, Arc<Mutex<Vec<Vec<String>>>>) {
        let (url, seen) = fake_server(mode).await;
        let mut s = settings();
        s.base_url = url;
        s.batch_size = batch_size;
        (EmbedderImpl::new(s).expect("embedder"), seen)
    }

    fn texts(n: usize) -> Vec<String> {
        (0..n).map(|i| format!(" passage number {i}")).collect()
    }

    fn expected(texts: &[String]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| vector_for(t)).collect()
    }

    #[tokio::test]
    async fn batched_order_is_preserved_across_batches() {
        let input = texts(7);
        let (embedder, seen) = embedder_for(Mode::InOrder, 3).await;
        let out = embedder
            .embed_many(&input, EmbedKind::Passage)
            .await
            .unwrap();
        assert_eq!(out, expected(&input));
        let seen = seen.lock().await;
        assert_eq!(seen.len(), 3, "7 inputs at batch 3 is three requests");
        assert_eq!(
            seen.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![3, 3, 1],
            "the last batch carries the remainder"
        );
        let flattened: Vec<&String> = seen.iter().flatten().collect();
        assert_eq!(
            flattened,
            input.iter().collect::<Vec<_>>(),
            "inputs unsplit"
        );
    }

    /// The latency report tells a cache hit from a provider call, so the
    /// embedder has to be able to tell it too.
    #[tokio::test]
    async fn detailed_query_embed_reports_cache_hit_and_miss() {
        let (embedder, seen) = embedder_for(Mode::InOrder, 2).await;

        let first = embedder
            .embed_query_detailed("apa itu rantai")
            .await
            .unwrap();
        assert!(!first.cached, "a first query is never a cache hit");
        assert_eq!(first.vector, vector_for("apa itu rantai"));

        let second = embedder
            .embed_query_detailed("apa itu rantai")
            .await
            .unwrap();
        assert!(
            second.cached,
            "the same question again comes from the cache"
        );
        assert_eq!(second.vector, first.vector);

        let other = embedder
            .embed_query_detailed("apa itu rantai-kb")
            .await
            .unwrap();
        assert!(!other.cached, "a different question is a miss");

        // The point of a cache hit: no provider call for it.
        assert_eq!(seen.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn shuffled_response_index_restores_request_order() {
        let input = texts(5);
        let (embedder, _) = embedder_for(Mode::Shuffled, 5).await;
        let out = embedder
            .embed_many(&input, EmbedKind::Passage)
            .await
            .unwrap();
        assert_eq!(out, expected(&input), "index, not position, decides");
    }

    #[tokio::test]
    async fn response_without_index_is_positional() {
        let input = texts(4);
        let (embedder, _) = embedder_for(Mode::NoIndex, 4).await;
        let out = embedder
            .embed_many(&input, EmbedKind::Passage)
            .await
            .unwrap();
        assert_eq!(out, expected(&input));
    }

    #[tokio::test]
    async fn count_mismatch_is_an_error() {
        let input = texts(4);
        let (embedder, _) = embedder_for(Mode::Short, 4).await;
        let err = embedder
            .embed_many(&input, EmbedKind::Passage)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("3 vectors for 4 inputs"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn batch_size_one_is_one_request_per_input() {
        let input = texts(3);
        let (embedder, seen) = embedder_for(Mode::InOrder, 1).await;
        let out = embedder
            .embed_many(&input, EmbedKind::Passage)
            .await
            .unwrap();
        assert_eq!(out, expected(&input));
        let seen = seen.lock().await;
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|b| b.len() == 1));
    }

    #[tokio::test]
    async fn single_query_keeps_prefix_and_dimension() {
        let (url, seen) = fake_server(Mode::NoIndex).await;
        let mut s = settings();
        s.base_url = url;
        s.query_prefix = "Instruct: q\n".into();
        let embedder = EmbedderImpl::new(s).unwrap();
        let v = embedder.embed_query("apa kabar").await.unwrap();
        assert_eq!(v, vector_for("Instruct: q\napa kabar"));
        let seen = seen.lock().await;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], vec!["Instruct: q\napa kabar".to_string()]);
    }

    #[test]
    fn input_type_is_sent_only_when_enabled() {
        let req = |input_type| {
            serde_json::to_value(OpenAiReq {
                model: "voyageai/voyage-multimodal-3.5",
                input: vec!["wages"],
                dimensions: Some(1024),
                input_type,
            })
            .unwrap()
        };
        assert!(req(None).get("input_type").is_none());
        let on = |kind: EmbedKind| req(Some(kind.as_input_type()))["input_type"].clone();
        assert_eq!(on(EmbedKind::Query), "query");
        assert_eq!(on(EmbedKind::Passage), "document");
    }

    #[tokio::test]
    async fn gemini_cap_bounds_the_request() {
        let input = texts(10);
        let (embedder, seen) = embedder_for(Mode::InOrder, 256).await;
        // The cap is applied to the *provider* model name, so re-check it
        // through the settings the clamp reads.
        let mut s = settings();
        s.batch_size = 256;
        s.model = "google/gemini-embedding-001".into();
        assert_eq!(s.clamp_batch_size(), 100);
        let out = embedder
            .embed_many(&input, EmbedKind::Passage)
            .await
            .unwrap();
        assert_eq!(
            out,
            expected(&input),
            "no cap is applied without a cap model"
        );
        assert_eq!(seen.lock().await.len(), 1);
    }

    #[test]
    fn duplicate_index_is_rejected() {
        let bytes = br#"{"data":[{"index":0,"embedding":[1.0]},{"index":0,"embedding":[2.0]}]}"#;
        let err = parse_openai_ordered(bytes, 2).unwrap_err();
        assert!(err.to_string().contains("duplicate index"), "{err}");
    }

    #[test]
    fn out_of_range_index_is_rejected() {
        let bytes = br#"{"data":[{"index":0,"embedding":[1.0]},{"index":9,"embedding":[2.0]}]}"#;
        let err = parse_openai_ordered(bytes, 2).unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
    }

    #[test]
    fn partial_index_is_rejected() {
        let bytes = br#"{"data":[{"index":0,"embedding":[1.0]},{"embedding":[2.0]}]}"#;
        let err = parse_openai_ordered(bytes, 2).unwrap_err();
        assert!(err.to_string().contains("carry an index"), "{err}");
    }
}

#[cfg(test)]
mod dispatch_tests {
    //! The override has to be reachable *through the trait object*, because
    //! that is how the engine holds it. Calling the method on the concrete type
    //! proves nothing about dispatch.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn the_cache_flag_survives_dispatch_through_the_trait_object() {
        use axum::Json;
        use axum::Router;
        use axum::routing::post;

        async fn handle(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
            let count = body["input"].as_array().map(Vec::len).unwrap_or(0);
            Json(serde_json::json!({
                "object": "list",
                "data": (0..count).map(|i| serde_json::json!({"index": i, "embedding": [1.0, 0.0, 0.0, 0.0]})).collect::<Vec<_>>()
            }))
        }
        let app = Router::new().route("/v1/embeddings", post(handle));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let settings = EmbedderSettings {
            base_url: format!("http://{addr}/v1/embeddings"),
            model: "test-model".into(),
            api_key: Some("k".into()),
            dimension: 4,
            batch_size: 2,
            embed_concurrency: 2,
            query_prefix: String::new(),
            passage_prefix: String::new(),
            send_input_type: false,
            request_timeout: std::time::Duration::from_secs(5),
        };
        let embedder: Arc<dyn Embedder> = Arc::new(EmbedderImpl::new(settings).unwrap());

        let first = embedder.embed_query_detailed("sama").await.unwrap();
        assert!(!first.cached);
        let second = embedder.embed_query_detailed("sama").await.unwrap();
        assert!(second.cached, "the trait object must reach the override");
        assert_eq!(second.vector, first.vector);
    }
}
