//! The `CommunityStore` port: graph community summaries for the global arm
//! (`DEC-5`).
//!
//! # What a community is
//!
//! A set of entities that co-occur across the caller's documents, the chunks
//! those entities came from, and one prose summary written at build time. The
//! builder (`kb-server communities build`) detects the communities in
//! `kb_core::community` and writes them here; the retrieval arm ranks the
//! summaries with BM25 and returns member chunks.
//!
//! # The scope rule
//!
//! **A community returned for a scope contains only that scope's documents and
//! chunks.** A community with no in-scope document is not returned at all.
//!
//! This is deliberately stronger than filtering the *result*: the summary text
//! describes documents the caller may not see, so a community that survived on
//! an out-of-scope member could still steer the caller's ranking. The filter is
//! on the row's members before the arm ever sees the summary, which is why
//! [`CommunityStore::load_communities`] takes a [`DocumentScope`] rather than a
//! tenant.
//!
//! # The summary text never leaves the service
//!
//! The arm uses [`CommunityRecord::summary`] to *select* a community. It is
//! never returned in a chunk, a source or the context string, because it was
//! written from material the caller may not be allowed to read. The answer
//! contains member chunks only, and every one of those is fetched through the
//! scoped `RetrievalIndex` as well.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::PortError;
use crate::ids::{ChunkId, DocumentId, TenantId};
use crate::scope::DocumentScope;

/// One stored community, as the retrieval arm reads it.
///
/// `document_ids` and `chunk_ids` are already narrowed to the scope the read
/// was made for. `entity_keys` are the community's members as the builder
/// detected them; they are not narrowed because an entity is not a document
/// and the arm does not return them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunityRecord {
    /// Storage id (deterministic: `(tenant, version, position)`).
    pub id: String,
    /// Build version. Replaced wholesale on every rebuild.
    pub version: u64,
    /// The summary the arm ranks. Never returned to a caller.
    pub summary: String,
    /// Entity keys in the community, sorted.
    pub entity_keys: Vec<String>,
    /// In-scope documents this community draws on.
    pub document_ids: Vec<DocumentId>,
    /// In-scope chunks assigned to this community.
    pub chunk_ids: Vec<ChunkId>,
    /// Model that wrote the summary (`"none"` for the entity-name baseline).
    pub model: String,
}

/// One community the builder writes. The store assigns the id and version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCommunity {
    pub summary: String,
    pub entity_keys: Vec<String>,
    pub document_ids: Vec<DocumentId>,
    pub chunk_ids: Vec<ChunkId>,
    pub model: String,
}

/// Community persistence.
#[async_trait]
pub trait CommunityStore: Send + Sync {
    /// The tenant's communities, narrowed to `scope`.
    ///
    /// A community with no in-scope document is absent; one with a mix of
    /// in-scope and out-of-scope documents returns only the in-scope document
    /// and chunk ids. An empty scope returns an empty list rather than erroring.
    async fn load_communities(
        &self,
        scope: &DocumentScope,
    ) -> Result<Vec<CommunityRecord>, PortError>;

    /// Replace the tenant's entire community set, atomically.
    ///
    /// The previous version is deleted in the same transaction as the new rows
    /// are inserted, so a reader sees either the old set or the new one, never
    /// a half-written mixture. Returns the version written.
    async fn replace_communities(
        &self,
        tenant: &TenantId,
        communities: Vec<NewCommunity>,
    ) -> Result<u64, PortError>;

    /// The version currently stored for the tenant, `0` when there is none.
    async fn community_version(&self, tenant: &TenantId) -> Result<u64, PortError>;

    /// Liveness probe for the community backend.
    async fn health_check(&self) -> Result<(), PortError> {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_record_serialises_without_the_summary_leaving_the_service() {
        // Not a wire type: the record is internal. The test pins that it stays
        // serialisable (the build command's report uses it) and that the field
        // names are what the adapter reads.
        let record = CommunityRecord {
            id: "c1".into(),
            version: 3,
            summary: "a summary".into(),
            entity_keys: vec!["alpha".into()],
            document_ids: vec![DocumentId::new_unchecked("doc-1")],
            chunk_ids: vec![ChunkId::new_unchecked("doc-1_0")],
            model: "test/model".into(),
        };
        let json = serde_json::to_string(&record).expect("serialise");
        assert!(json.contains("\"summary\""));
        assert!(json.contains("\"document_ids\""));
    }
}
