//! Request and response DTOs for `kb-server`.
//!
//! Explicit domain-to-wire conversions matching the TypeScript service's JSON shape.

use kb_core::SourceEntry;
use kb_engine::RetrievalOutcome;
use kb_ports::SearchHit;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Parse a request body the way the TypeScript routes do: `request.json()`
/// wrapped in `.catch(() => ({}))`, so a malformed body becomes an empty object
/// and the route answers with its own field-level error instead of a parser
/// message the service never produces.
pub fn parse_json<T: DeserializeOwned + Default>(body: &[u8]) -> T {
    serde_json::from_slice(body).unwrap_or_default()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchRequestDto {
    /// Left as raw JSON: the TypeScript route rejects a non-string query with
    /// its own 400, which a typed field would turn into a parser error.
    #[serde(default)]
    pub query: Option<serde_json::Value>,
    #[serde(rename = "maxChunks", default)]
    pub max_chunks: Option<u32>,
    #[serde(rename = "knowledgeBaseIds", default)]
    pub knowledge_base_ids: Option<Vec<String>>,
    #[serde(rename = "documentIds", default)]
    pub document_ids: Option<Vec<String>>,
    pub category: Option<String>,
    pub hybrid: Option<bool>,
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResponseDto {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chunks: Option<Vec<ChunkDto>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sources: Option<Vec<SourceDto>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub results: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stats: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub degrades: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceDto {
    #[serde(rename = "documentId")]
    pub document_id: Option<String>,
    #[serde(rename = "documentTitle")]
    pub document_title: String,
    pub section: Option<String>,
    pub categories: Vec<String>,
    #[serde(rename = "assetKey")]
    pub asset_key: Option<String>,
    pub page: Option<u32>,
    #[serde(rename = "chunkType")]
    pub chunk_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChunkDto {
    pub id: String,
    pub content: String,
    #[serde(rename = "documentId")]
    pub document_id: String,
    #[serde(rename = "documentTitle")]
    pub document_title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    pub section: Option<String>,
    pub similarity: f32,
    #[serde(rename = "contextualPrefix", skip_serializing_if = "Option::is_none")]
    pub contextual_prefix: Option<Option<String>>,
    #[serde(rename = "assetKey", skip_serializing_if = "Option::is_none")]
    pub asset_key: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<Option<u32>>,
    #[serde(rename = "chunkType", skip_serializing_if = "Option::is_none")]
    pub chunk_type: Option<Option<String>>,
}

impl SearchResponseDto {
    /// The "nothing matched" shape, shared by the empty-scope and empty-result
    /// paths so all three answer with the same keys.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            chunks: Some(Vec::new()),
            sources: Some(Vec::new()),
            context: Some(String::new()),
            results: None,
            stats: None,
            note: None,
            degrades: Vec::new(),
        }
    }
}

impl From<RetrievalOutcome> for SearchResponseDto {
    fn from(outcome: RetrievalOutcome) -> Self {
        let chunks = outcome.chunks.into_iter().map(ChunkDto::from).collect();
        let sources = outcome.sources.into_iter().map(SourceDto::from).collect();
        let degrades = outcome
            .degrades
            .into_iter()
            .map(|d| d.label().to_string())
            .collect();
        SearchResponseDto {
            chunks: Some(chunks),
            sources: Some(sources),
            context: Some(outcome.context),
            results: None,
            stats: None,
            note: None,
            degrades,
        }
    }
}

impl From<SearchHit> for ChunkDto {
    fn from(hit: SearchHit) -> Self {
        let (contextual_prefix, asset_key, page, chunk_type) = if hit.is_bm25 {
            (
                Some(None), // BM25 hit serializes contextualPrefix: null
                None,       // BM25 hit omits assetKey
                None,       // BM25 hit omits page
                None,       // BM25 hit omits chunkType
            )
        } else {
            (
                hit.chunk.metadata.contextual_prefix.map(Some), // vector hit omits contextualPrefix if None
                Some(hit.chunk.metadata.asset_key), // vector hit serializes assetKey (null if None)
                Some(hit.chunk.metadata.page),      // vector hit serializes page (null if None)
                Some(Some(hit.chunk.chunk_type.as_storage().to_string())), // vector hit serializes chunkType: "text"
            )
        };
        ChunkDto {
            id: hit.chunk.id.0,
            content: hit.chunk.content,
            document_id: hit.chunk.document_id.0,
            document_title: hit.chunk.document_title,
            categories: hit.chunk.categories,
            subcategory: hit.chunk.subcategory,
            section: hit.chunk.metadata.section,
            similarity: hit.similarity,
            contextual_prefix,
            asset_key,
            page,
            chunk_type,
        }
    }
}

impl From<SourceEntry> for SourceDto {
    fn from(s: SourceEntry) -> Self {
        SourceDto {
            document_id: s.document_id,
            document_title: s.document_title,
            section: s.section,
            categories: s.categories,
            asset_key: s.asset_key,
            page: s.page,
            chunk_type: s.chunk_type,
        }
    }
}
