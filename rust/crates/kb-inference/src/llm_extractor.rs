//! LLM-based entity and relation extraction.
//!
//! Mirrors `src/lib/document-intelligence/combined-extractor.ts`,
//! `entity-extractor.ts`, and `relation-extractor.ts`.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{Entity, EntityExtractor, PortError, Relation};
use serde::Deserialize;
use serde_json::{Value, json};

const DEFAULT_MODEL: &str = "openai/gpt-4.1-nano";
const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_MAX_CHUNK_CHARS: usize = 5000;
const DEFAULT_CONCURRENCY: usize = 10;
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// Settings for LLM extraction.
#[derive(Debug, Clone)]
pub struct ExtractorSettings {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub concurrency: usize,
    pub max_tokens: u32,
    pub temperature: f32,
    pub max_chunk_chars: usize,
    pub timeout: Duration,
    pub max_retries: u32,
    pub retry_delay: Duration,
}

impl ExtractorSettings {
    /// Read settings from environment variables matching TypeScript names.
    #[must_use]
    pub fn from_env() -> Self {
        let model = std::env::var("ENTITY_EXTRACTION_LLM_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        let base_url = std::env::var("ENTITY_EXTRACTION_LLM_BASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();

        let api_key = std::env::var("ENTITY_EXTRACTION_LLM_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .unwrap_or_default();

        let concurrency = std::env::var("ENTITY_EXTRACTION_CONCURRENCY")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_CONCURRENCY);

        let timeout_ms = std::env::var("ENTITY_EXTRACTION_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MS);

        Self {
            api_key,
            base_url,
            model,
            concurrency,
            max_tokens: 4000,
            temperature: 0.1,
            max_chunk_chars: DEFAULT_MAX_CHUNK_CHARS,
            timeout: Duration::from_millis(timeout_ms),
            max_retries: 3,
            retry_delay: Duration::from_millis(1000),
        }
    }
}

impl Default for ExtractorSettings {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Combined single-pass entity and relation extractor.
#[derive(Clone)]
pub struct LlmCombinedExtractor {
    settings: ExtractorSettings,
    client: reqwest::Client,
}

impl LlmCombinedExtractor {
    /// Create a new extractor with the provided settings.
    pub fn new(settings: ExtractorSettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest client: {e}")))?;
        Ok(Self { settings, client })
    }

    /// Build system and user prompt for combined extraction verbatim from TS.
    #[must_use]
    pub fn build_combined_prompt(text: &str) -> (String, String) {
        let system_prompt = "\
You are an expert at extracting structured knowledge from text.\n\
Your task is to extract:\n\
1. ENTITIES: People, organizations, locations, products, concepts, dates, events, laws, regulations, etc.\n\
2. RELATIONS: How entities are connected (e.g., \"works_for\", \"located_in\", \"regulates\", \"founded_by\", etc.)\n\
\n\
Output JSON with this exact structure:\n\
{\n\
  \"entities\": [\n\
    {\"name\": \"Entity Name\", \"type\": \"PERSON|ORG|LOCATION|DATE|EVENT|CONCEPT|REGULATION|PRODUCT|OTHER\", \"description\": \"brief description\", \"confidence\": 0.9}\n\
  ],\n\
  \"relations\": [\n\
    {\"source\": \"Entity A\", \"target\": \"Entity B\", \"relation_type\": \"relationship_type\", \"context\": \"text supporting this relation\", \"confidence\": 0.85}\n\
  ]\n\
}\n\
\n\
Guidelines:\n\
- Extract ALL meaningful entities, not just the most prominent ones\n\
- Extract relations ONLY between entities you extracted\n\
- Use consistent entity names (same entity = same name)\n\
- Confidence: 0.9+ for explicit mentions, 0.7-0.9 for inferred\n\
- relation_type should be lowercase with underscores (e.g., works_for, part_of)\n\
- Keep context short (the phrase showing the relationship)\n\
- Be thorough but precise".to_string();

        let user_prompt = format!(
            "Extract all entities and relations from this text:\n\n{text}\n\nReturn ONLY valid JSON with \"entities\" and \"relations\" arrays."
        );

        (system_prompt, user_prompt)
    }

    /// Build system and user prompt for entity-only extraction.
    #[must_use]
    pub fn build_entity_prompt(text: &str, max_chars: usize) -> (String, String) {
        let system_prompt = "\
You are an entity extraction system. Extract entities and return ONLY valid JSON.\n\
\n\
Entity Types:\n\
- Person, Organization, Location, Date, Event, Concept, Product, Number\n\
- Technology, API, Function, Error\n\
- Transaction, Account, Currency\n\
\n\
IMPORTANT:\n\
- Return ONLY valid JSON, no other text\n\
- Keep descriptions SHORT (max 50 chars)\n\
- Keep context SHORT (max 100 chars)\n\
- Limit to TOP 30 most important entities per chunk\n\
\n\
JSON format:\n\
{\"entities\":[{\"name\":\"...\",\"type\":\"...\",\"confidence\":0.9,\"context\":\"...\",\"description\":\"...\"}]}".to_string();

        let truncated_text = if text.len() > max_chars {
            let mut s: String = text.chars().take(max_chars).collect();
            s.push_str("...[truncated]");
            s
        } else {
            text.to_string()
        };

        let user_prompt = format!("Extract entities from this text:\n\n{truncated_text}");
        (system_prompt, user_prompt)
    }

    /// Split text into paragraphs and sentences respecting max chunk characters.
    #[must_use]
    pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
        if text.trim().is_empty() {
            return Vec::new();
        }

        let mut chunks = Vec::new();
        let paragraphs = text.split("\n\n");
        let mut current_chunk = String::new();

        for paragraph in paragraphs {
            let p = paragraph.trim();
            if p.is_empty() {
                continue;
            }

            if p.len() > max_chars {
                if !current_chunk.is_empty() {
                    chunks.push(std::mem::take(&mut current_chunk));
                }
                // Split large paragraph by sentence delimiters
                let sentences = p.split(|c| c == '.' || c == '!' || c == '?');
                for s in sentences {
                    let st = s.trim();
                    if st.is_empty() {
                        continue;
                    }
                    if current_chunk.len() + st.len() > max_chars {
                        if !current_chunk.is_empty() {
                            chunks.push(std::mem::take(&mut current_chunk));
                        }
                        if st.len() > max_chars {
                            let part: String = st.chars().take(max_chars).collect();
                            current_chunk = part;
                        } else {
                            current_chunk = st.to_string();
                        }
                    } else {
                        if !current_chunk.is_empty() {
                            current_chunk.push(' ');
                        }
                        current_chunk.push_str(st);
                    }
                }
            } else if current_chunk.len() + p.len() + 2 > max_chars {
                chunks.push(std::mem::take(&mut current_chunk));
                current_chunk = p.to_string();
            } else {
                if !current_chunk.is_empty() {
                    current_chunk.push_str("\n\n");
                }
                current_chunk.push_str(p);
            }
        }

        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }

        if chunks.is_empty() {
            vec![text.chars().take(max_chars).collect()]
        } else {
            chunks
        }
    }

    /// Parse LLM response with thinking tag removal, code fence stripping, and repair fallback.
    #[must_use]
    pub fn parse_combined_response(content: &str) -> (Vec<RawEntity>, Vec<RawRelation>) {
        let mut cleaned = content.trim();

        // 1. Remove <think>...</think>
        let mut stripped_think = cleaned.to_string();
        while let Some(start) = stripped_think.find("<think>") {
            if let Some(end_offset) = stripped_think[start..].find("</think>") {
                stripped_think.replace_range(start..start + end_offset + 8, "");
            } else {
                break;
            }
        }
        cleaned = stripped_think.trim();

        // 2. Strip code fences
        if cleaned.starts_with("```json") {
            cleaned = cleaned.trim_start_matches("```json").trim();
            if let Some(pos) = cleaned.rfind("```") {
                cleaned = cleaned[..pos].trim();
            }
        } else if cleaned.starts_with("```") {
            cleaned = cleaned.trim_start_matches("```").trim();
            if let Some(pos) = cleaned.rfind("```") {
                cleaned = cleaned[..pos].trim();
            }
        }

        // 3. Slice from first '{'
        if let Some(json_start) = cleaned.find('{') {
            if json_start > 0 {
                cleaned = &cleaned[json_start..];
            }
        }

        // 4. Try parsing JSON
        if let Ok(val) = serde_json::from_str::<Value>(cleaned) {
            return extract_raw_items(&val);
        }

        // 5. Try repair truncated JSON
        if let Some(repaired) = repair_truncated_json(cleaned) {
            return extract_raw_items(&repaired);
        }

        (Vec::new(), Vec::new())
    }

    /// Execute single chunk extraction against endpoint.
    async fn extract_chunk(&self, chunk: &str) -> Result<(Vec<RawEntity>, Vec<RawRelation>), PortError> {
        let (system_prompt, user_prompt) = Self::build_combined_prompt(chunk);
        let url = format!("{}/chat/completions", self.settings.base_url.trim_end_matches('/'));

        let body = json!({
            "model": self.settings.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt },
            ],
            "max_tokens": self.settings.max_tokens,
            "temperature": self.settings.temperature,
            "reasoning": { "enabled": false },
            "response_format": { "type": "json_object" },
        });

        let mut req = self.client.post(&url).json(&body);
        if !self.settings.api_key.trim().is_empty() {
            req = req.bearer_auth(&self.settings.api_key);
        }
        req = req
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://rantai.dev")
            .header("X-Title", "RantAI Document Intelligence");

        let res = req
            .send()
            .await
            .map_err(|e| PortError::Unavailable(format!("LLM extraction request failed: {e}")))?;

        if !res.status().is_success() {
            let status = res.status();
            let err_text = res.text().await.unwrap_or_default();
            return Err(PortError::Unavailable(format!("LLM API error {status}: {err_text}")));
        }

        let resp_json: Value = res
            .json()
            .await
            .map_err(|e| PortError::Malformed(format!("LLM extraction invalid JSON: {e}")))?;

        let content = resp_json
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("");

        Ok(Self::parse_combined_response(content))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawEntity {
    pub name: String,
    #[serde(rename = "type")]
    pub entity_type: Option<String>,
    pub description: Option<String>,
    pub confidence: Option<f32>,
    pub context: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawRelation {
    pub source: Option<String>,
    #[serde(rename = "source_entity")]
    pub source_entity: Option<String>,
    pub target: Option<String>,
    #[serde(rename = "target_entity")]
    pub target_entity: Option<String>,
    pub relation_type: Option<String>,
    pub confidence: Option<f32>,
    pub context: Option<String>,
}

fn extract_raw_items(val: &Value) -> (Vec<RawEntity>, Vec<RawRelation>) {
    let entities: Vec<RawEntity> = val
        .get("entities")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    let relations: Vec<RawRelation> = val
        .get("relations")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    (entities, relations)
}

/// Repair truncated JSON by balancing open braces and brackets in correct nesting order.
fn repair_truncated_json(content: &str) -> Option<Value> {
    let s = content.trim().to_string();

    // 1. Stack-based delimiter balancing
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escape = false;

    for c in s.chars() {
        if escape {
            escape = false;
            continue;
        }
        if c == '\\' && in_string {
            escape = true;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match c {
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if let Some(pos) = stack.iter().rposition(|&expected| expected == c) {
                    stack.truncate(pos);
                }
            }
            _ => {}
        }
    }

    let mut candidate = s.clone();
    if in_string {
        candidate.push('"');
    }
    let trimmed_c = candidate.trim_end();
    if trimmed_c.ends_with(',') {
        candidate = trimmed_c[..trimmed_c.len() - 1].to_string();
    }
    for &closer in stack.iter().rev() {
        candidate.push(closer);
    }

    if let Ok(val) = serde_json::from_str::<Value>(&candidate) {
        return Some(val);
    }

    // 2. Retry by trimming incomplete trailing item
    let mut trimmed = s;
    for _ in 0..5 {
        if let Some(comma_pos) = trimmed.rfind(',') {
            trimmed = trimmed[..comma_pos].trim_end().to_string();
            let mut retry = trimmed.clone();
            let open_b = retry.chars().filter(|&c| c == '{').count() as i32;
            let close_b = retry.chars().filter(|&c| c == '}').count() as i32;
            let open_k = retry.chars().filter(|&c| c == '[').count() as i32;
            let close_k = retry.chars().filter(|&c| c == ']').count() as i32;
            for _ in 0..(open_k - close_k).max(0) {
                retry.push(']');
            }
            for _ in 0..(open_b - close_b).max(0) {
                retry.push('}');
            }
            if let Ok(val) = serde_json::from_str::<Value>(&retry) {
                return Some(val);
            }
        } else {
            break;
        }
    }

    None
}

#[async_trait]
impl EntityExtractor for LlmCombinedExtractor {
    async fn extract_entities(
        &self,
        text: &str,
        document_id: Option<&str>,
        chunk_id: Option<&str>,
    ) -> Result<Vec<Entity>, PortError> {
        let (entities, _) = self.extract_entities_and_relations(text, document_id, None).await?;
        Ok(entities
            .into_iter()
            .map(|mut e| {
                e.chunk_id = chunk_id.map(str::to_string);
                e
            })
            .collect())
    }

    async fn extract_entities_and_relations(
        &self,
        text: &str,
        document_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<(Vec<Entity>, Vec<Relation>), PortError> {
        let chunks = Self::chunk_text(text, self.settings.max_chunk_chars);
        let mut raw_entities = Vec::new();
        let mut raw_relations = Vec::new();

        // Process sequentially or in small concurrency batches
        for chunk in &chunks {
            match self.extract_chunk(chunk).await {
                Ok((ents, rels)) => {
                    raw_entities.extend(ents);
                    raw_relations.extend(rels);
                }
                Err(err) => {
                    tracing::warn!("LLM chunk extraction failed: {err}");
                }
            }
        }

        // Merge entities by name+type, tracking canonical names
        let mut entity_map: HashMap<String, Entity> = HashMap::new();
        let mut name_canonical_map: HashMap<String, String> = HashMap::new();

        for ent in raw_entities {
            let name = ent.name.trim();
            if name.is_empty() {
                continue;
            }
            let ent_type = ent.entity_type.as_deref().unwrap_or("OTHER");
            let norm_name = name.to_lowercase();
            let key = format!("{norm_name}::{ent_type}");
            let conf = ent.confidence.unwrap_or(0.8).clamp(0.0, 1.0);

            match entity_map.get_mut(&key) {
                Some(existing) => {
                    if conf > existing.confidence {
                        existing.confidence = conf;
                    }
                    name_canonical_map.insert(name.to_string(), existing.name.clone());
                    name_canonical_map.insert(norm_name, existing.name.clone());
                }
                None => {
                    let new_ent = Entity {
                        name: name.to_string(),
                        entity_type: ent_type.to_string(),
                        confidence: conf,
                        document_id: document_id.map(str::to_string),
                        chunk_id: None,
                        metadata: Some(json!({
                            "description": ent.description,
                            "context": ent.context,
                            "extraction_method": "combined-llm",
                        })),
                    };
                    name_canonical_map.insert(name.to_string(), name.to_string());
                    name_canonical_map.insert(norm_name, name.to_string());
                    entity_map.insert(key, new_ent);
                }
            }
        }

        // Merge relations resolving canonical names
        let mut relation_map: HashMap<String, Relation> = HashMap::new();
        for rel in raw_relations {
            let src_raw = rel.source.or(rel.source_entity).unwrap_or_default();
            let tgt_raw = rel.target.or(rel.target_entity).unwrap_or_default();
            let rel_type = rel.relation_type.unwrap_or_else(|| "RELATED_TO".to_string());

            let src = name_canonical_map
                .get(&src_raw)
                .or_else(|| name_canonical_map.get(&src_raw.to_lowercase()));
            let tgt = name_canonical_map
                .get(&tgt_raw)
                .or_else(|| name_canonical_map.get(&tgt_raw.to_lowercase()));

            let (Some(source_entity), Some(target_entity)) = (src, tgt) else {
                continue;
            };

            let conf = rel.confidence.unwrap_or(0.85).clamp(0.0, 1.0);
            let key = format!("{source_entity}::{rel_type}::{target_entity}");

            match relation_map.get_mut(&key) {
                Some(existing) => {
                    if conf > existing.confidence {
                        existing.confidence = conf;
                    }
                }
                None => {
                    relation_map.insert(
                        key,
                        Relation {
                            relation_type: rel_type,
                            confidence: conf,
                            source_entity: source_entity.clone(),
                            target_entity: target_entity.clone(),
                            context: rel.context.clone(),
                            metadata: Some(json!({
                                "context": rel.context,
                                "source_entity": source_entity,
                                "target_entity": target_entity,
                                "file_id": document_id,
                                "user_id": user_id,
                                "extraction_method": "combined-llm",
                            })),
                        },
                    );
                }
            }
        }

        let entities: Vec<Entity> = entity_map.into_values().collect();
        let relations: Vec<Relation> = relation_map.into_values().collect();
        Ok((entities, relations))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_and_thinking_json_robustly() {
        let raw = "<think>Analyzing text...</think>\n```json\n{\n  \"entities\": [{\"name\": \"Alice\", \"type\": \"Person\", \"confidence\": 0.95}],\n  \"relations\": [{\"source\": \"Alice\", \"target\": \"Bob\", \"relation_type\": \"works_for\", \"confidence\": 0.9}]\n}\n```";
        let (ents, rels) = LlmCombinedExtractor::parse_combined_response(raw);
        assert_eq!(ents.len(), 1);
        assert_eq!(ents[0].name, "Alice");
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relation_type.as_deref(), Some("works_for"));
    }

    #[test]
    fn repairs_truncated_json() {
        let truncated = "{\"entities\": [{\"name\": \"Alice\", \"type\": \"Person\"}], \"relations\": [{\"source\": \"Alice\", \"target\": \"Bob\"";
        let (ents, _) = LlmCombinedExtractor::parse_combined_response(truncated);
        assert_eq!(ents.len(), 1);
        assert_eq!(ents[0].name, "Alice");
    }

    #[test]
    fn chunk_text_splits_large_content() {
        let text = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let chunks = LlmCombinedExtractor::chunk_text(text, 25);
        assert!(chunks.len() >= 2);
    }
}
