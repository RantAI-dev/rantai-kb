//! SurrealDB HTTP client and low-level response parsing.

use std::time::Duration;

use kb_ports::{ChunkId, ChunkMetadata, ChunkType, DocumentId, PortError, SearchHit};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_LIMIT: u32 = 100;

#[must_use]
pub fn safe_limit(limit: u32) -> u32 {
    limit.clamp(1, MAX_LIMIT)
}

/// Settings for connecting to SurrealDB.
#[derive(Clone, Debug)]
pub struct SurrealSettings {
    pub url: String,
    pub namespace: String,
    pub database: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub request_timeout: Duration,
}

/// Low-level HTTP client for SurrealDB.
#[derive(Clone)]
pub struct SurrealChunkStore {
    pub(crate) settings: SurrealSettings,
    pub(crate) client: reqwest::Client,
}

impl SurrealChunkStore {
    /// Construct a new SurrealChunkStore.
    pub fn new(settings: SurrealSettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest: {e}")))?;
        Ok(Self { settings, client })
    }

    /// SurrealDB SQL endpoint: `{url}/sql`.
    pub fn endpoint(&self) -> String {
        format!("{}/sql", self.settings.url.trim_end_matches('/'))
    }

    /// Execute a SurrealQL query, passing query parameters (e.g. `?q=...`) to bind `$q`.
    pub async fn query_with_params(
        &self,
        sql: &str,
        params: &[(&str, &str)],
    ) -> Result<Vec<Value>, PortError> {
        let mut req = self
            .client
            .post(self.endpoint())
            .header("Accept", "application/json")
            .header("Content-Type", "text/plain")
            .header("Surreal-NS", &self.settings.namespace)
            .header("Surreal-DB", &self.settings.database);

        if !params.is_empty() {
            req = req.query(params);
        }

        if let (Some(u), Some(p)) = (&self.settings.username, &self.settings.password) {
            req = req.basic_auth(u, Some(p));
        }

        let res = req
            .body(sql.to_string())
            .send()
            .await
            .map_err(|e| PortError::Unavailable(format!("surreal send: {e}")))?;

        let status = res.status().as_u16();
        let bytes = res
            .bytes()
            .await
            .map_err(|e| PortError::Unavailable(format!("surreal body: {e}")))?;

        if !(200..300).contains(&status) {
            let excerpt: String = String::from_utf8_lossy(&bytes).chars().take(300).collect();
            return Err(PortError::Unavailable(format!(
                "surreal http {status}: {excerpt}"
            )));
        }

        let envelope: Vec<Value> = serde_json::from_slice(&bytes)
            .map_err(|e| PortError::Malformed(format!("surreal shape: {e}")))?;

        // A statement whose result envelope has status other than "OK" is an error.
        for stmt in &envelope {
            if let Some(st) = stmt.get("status").and_then(Value::as_str) {
                if st != "OK" {
                    let detail = stmt.get("result").and_then(Value::as_str).unwrap_or(st);
                    return Err(PortError::Unavailable(format!(
                        "surreal statement error status '{st}': {detail}"
                    )));
                }
            }
        }

        Ok(envelope)
    }

    /// Execute a SurrealQL query without query-string parameters.
    pub async fn query(&self, sql: &str) -> Result<Vec<Value>, PortError> {
        self.query_with_params(sql, &[]).await
    }

    /// Extract rows array from the envelope of the first statement.
    pub fn extract_rows(envelope: &[Value]) -> Vec<Value> {
        let Some(first) = envelope.first() else {
            return Vec::new();
        };
        if let Some(arr) = first.get("result").and_then(Value::as_array) {
            return arr.clone();
        }
        if first.is_array() {
            return first.as_array().cloned().unwrap_or_default();
        }
        Vec::new()
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChunkRow {
    pub id: String,
    pub document_id: String,
    pub content: String,
    pub chunk_index: i64,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub contextual_prefix: Option<String>,
    #[serde(default)]
    pub similarity: Option<f32>,
    #[serde(default)]
    pub score: Option<f32>,
}

pub fn row_to_chunk(row: &Value) -> Option<ChunkRow> {
    serde_json::from_value(row.clone()).ok()
}

pub fn row_meta(meta: Option<&Value>) -> ChunkMetadata {
    let Some(v) = meta else {
        return ChunkMetadata::default();
    };
    let obj = match v {
        Value::Object(_) => v,
        _ => return ChunkMetadata::default(),
    };
    let section = obj.get("section").and_then(Value::as_str).map(String::from);
    let page = obj.get("page").and_then(Value::as_u64).map(|n| n as u32);
    let chunk_index = obj
        .get("chunkIndex")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    let asset_key = obj
        .get("assetKey")
        .and_then(Value::as_str)
        .map(String::from);
    let anchor_chunk_index = obj
        .get("anchorChunkIndex")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    let contextual_prefix = obj
        .get("contextualPrefix")
        .and_then(Value::as_str)
        .map(String::from);
    ChunkMetadata {
        section,
        page,
        chunk_index,
        asset_key,
        anchor_chunk_index,
        contextual_prefix,
    }
}

pub fn row_chunk_type(meta: Option<&Value>) -> ChunkType {
    let Some(v) = meta else {
        return ChunkType::Text;
    };
    let s = v.get("chunkType").and_then(Value::as_str);
    ChunkType::from_storage(s)
}

pub fn build_hit(row: ChunkRow, rank: u32, score: f32) -> SearchHit {
    let meta = row_meta(row.metadata.as_ref());
    let chunk_type = row_chunk_type(row.metadata.as_ref());
    let id = ChunkId::new_unchecked(row.id);
    let document_id = DocumentId::new_unchecked(row.document_id);
    let document_title = String::new();
    let mut metadata = meta;
    // The row column is authoritative: neighbour lookup keys on it, so it
    // overrides the metadata `chunkIndex`.
    if let Ok(idx) = u32::try_from(row.chunk_index) {
        metadata.chunk_index = Some(idx);
    }
    if metadata.contextual_prefix.is_none() {
        metadata.contextual_prefix = row.contextual_prefix;
    }
    SearchHit {
        chunk: kb_ports::Chunk {
            id,
            document_id,
            document_title,
            content: row.content,
            chunk_type,
            metadata,
        },
        similarity: score,
        rank,
    }
}
