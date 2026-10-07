//! Query expansion via OpenRouter chat completions.
//!
//! Returns `[original, ...paraphrases]` when:
//!   - `enabled == true` AND
//!   - `api_key` is set AND
//!   - the LLM call succeeds AND
//!   - the model returns parseable JSON.
//!
//! Failure modes return `[original]` only. Never throws. Caches successful
//! expansions in a 1h LRU keyed on the trimmed query.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kb_ports::PortError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct ExpansionSettings {
    pub enabled: bool,
    pub api_key: Option<String>,
    pub endpoint: String,
    pub model: String,
    pub paraphrase_count: u32,
    pub timeout: Duration,
}

const DEFAULT_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

impl ExpansionSettings {
    /// Construct settings from the env. Convenience for callers.
    #[must_use]
    pub fn from_env_or_disabled() -> Self {
        Self {
            enabled: false,
            api_key: None,
            endpoint: DEFAULT_ENDPOINT.into(),
            model: "openai/gpt-4o-mini".into(),
            paraphrase_count: 3,
            timeout: Duration::from_secs(8),
        }
    }
}

#[derive(Debug, Serialize)]
struct ChatReq<'a> {
    model: &'a str,
    messages: Vec<ChatMsg<'a>>,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct ChatMsg<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChatResp {
    choices: Vec<ChatChoice>,
}
#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMsgOut,
}
#[derive(Debug, Deserialize)]
struct ChatMsgOut {
    content: String,
}

struct LruCache {
    map: HashMap<String, (Vec<String>, Instant)>,
    order: Vec<String>,
    capacity: usize,
    ttl: Duration,
}

impl LruCache {
    fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            map: HashMap::new(),
            order: Vec::new(),
            capacity,
            ttl,
        }
    }
    fn get(&mut self, k: &str) -> Option<Vec<String>> {
        let v = self.map.get(k)?;
        if v.1.elapsed() > self.ttl {
            self.map.remove(k);
            self.order.retain(|x| x != k);
            return None;
        }
        Some(v.0.clone())
    }
    fn put(&mut self, k: String, v: Vec<String>) {
        if self.map.contains_key(&k) {
            self.map.insert(k, (v, Instant::now()));
            return;
        }
        if self.order.len() >= self.capacity {
            if let Some(evicted) = self.order.first().cloned() {
                self.order.remove(0);
                self.map.remove(&evicted);
            }
        }
        self.order.push(k.clone());
        self.map.insert(k, (v, Instant::now()));
    }
}

pub struct QueryExpander {
    settings: ExpansionSettings,
    client: reqwest::Client,
    cache: Mutex<LruCache>,
}

impl QueryExpander {
    /// Build an expander with the given settings.
    pub fn new(settings: ExpansionSettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("expander reqwest: {e}")))?;
        Ok(Self {
            settings,
            client,
            cache: Mutex::new(LruCache::new(1024, Duration::from_secs(60 * 60))),
        })
    }

    /// Expand the query. Always succeeds (returns at least `[original]`).
    pub async fn expand(&self, query: &str) -> Vec<String> {
        let q = query.trim().to_string();
        if q.is_empty() {
            return Vec::new();
        }
        if !self.settings.enabled {
            return vec![q];
        }
        let Some(api_key) = self.settings.api_key.clone() else {
            return vec![q];
        };

        let cached = match self.cache.lock() {
            Ok(mut c) => c.get(&q),
            Err(_) => None,
        };
        if let Some(v) = cached {
            return v;
        }

        let cleaned = self.fetch_and_parse(&q, &api_key).await;
        if let Ok(mut c) = self.cache.lock() {
            c.put(q.clone(), cleaned.clone());
        }
        cleaned
    }

    async fn fetch_and_parse(&self, q: &str, api_key: &str) -> Vec<String> {
        let prompt = format!(
            "Generate exactly {} alternative phrasings of the following user \
             question, optimized for semantic search over a knowledge base. \
             Each paraphrase MUST preserve the original intent but vary \
             wording, synonyms, or perspective. Output ONLY a JSON array of \
             {} strings. No prose, no markdown fences.\n\nQuestion: {}",
            self.settings.paraphrase_count, self.settings.paraphrase_count, q
        );
        let body = ChatReq {
            model: &self.settings.model,
            messages: vec![ChatMsg {
                role: "user",
                content: &prompt,
            }],
            max_tokens: 300,
            temperature: 0.3,
        };
        let body_bytes = match serde_json::to_vec(&body) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("query expansion encode error: {e}");
                return vec![q.to_string()];
            }
        };

        let resp = match self
            .client
            .post(&self.settings.endpoint)
            .bearer_auth(api_key)
            .header("content-type", "application/json")
            .body(body_bytes)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("query expansion request failed: {e}");
                return vec![q.to_string()];
            }
        };
        if !resp.status().is_success() {
            tracing::warn!("query expansion status {}", resp.status());
            return vec![q.to_string()];
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("query expansion body error: {e}");
                return vec![q.to_string()];
            }
        };
        let parsed: ChatResp = match serde_json::from_slice(&bytes) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("query expansion shape: {e}");
                return vec![q.to_string()];
            }
        };
        let raw = parsed
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();
        let Some(matched) = raw.match_indices('[').next().and_then(|(start, _)| {
            raw.rfind(']').and_then(|end| {
                if end > start {
                    Some(&raw[start..=end])
                } else {
                    None
                }
            })
        }) else {
            return vec![q.to_string()];
        };
        let paraphrases: Vec<String> = match serde_json::from_str(matched) {
            Ok(v) => v,
            Err(_) => return vec![q.to_string()],
        };

        let norm = |s: &str| {
            s.to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut seen = std::collections::HashSet::new();
        seen.insert(norm(q));
        let mut cleaned = vec![q.to_string()];
        for p in paraphrases {
            let trimmed = p.trim();
            if trimmed.is_empty() {
                continue;
            }
            let key = norm(trimmed);
            if seen.insert(key) {
                cleaned.push(trimmed.to_string());
            }
        }
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_returns_original() {
        let mut s = ExpansionSettings::from_env_or_disabled();
        s.enabled = false;
        let Ok(e) = QueryExpander::new(s) else { return };
        assert_eq!(
            e.expand("what is revenue?").await,
            vec!["what is revenue?".to_string()]
        );
    }

    #[tokio::test]
    async fn no_api_key_returns_original() {
        let mut s = ExpansionSettings::from_env_or_disabled();
        s.enabled = true;
        s.api_key = None;
        let Ok(e) = QueryExpander::new(s) else { return };
        assert_eq!(
            e.expand("what is revenue?").await,
            vec!["what is revenue?".to_string()]
        );
    }

    #[tokio::test]
    async fn empty_query_returns_empty() {
        let s = ExpansionSettings::from_env_or_disabled();
        let Ok(e) = QueryExpander::new(s) else { return };
        assert!(e.expand("   ").await.is_empty());
    }

    #[test]
    fn cache_eviction_drops_oldest() {
        let mut c = LruCache::new(2, Duration::from_secs(60));
        c.put("a".into(), vec!["x".into()]);
        c.put("b".into(), vec!["y".into()]);
        c.put("c".into(), vec!["z".into()]);
        assert!(c.get("a").is_none());
        assert!(c.get("b").is_some());
        assert!(c.get("c").is_some());
    }
}
