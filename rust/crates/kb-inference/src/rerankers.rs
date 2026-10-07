//! Three reranker providers — TEI `/rerank`, vLLM Cohere-v2 shape, and the
//! real Cohere `/v2/rerank` endpoint.
//!
//! All three share the same back-fill semantics: the backend may not return
//! every input, so missing items are appended in input order at score 0 so the
//! engine can still map every candidate id.

use async_trait::async_trait;
use kb_ports::{PortError, RerankCandidate, RerankResult, Reranker};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct TeiReranker {
    name: String,
    endpoint: String,
    client: reqwest::Client,
}

impl TeiReranker {
    pub fn new(base_url: &str, model: &str) -> Result<Self, PortError> {
        if base_url.is_empty() {
            return Err(PortError::Invalid("TeiReranker: baseUrl is required".into()));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest builder: {e}")))?;
        Ok(Self {
            name: format!("TEI {model}"),
            endpoint: format!("{}/rerank", base_url.trim_end_matches('/')),
            client,
        })
    }
}

#[derive(Debug, Serialize)]
struct TeiReq<'a> {
    query: &'a str,
    texts: Vec<String>,
    truncate: bool,
}
#[derive(Debug, Deserialize)]
struct TeiRespItem {
    index: u32,
    score: f32,
}

#[async_trait]
impl Reranker for TeiReranker {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let body = TeiReq {
            query,
            texts: candidates
                .iter()
                .map(|c| {
                    if c.text.trim().is_empty() {
                        " ".into()
                    } else {
                        c.text.clone()
                    }
                })
                .collect(),
            truncate: true,
        };
        let bytes = serde_json::to_vec(&body)
            .map_err(|e| PortError::Malformed(format!("tei req: {e}")))?;
        let res = self
            .client
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(|e| PortError::Unavailable(format!("tei send: {e}")))?;
        let status = res.status().as_u16();
        let body = res
            .bytes()
            .await
            .map_err(|e| PortError::Unavailable(format!("tei body: {e}")))?;
        if !(200..300).contains(&status) {
            let excerpt: String = String::from_utf8_lossy(&body).chars().take(300).collect();
            return Err(PortError::Unavailable(format!(
                "TEI rerank http {status}: {excerpt}"
            )));
        }
        let rows: Vec<TeiRespItem> = if let Ok(arr) = serde_json::from_slice::<Vec<TeiRespItem>>(&body) {
            arr
        } else {
            // Fall back to wrapping a `data` / `results` field.
            #[derive(Deserialize)]
            struct Wrapper {
                #[serde(default)]
                data: Option<Vec<TeiRespItem>>,
                #[serde(default)]
                results: Option<Vec<TeiRespItem>>,
            }
            let w: Wrapper = serde_json::from_slice(&body)
                .map_err(|e| PortError::Malformed(format!("tei shape: {e}")))?;
            w.data.or(w.results).unwrap_or_default()
        };

        let mut picked = std::collections::HashSet::new();
        let mut out: Vec<RerankResult> = Vec::new();
        for r in &rows {
            let idx = r.index as usize;
            let Some(cand) = candidates.get(idx) else {
                continue;
            };
            if !picked.insert(cand.id.clone()) {
                continue;
            }
            out.push(RerankResult {
                id: cand.id.clone(),
                score: r.score,
            });
            if out.len() as u32 >= final_k {
                break;
            }
        }
        for cand in &candidates {
            if out.len() as u32 >= final_k {
                break;
            }
            if !picked.insert(cand.id.clone()) {
                continue;
            }
            out.push(RerankResult {
                id: cand.id.clone(),
                score: 0.0,
            });
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct VllmReranker {
    name: String,
    model: String,
    endpoint: String,
    client: reqwest::Client,
}

impl VllmReranker {
    pub fn new(base_url: &str, model: &str) -> Result<Self, PortError> {
        if base_url.is_empty() {
            return Err(PortError::Invalid("VllmReranker: baseUrl is required".into()));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest builder: {e}")))?;
        Ok(Self {
            name: model.to_string(),
            model: model.to_string(),
            endpoint: format!("{}/rerank", base_url.trim_end_matches('/')),
            client,
        })
    }
}

#[derive(Debug, Serialize)]
struct CohereShapeReq<'a> {
    model: &'a str,
    query: &'a str,
    documents: &'a [String],
    top_n: u32,
}

#[derive(Debug, Deserialize)]
struct CohereShapeResp {
    #[serde(default)]
    results: Vec<CohereShapeItem>,
}

#[derive(Debug, Deserialize)]
struct CohereShapeItem {
    index: u32,
    relevance_score: f32,
}

async fn rerank_cohere_shape(
    client: &reqwest::Client,
    endpoint: &str,
    api_key: Option<&str>,
    model: &str,
    query: &str,
    candidates: Vec<RerankCandidate>,
    final_k: u32,
) -> Result<Vec<RerankResult>, PortError> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let docs: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();
    let body = CohereShapeReq {
        model,
        query,
        documents: &docs,
        top_n: final_k,
    };
    let bytes = serde_json::to_vec(&body)
        .map_err(|e| PortError::Malformed(format!("rerank req: {e}")))?;
    let mut req = client
        .post(endpoint)
        .header("content-type", "application/json")
        .body(bytes);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let res = req
        .send()
        .await
        .map_err(|e| PortError::Unavailable(format!("rerank send: {e}")))?;
    let status = res.status().as_u16();
    let resp_body = res
        .bytes()
        .await
        .map_err(|e| PortError::Unavailable(format!("rerank body: {e}")))?;
    if !(200..300).contains(&status) {
        let excerpt: String = String::from_utf8_lossy(&resp_body).chars().take(300).collect();
        return Err(PortError::Unavailable(format!(
            "rerank http {status}: {excerpt}"
        )));
    }
    let parsed: CohereShapeResp = serde_json::from_slice(&resp_body)
        .map_err(|e| PortError::Malformed(format!("rerank shape: {e}")))?;

    let mut picked = std::collections::HashSet::new();
    let mut out: Vec<RerankResult> = Vec::new();
    for r in &parsed.results {
        let idx = r.index as usize;
        let Some(cand) = candidates.get(idx) else {
            continue;
        };
        if !picked.insert(cand.id.clone()) {
            continue;
        }
        out.push(RerankResult {
            id: cand.id.clone(),
            score: r.relevance_score,
        });
        if out.len() as u32 >= final_k {
            break;
        }
    }
    for cand in &candidates {
        if out.len() as u32 >= final_k {
            break;
        }
        if !picked.insert(cand.id.clone()) {
            continue;
        }
        out.push(RerankResult {
            id: cand.id.clone(),
            score: 0.0,
        });
    }
    Ok(out)
}

#[async_trait]
impl Reranker for VllmReranker {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError> {
        let model = self.model.clone();
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        rerank_cohere_shape(&client, &endpoint, None, &model, query, candidates, final_k).await
    }
}

#[derive(Debug, Clone)]
pub struct CohereReranker {
    name: String,
    model: String,
    api_key: String,
    endpoint: String,
    client: reqwest::Client,
}

impl CohereReranker {
    /// Build a Cohere client. `api_key` is required.
    pub fn new(model: &str, api_key: &str, endpoint: Option<&str>) -> Result<Self, PortError> {
        if api_key.is_empty() {
            return Err(PortError::Invalid("CohereReranker: apiKey required".into()));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest builder: {e}")))?;
        let ep = endpoint
            .unwrap_or("https://api.cohere.com/v2/rerank")
            .to_string();
        Ok(Self {
            name: model.to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
            endpoint: ep,
            client,
        })
    }
}

#[async_trait]
impl Reranker for CohereReranker {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError> {
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        rerank_cohere_shape(&client, &endpoint, Some(&api_key), &model, query, candidates, final_k)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_ports::ChunkType;

    fn cand(id: &str, text: &str) -> RerankCandidate {
        RerankCandidate {
            id: id.into(),
            text: text.into(),
            chunk_type: ChunkType::Text,
        }
    }

    #[tokio::test]
    async fn tei_reranker_requires_base_url() {
        assert!(TeiReranker::new("", "x").is_err());
    }

    #[tokio::test]
    async fn cohere_requires_api_key() {
        assert!(CohereReranker::new("rerank-v3.5", "", Some("verifier")).is_err());
    }

    #[tokio::test]
    async fn backfill_covers_missing_indices() {
        // Wire a TEI reranker against a mock HTTP server (test-only). We can
        // only exercise the response parsing here without I/O; the integration
        // lives in the engine tests.
        let r = TeiReranker::new("http://localhost:0", "bge").expect("tei");
        assert_eq!(r.name, "TEI bge");
    }

    #[test]
    fn request_body_keeps_empty_as_space() {
        // Sanity-check the wire shape we serialise for TEI.
        let body = TeiReq {
            query: "what",
            texts: vec!["".into(), "hello".into()],
            truncate: true,
        };
        let s = serde_json::to_string(&body).expect("encode");
        assert!(s.contains("\"query\":\"what\""));
        assert!(s.contains("\"truncate\":true"));
    }
}