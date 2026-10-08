//! LLM-as-reranker over OpenRouter chat completions.
//!
//! Port of `src/lib/rag/rerankers/llm-reranker.ts`. This is the reranker the
//! TypeScript service uses when `KB_RERANK_PROVIDER` is `openrouter` or unset.
//! It sends numbered passages to a chat model and reads back a JSON array of
//! indices. Missing or unusable indices are filled from the original order, so
//! the caller always gets `final_k` items when there are enough candidates.

use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{ChunkId, PortError, RerankCandidate, RerankResult, Reranker};
use serde::{Deserialize, Serialize};

/// Chat-completions endpoint used by the TypeScript reranker.
pub const OPENROUTER_CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
/// Characters of each passage shown to the model (matches the TS constant).
pub const CANDIDATE_TEXT_LIMIT: usize = 400;

/// LLM reranker with an OpenRouter key.
#[derive(Debug, Clone)]
pub struct LlmReranker {
    model: String,
    api_key: String,
    endpoint: String,
    client: reqwest::Client,
}

impl LlmReranker {
    /// Build a reranker. The key must be set; the TS code throws without it.
    pub fn new(model: &str, api_key: &str) -> Result<Self, PortError> {
        if api_key.trim().is_empty() {
            return Err(PortError::Invalid("OPENROUTER_API_KEY is not set".into()));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest builder: {e}")))?;
        Ok(Self {
            model: model.to_string(),
            api_key: api_key.to_string(),
            endpoint: OPENROUTER_CHAT_URL.to_string(),
            client,
        })
    }

    /// Build the numbered prompt. Public for tests.
    #[must_use]
    pub fn build_prompt(query: &str, candidates: &[RerankCandidate], final_k: u32) -> String {
        let numbered = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let text: String = c
                    .text
                    .chars()
                    .take(CANDIDATE_TEXT_LIMIT)
                    .map(|ch| if ch == '\n' { ' ' } else { ch })
                    .collect();
                format!("[{i}] {text}")
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        format!(
            "You are a retrieval reranker. Given a query and candidate passages, output the indices of the top {final_k} most relevant passages in descending order of relevance, as a JSON array of integers. Output ONLY the JSON array.\n\nQuery: {query}\n\nPassages:\n{numbered}\n\nTop {final_k} indices:"
        )
    }

    /// Pull the first `[1, 2, 3]`-shaped array out of the model reply.
    /// Returns an empty list when nothing parses. Public for tests.
    #[must_use]
    pub fn parse_indices(raw: &str) -> Vec<usize> {
        let Some(start) = raw.find('[') else {
            return Vec::new();
        };
        let Some(end_rel) = raw[start..].find(']') else {
            return Vec::new();
        };
        let slice = &raw[start..=start + end_rel];
        // The TS regex is `\[[\d,\s]+\]`: only digits, commas and spaces.
        let inner = &slice[1..slice.len() - 1];
        if inner.is_empty()
            || !inner
                .chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c.is_whitespace())
        {
            return Vec::new();
        }
        inner
            .split(',')
            .filter_map(|p| p.trim().parse::<usize>().ok())
            .collect()
    }

    /// Select `final_k` results from the picked indices, then fill from the
    /// original order. Public for tests.
    #[must_use]
    pub fn select(
        candidates: &[RerankCandidate],
        picked: &[usize],
        final_k: u32,
    ) -> Vec<RerankResult> {
        let limit = final_k as usize;
        let mut seen: Vec<ChunkId> = Vec::new();
        let mut out: Vec<RerankResult> = Vec::new();
        let push =
            |out: &mut Vec<RerankResult>, seen: &mut Vec<ChunkId>, cand: &RerankCandidate| {
                if seen.contains(&cand.id) {
                    return;
                }
                seen.push(cand.id.clone());
                let rank = out.len() as u32;
                out.push(RerankResult {
                    id: cand.id.clone(),
                    final_rank: rank,
                    score: final_k as f32 - rank as f32,
                });
            };
        for &idx in picked {
            if out.len() >= limit {
                break;
            }
            if let Some(cand) = candidates.get(idx) {
                push(&mut out, &mut seen, cand);
            }
        }
        for cand in candidates {
            if out.len() >= limit {
                break;
            }
            push(&mut out, &mut seen, cand);
        }
        out
    }
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Option<Vec<ChatChoice>>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatContent>,
}

#[derive(Deserialize)]
struct ChatContent {
    content: Option<String>,
}

#[async_trait]
impl Reranker for LlmReranker {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        // Same short-circuit as the TS code: fewer candidates than requested
        // means keep the original order, scored by position.
        if (candidates.len() as u64) < u64::from(final_k) {
            return Ok(candidates
                .into_iter()
                .enumerate()
                .map(|(i, c)| RerankResult {
                    id: c.id,
                    final_rank: i as u32,
                    score: final_k as f32 - i as f32,
                })
                .collect());
        }

        let prompt = Self::build_prompt(query, &candidates, final_k);
        let body = ChatRequest {
            model: &self.model,
            messages: vec![ChatMessage {
                role: "user",
                content: &prompt,
            }],
            max_tokens: 200,
            temperature: 0.0,
        };
        let res = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| PortError::Unavailable(format!("llm reranker request: {e}")))?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            let snippet: String = text.chars().take(300).collect();
            return Err(PortError::Unavailable(format!(
                "LlmReranker {} {}: {snippet}",
                self.model,
                status.as_u16()
            )));
        }
        let data: ChatResponse = res
            .json()
            .await
            .map_err(|e| PortError::Unavailable(format!("llm reranker body: {e}")))?;
        let raw = data
            .choices
            .and_then(|c| c.into_iter().next())
            .and_then(|c| c.message)
            .and_then(|m| m.content)
            .unwrap_or_default();

        let picked = Self::parse_indices(&raw);
        Ok(Self::select(&candidates, &picked, final_k))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, text: &str) -> RerankCandidate {
        RerankCandidate {
            id: ChunkId(id.into()),
            text: text.into(),
            original_rank: 0,
            original_score: 0.0,
        }
    }

    #[test]
    fn parses_first_bracketed_digit_list_only() {
        assert_eq!(LlmReranker::parse_indices("[2, 0, 1]"), vec![2, 0, 1]);
        assert_eq!(LlmReranker::parse_indices("Sure: [3,1] ok"), vec![3, 1]);
        assert!(LlmReranker::parse_indices("no array here").is_empty());
        assert!(LlmReranker::parse_indices("[a, b]").is_empty());
    }

    #[test]
    fn select_dedupes_skips_bad_indices_and_fills_from_original_order() {
        let cands = vec![cand("a", "x"), cand("b", "y"), cand("c", "z")];
        // 9 is out of range, 2 is picked twice; then the fill takes "a".
        let out = LlmReranker::select(&cands, &[2, 9, 2], 2);
        let ids: Vec<&str> = out.iter().map(|r| r.id.0.as_str()).collect();
        assert_eq!(ids, vec!["c", "a"]);
        assert_eq!(out[0].final_rank, 0);
        assert_eq!(out[1].final_rank, 1);
    }

    #[test]
    fn prompt_truncates_passages_and_flattens_newlines() {
        let long = "p".repeat(500);
        let cands = vec![cand("a", &format!("line1\nline2 {long}"))];
        let prompt = LlmReranker::build_prompt("q?", &cands, 1);
        assert!(prompt.contains("[0] line1 line2 "));
        assert!(!prompt.contains(&"p".repeat(401)));
        assert!(prompt.contains("Query: q?"));
    }

    #[test]
    fn requires_api_key() {
        assert!(LlmReranker::new("m", "").is_err());
        assert!(LlmReranker::new("m", "  ").is_err());
    }

    #[tokio::test]
    async fn short_circuits_when_fewer_candidates_than_final_k() {
        // No network: the early return must not call the endpoint.
        let r = LlmReranker::new("m", "key").expect("reranker");
        let out = r
            .rerank("q", vec![cand("a", "x")], 5)
            .await
            .expect("rerank");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].final_rank, 0);
    }
}
