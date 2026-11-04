//! Document intelligence domain types.
//!
//! Mirrors definitions from `src/lib/document-intelligence/types.ts`.

use serde::{Deserialize, Serialize};

/// Extracted entity from document text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    /// Entity name / surface form
    pub name: String,
    /// Entity type (e.g. "Person", "Organization", "Location", "Email", "URL")
    #[serde(rename = "type")]
    pub entity_type: String,
    /// Confidence score (0.0 to 1.0)
    pub confidence: f32,
    /// Document ID this entity belongs to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    /// Chunk ID where entity was found
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    /// Additional metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Extracted relation between two entities in a document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Relation {
    /// Relation type (e.g. "WORKS_FOR", "PART_OF", "CITES", "RELATED_TO")
    pub relation_type: String,
    /// Confidence score (0.0 to 1.0)
    pub confidence: f32,
    /// Source entity name
    pub source_entity: String,
    /// Target entity name
    pub target_entity: String,
    /// Supporting text excerpt / context
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Additional metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Configuration for pattern-based entity extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternExtractionConfig {
    /// Extract email, URL, phone, dates
    pub extract_universal: bool,
    /// Extract currency, percentages
    pub extract_financial: bool,
    /// Extract API endpoints, function calls, env vars, error codes
    pub extract_technical: bool,
}

impl Default for PatternExtractionConfig {
    fn default() -> Self {
        Self {
            extract_universal: true,
            extract_financial: true,
            extract_technical: true,
        }
    }
}

/// Configuration for pattern-based relation extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternRelationConfig {
    /// Extract universal relations (MENTIONS, PART_OF, CITES, WORKS_FOR, etc.)
    pub extract_universal: bool,
    /// Extract Indonesian legal relations (IMPLEMENTS, REFINES, SUPERSEDES, etc.)
    pub extract_legal: bool,
    /// Extract financial relations (PAYS, OWNED_BY, TRANSFERS)
    pub extract_financial: bool,
    /// Extract technical relations (CALLS, THROWS, DEPENDS_ON, RETURNS)
    pub extract_technical: bool,
}

impl Default for PatternRelationConfig {
    fn default() -> Self {
        Self {
            extract_universal: true,
            extract_legal: true,
            extract_financial: true,
            extract_technical: true,
        }
    }
}
