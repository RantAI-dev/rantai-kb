//! Request and response DTOs for `kb-server`.
//!
//! Explicit domain-to-wire conversions matching the TypeScript service's JSON shape.

use kb_core::SourceEntry;
use kb_engine::RetrievalOutcome;
use kb_ports::SearchHit;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct SearchRequestDto {
    pub query: String,
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
    pub chunks: Vec<ChunkDto>,
    pub sources: Vec<SourceDto>,
    pub context: String,
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
    #[serde(default)]
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
    #[serde(rename = "documentId")]
    pub document_id: String,
    #[serde(rename = "documentTitle")]
    pub document_title: String,
    pub content: String,
    #[serde(rename = "chunkIndex")]
    pub chunk_index: Option<u32>,
    #[serde(rename = "chunkType")]
    pub chunk_type: Option<String>,
    pub section: Option<String>,
    pub page: Option<u32>,
    #[serde(rename = "assetKey")]
    pub asset_key: Option<String>,
    #[serde(rename = "contextualPrefix")]
    pub contextual_prefix: Option<String>,
    pub score: f32,
    pub rank: u32,
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
            chunks,
            sources,
            context: outcome.context,
            degrades,
        }
    }
}

impl From<SearchHit> for ChunkDto {
    fn from(hit: SearchHit) -> Self {
        ChunkDto {
            id: hit.chunk.id.0,
            document_id: hit.chunk.document_id.0,
            document_title: hit.chunk.document_title,
            content: hit.chunk.content,
            chunk_index: hit.chunk.metadata.chunk_index,
            chunk_type: Some(hit.chunk.chunk_type.as_storage().to_string()),
            section: hit.chunk.metadata.section,
            page: hit.chunk.metadata.page,
            asset_key: hit.chunk.metadata.asset_key,
            contextual_prefix: hit.chunk.metadata.contextual_prefix,
            score: hit.similarity,
            rank: hit.rank,
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
