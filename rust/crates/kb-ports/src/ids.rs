//! Newtype ids. They cannot be swapped between kinds even though all wrap `String`.
//!
//! Wrapping `String` rather than `Uuid` keeps these types cheap to construct
//! for tests and consistent with the schema columns (which are `String` in
//! Prisma, and encoded as strings by the Qdrant payload).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Tenant scope. Every row in Postgres is keyed by it; the chunk store filters
/// by it via [`crate::DocumentScope`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TenantId(pub String);

impl TenantId {
    /// Wrap a string that has already been validated upstream.
    pub fn new_unchecked(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Document id. Postgres `Document.id`, also used as the `document_id` filter
/// in the Qdrant payload and every chunk-store query.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocumentId(pub String);

impl DocumentId {
    /// Wrap a string that has already been validated upstream.
    pub fn new_unchecked(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Chunk id. Written by ingest as `{documentId}_{chunkIndex}` and encoded by
/// each adapter into its own point id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChunkId(pub String);

impl ChunkId {
    /// Wrap a string that has already been validated upstream.
    pub fn new_unchecked(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ChunkId {
    /// The document id ingest embedded in this chunk id.
    ///
    /// Ingest names a chunk `{documentId}_{chunkIndex}`, and a document id is a
    /// UUID (hex and dashes, never an underscore), so the last `_` is the
    /// separator. `None` for an id that was not written by that rule.
    ///
    /// This exists for the community arm, whose stored member chunk ids have to
    /// be narrowed to a caller's scope without a second table: the derived
    /// document id is what the scope compares against.
    #[must_use]
    pub fn document_part(&self) -> Option<&str> {
        self.0
            .rsplit_once('_')
            .map(|(document, _index)| document)
            .filter(|document| !document.is_empty())
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Knowledge base id. Postgres `KnowledgeBase.id`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KnowledgeBaseId(pub String);

impl KnowledgeBaseId {
    /// Wrap a string that has already been validated upstream.
    pub fn new_unchecked(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KnowledgeBaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
