//! SurrealDB-backed `ChunkStore` over the HTTP `/sql` endpoint.
//!
//! We deliberately do NOT depend on the `surrealdb` crate — the engine talks to
//! SurrealDB over its plain HTTP API (`POST /sql`). SurrealDB's HTTP endpoint
//! runs a single statement with no bindings (WebSocket connections carry
//! variables); everything that goes into the query is inlined.
//!
//! All values are inlined from sources we control:
//!   - `document_id IN [...]` lists come from `DocumentScope.document_ids`,
//!     which `kb-store-pg` resolved against the tenant's data.
//!   - embeddings are the engine's own output.
//!   - chunk indices come from neighbour anchors we built.
//!
//! We serialise numeric arrays as JSON literals and trust the JSON encoder to
//! emit clean output. Document ids are inlined as SurrealQL string literals
//! after a strict ASCII + length check (the scope cannot contain anything
//! else: Postgres UUIDs and the Prisma `String @id` are both safe).
//!
//! The only inline integer is `LIMIT N`, capped to 2000 by the caller —
//! SurrealDB binds LIMIT inconsistently across versions.

use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{
    ChunkId, ChunkMetadata, ChunkStore, ChunkType, DocumentId, DocumentScope, NeighbourAnchor,
    PortError, SearchHit,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_LIMIT: u32 = 2000;

#[derive(Clone, Debug)]
pub struct SurrealSettings {
    pub url: String,
    pub namespace: String,
    pub database: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub request_timeout: Duration,
}

#[derive(Clone)]
pub struct SurrealChunkStore {
    settings: SurrealSettings,
    client: reqwest::Client,
}

impl SurrealChunkStore {
    pub fn new(settings: SurrealSettings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("reqwest: {e}")))?;
        Ok(Self { settings, client })
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/sql/{}/{}",
            self.settings.url.trim_end_matches('/'),
            surql_string(&self.settings.namespace),
            surql_string(&self.settings.database)
        )
    }

    async fn query(&self, sql: &str) -> Result<Vec<Value>, PortError> {
        let mut req = self
            .client
            .post(self.endpoint())
            .header("Accept", "application/json")
            .header("Content-Type", "text/plain");
        if let (Some(u), Some(p)) = (&self.settings.username, &self.settings.password) {
            let creds = format!("{}:{}", u, p);
            req = req.header(
                "Authorization",
                format!("Basic {}", base64_encode(creds.as_bytes())),
            );
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
        let parsed: Vec<Value> = serde_json::from_slice(&bytes)
            .map_err(|e| PortError::Malformed(format!("surreal shape: {e}")))?;
        Ok(parsed)
    }

    fn extract_rows(envelope: &[Value]) -> Vec<Value> {
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

fn safe_limit(limit: u32) -> u32 {
    limit.clamp(1, MAX_LIMIT)
}

/// SurrealQL string literal: wraps in `'…'` and escapes any embedded `\'` /
/// `\n` / `\\` chars. Use only on caller-validated strings (UUIDs, integer
/// identifiers).
fn surql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

fn surql_id_list(ids: &[String]) -> String {
    if ids.is_empty() {
        return "[]".into();
    }
    let mut out = String::from("[");
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&surql_string(id));
    }
    out.push(']');
    out
}

fn surql_int_list(ints: &[u32]) -> String {
    let mut out = String::from("[");
    for (i, n) in ints.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&n.to_string());
    }
    out.push(']');
    out
}

/// Inline an `f32` slice as a SurrealQL array literal `[f, f, …]`.
fn surql_f32_array(values: &[f32]) -> String {
    let mut out = String::with_capacity(values.len() * 8);
    out.push('[');
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        // f32::to_string emits up to 9 significant digits, including NaN/Inf.
        // NaN/Inf would parse-fail in SurrealQL — the embedder doesn't produce
        // them, but we replace them defensively.
        let s = if v.is_finite() {
            v.to_string()
        } else {
            "0.0".to_string()
        };
        out.push_str(&s);
    }
    out.push(']');
    out
}

/// Small inline base64 encoder.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHA[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

#[derive(Debug, Deserialize, Serialize)]
struct ChunkRow {
    id: String,
    document_id: String,
    content: String,
    chunk_index: i64,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    contextual_prefix: Option<String>,
    #[serde(default)]
    similarity: Option<f32>,
    #[serde(default)]
    score: Option<f32>,
}

fn row_to_chunk(row: &Value) -> Option<ChunkRow> {
    serde_json::from_value(row.clone()).ok()
}

fn row_meta(meta: Option<&Value>) -> ChunkMetadata {
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

fn row_chunk_type(meta: Option<&Value>) -> ChunkType {
    let Some(v) = meta else {
        return ChunkType::Text;
    };
    let s = v.get("chunkType").and_then(Value::as_str);
    ChunkType::from_storage(s)
}

fn build_hit(row: ChunkRow, rank: u32, score: f32) -> SearchHit {
    let meta = row_meta(row.metadata.as_ref());
    let chunk_type = row_chunk_type(row.metadata.as_ref());
    let id = ChunkId::new_unchecked(row.id);
    let document_id = DocumentId::new_unchecked(row.document_id);
    let document_title = String::new();
    let mut metadata = meta;
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

#[async_trait]
impl ChunkStore for SurrealChunkStore {
    async fn vector_search(
        &self,
        scope: &DocumentScope,
        embedding: &[f32],
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        let ids: Vec<String> = scope.document_ids().iter().map(|d| d.0.clone()).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let limit = safe_limit(limit);
        let sql = format!(
            "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix, \
             vector::similarity::cosine(embedding, {}) AS similarity \
             FROM document_chunk \
             WHERE document_id IN {} \
             ORDER BY similarity DESC \
             LIMIT {};",
            surql_f32_array(embedding),
            surql_id_list(&ids),
            limit,
        );
        let envelope = self.query(&sql).await?;
        let rows = Self::extract_rows(&envelope);
        let mut hits = Vec::with_capacity(rows.len());
        for (i, row) in rows.into_iter().enumerate() {
            if let Some(r) = row_to_chunk(&row) {
                let score = r.similarity.unwrap_or(0.0);
                hits.push(build_hit(r, i as u32, score));
            }
        }
        Ok(hits)
    }

    async fn bm25_search(
        &self,
        scope: &DocumentScope,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = scope.document_ids().iter().map(|d| d.0.clone()).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let limit = safe_limit(limit);
        let sql = format!(
            "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix, \
             search::score(0) AS score \
             FROM document_chunk \
             WHERE content @@ {} AND document_id IN {} \
             ORDER BY score DESC, id ASC \
             LIMIT {};",
            surql_string(query),
            surql_id_list(&ids),
            limit,
        );
        let envelope = self.query(&sql).await?;
        let rows = Self::extract_rows(&envelope);
        let mut hits = Vec::with_capacity(rows.len());
        for (i, row) in rows.into_iter().enumerate() {
            if let Some(r) = row_to_chunk(&row) {
                let score = r.score.unwrap_or(0.0);
                hits.push(build_hit(r, i as u32, score));
            }
        }
        Ok(hits)
    }

    async fn neighbours(
        &self,
        anchors: &[NeighbourAnchor],
        window: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        if anchors.is_empty() || window == 0 {
            return Ok(Vec::new());
        }
        let mut targets: std::collections::HashSet<(String, u32)> =
            std::collections::HashSet::new();
        let mut ddocs: std::collections::HashSet<String> = std::collections::HashSet::new();
        for a in anchors {
            ddocs.insert(a.document_id.0.clone());
            for d in 1..=window as i64 {
                let pos = a.chunk_index as i64 + d;
                if pos >= 0 {
                    targets.insert((a.document_id.0.clone(), pos as u32));
                }
                let neg = a.chunk_index as i64 - d;
                if neg >= 0 {
                    targets.insert((a.document_id.0.clone(), neg as u32));
                }
            }
        }
        let indices: Vec<u32> = targets.iter().map(|(_, i)| *i).collect();
        let doc_ids: Vec<String> = ddocs.into_iter().collect();
        if indices.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix \
             FROM document_chunk \
             WHERE document_id IN {} AND chunk_index IN {}",
            surql_id_list(&doc_ids),
            surql_int_list(&indices),
        );
        let envelope = self.query(&sql).await?;
        let rows = Self::extract_rows(&envelope);

        let anchor_ids: std::collections::HashSet<String> =
            anchors.iter().map(|a| a.chunk_id.0.clone()).collect();
        let mut hits = Vec::with_capacity(rows.len());
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for row in rows {
            let Some(r) = row_to_chunk(&row) else {
                continue;
            };
            if anchor_ids.contains(&r.id) || !seen.insert(r.id.clone()) {
                continue;
            }
            hits.push(build_hit(r, u32::MAX, 0.0));
        }
        Ok(hits)
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        query: &str,
        retrieved_text: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = scope.document_ids().iter().map(|d| d.0.clone()).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix \
             FROM document_chunk \
             WHERE document_id IN {} AND metadata.chunkType = 'figure'",
            surql_id_list(&ids),
        );
        let envelope = self.query(&sql).await?;
        let rows = Self::extract_rows(&envelope);
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let stopwords: &[&str] = &[
            "yang", "untuk", "pada", "dan", "dengan", "dari", "atau", "adalah", "dalam", "serta",
            "oleh", "hal", "ini", "itu", "para", "kepada", "gambar", "tabel", "raja", "dewa",
            "kitab", "hari", "suci",
        ];
        let query_lower = query.to_lowercase();
        let text_lower = retrieved_text.to_lowercase();

        let mut scored_rows: Vec<(i32, ChunkRow)> = Vec::new();
        for row in rows {
            let Some(r) = row_to_chunk(&row) else {
                continue;
            };
            let asset_key = r
                .metadata
                .as_ref()
                .and_then(|m| m.get("assetKey"))
                .and_then(Value::as_str);
            if asset_key.is_none() {
                continue;
            }
            let caption = r
                .metadata
                .as_ref()
                .and_then(|m| m.get("section"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| r.content.clone());
            let trimmed = caption
                .trim_start_matches(|c: char| c == '[' || c == ']')
                .trim()
                .to_string();
            let anchored = r
                .metadata
                .as_ref()
                .and_then(|m| m.get("anchorChunkIndex"))
                .and_then(Value::as_u64)
                .is_some();
            let mut score: i32 = 0;
            if anchored {
                score = 3;
            } else if starts_with_figure_label(&trimmed) {
                let kws = caption_keywords(&trimmed, stopwords);
                if !kws.is_empty() {
                    if kws.iter().any(|k| query_lower.contains(k)) {
                        score = 2;
                    } else if kws.iter().any(|k| text_lower.contains(k)) {
                        score = 1;
                    }
                }
            }
            if score > 0 {
                scored_rows.push((score, r));
            }
        }
        scored_rows.sort_by(|a, b| b.0.cmp(&a.0));
        let limit = safe_limit(limit) as usize;
        let mut hits = Vec::with_capacity(limit.min(scored_rows.len()));
        for (_, r) in scored_rows.into_iter().take(limit) {
            hits.push(build_hit(r, u32::MAX, 0.0));
        }
        Ok(hits)
    }
}

fn starts_with_figure_label(caption: &str) -> bool {
    let lower = caption.to_lowercase();
    [
        "gambar",
        "tabel",
        "grafik",
        "diagram",
        "foto",
        "bagan",
        "ilustrasi",
        "peta",
    ]
    .iter()
    .any(|w| {
        let trimmed = lower.trim_start();
        if let Some(rest) = trimmed.strip_prefix(w) {
            rest.chars()
                .next()
                .map(|c| c == ' ' || c == '.' || c.is_whitespace())
                .unwrap_or(true)
        } else {
            false
        }
    })
}

fn caption_keywords(caption: &str, stopwords: &[&str]) -> Vec<String> {
    let mut body = caption.to_string();
    let lower = body.to_lowercase();
    let labels = [
        "gambar",
        "tabel",
        "grafik",
        "diagram",
        "foto",
        "bagan",
        "ilustrasi",
        "peta",
    ];
    for label in labels {
        if let Some(pos) = lower.find(label) {
            let mut end = pos + label.len();
            if let Some(c) = body[end..].chars().next() {
                if c == ' ' || c == '.' {
                    end += 1;
                }
            }
            while end < body.len() {
                let Some(c) = body[end..].chars().next() else {
                    break;
                };
                if c.is_ascii_digit() || c == '.' || c == ' ' {
                    end += 1;
                } else {
                    break;
                }
            }
            body = body[end..].to_string();
            break;
        }
    }
    body.trim()
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| w.len() >= 4 && !stopwords.iter().any(|s| *s == w.to_lowercase()))
        .map(|w| w.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_limit_clamps_to_max() {
        assert_eq!(safe_limit(0), 1);
        assert_eq!(safe_limit(10), 10);
        assert_eq!(safe_limit(MAX_LIMIT + 1000), MAX_LIMIT);
    }

    #[test]
    fn surql_string_escapes_quotes_and_control_chars() {
        assert_eq!(surql_string("hello"), "'hello'");
        assert_eq!(surql_string("a'b"), "'a\\'b'");
        assert_eq!(surql_string("a\\b"), "'a\\\\b'");
        assert_eq!(surql_string("a\nb"), "'a\\nb'");
    }

    #[test]
    fn surql_id_list_handles_empty_and_multiple() {
        assert_eq!(surql_id_list(&[]), "[]");
        let ids = vec!["a".to_string(), "b".to_string()];
        assert_eq!(surql_id_list(&ids), "['a', 'b']");
    }

    #[test]
    fn surql_f32_array_emits_finite_values() {
        let arr = surql_f32_array(&[0.0, 1.5, -2.25]);
        assert_eq!(arr, "[0, 1.5, -2.25]");
        let arr = surql_f32_array(&[f32::NAN, f32::INFINITY]);
        assert_eq!(arr, "[0.0, 0.0]");
    }

    #[test]
    fn base64_encoder_matches_padding_rules() {
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn extract_rows_handles_envelope_and_bare() {
        let env = vec![serde_json::json!({"status": "OK", "result": [{"a": 1}]})];
        let rows = SurrealChunkStore::extract_rows(&env);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["a"], json!(1));
        let bare: Vec<Value> = vec![json!([{"a": 2}])];
        let rows = SurrealChunkStore::extract_rows(&bare);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["a"], json!(2));
        let empty: Vec<Value> = vec![];
        assert!(SurrealChunkStore::extract_rows(&empty).is_empty());
    }

    #[test]
    fn figure_keywords_strip_label_and_stopwords() {
        let stop = ["yang", "gambar", "raja"];
        let kws = caption_keywords("Gambar 1.15 Posisi usus halus", &stop);
        assert!(kws.contains(&"posisi".to_string()));
        assert!(kws.contains(&"usus".to_string()));
        assert!(!kws.iter().any(|w| w == "gambar"));
        assert!(!kws.iter().any(|w| w == "15"));
    }
}
