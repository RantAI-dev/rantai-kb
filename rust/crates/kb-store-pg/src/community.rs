//! `CommunityStore` over the `KbCommunity` table.
//!
//! The storage half of the global (graph community) arm (`DEC-5`). Communities
//! are written by the operator command `kb-server communities build`, never by
//! ingest: the summary pass is a paid LLM batch and AGENTS rule 7 says no paid
//! batch starts without an explicit operator decision.
//!
//! # Replace, don't merge
//!
//! A rebuild replaces the tenant's whole set in one transaction: the old rows
//! are deleted and the new ones inserted together, so a search sees either the
//! old communities or the new ones. Merging would leave communities from two
//! different graphs in one set, and a summary of a graph that no longer exists
//! is worse than no summary.

use async_trait::async_trait;
use kb_ports::{
    ChunkId, CommunityRecord, CommunityStore, DocumentId, DocumentScope, NewCommunity, PortError,
    TenantId,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

use crate::community_queries::{
    DELETE_TENANT_COMMUNITIES, INSERT_COMMUNITY, SELECT_COMMUNITY_VERSION,
    SELECT_SCOPED_COMMUNITIES, SELECT_TENANT_MENTIONS,
};

/// One mention row of the build read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantMention {
    pub entity_key: String,
    pub entity_name: String,
    pub chunk_id: String,
    pub document_id: String,
}

/// Postgres-backed community store.
#[derive(Clone)]
pub struct PgCommunityStore {
    pool: PgPool,
}

impl PgCommunityStore {
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Every entity mention of one tenant, for the build command.
    ///
    /// Tenant-wide by design: the caller is an operator building the tenant's
    /// community set, not a search whose visibility is a scope. The build
    /// command is the only caller.
    pub async fn tenant_mentions(
        &self,
        tenant: &TenantId,
    ) -> Result<Vec<TenantMention>, PortError> {
        let rows = sqlx::query(SELECT_TENANT_MENTIONS)
            .bind(tenant.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg tenant mentions: {e}")))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let entity_key: String = row
                .try_get("entity_key")
                .map_err(|e| PortError::Unavailable(format!("pg mention entity_key: {e}")))?;
            let entity_name: String = row
                .try_get("name")
                .map_err(|e| PortError::Unavailable(format!("pg mention name: {e}")))?;
            let chunk_id: String = row
                .try_get("chunk_id")
                .map_err(|e| PortError::Unavailable(format!("pg mention chunk_id: {e}")))?;
            let document_id: String = row
                .try_get("document_id")
                .map_err(|e| PortError::Unavailable(format!("pg mention document_id: {e}")))?;
            out.push(TenantMention {
                entity_key,
                entity_name,
                chunk_id,
                document_id,
            });
        }
        Ok(out)
    }
}

/// Deterministic row id: `(tenant, version, position)`, UUID-shaped like the
/// graph tables' derived ids. A rebuild writes new ids because the version
/// moved; re-running the same build at the same version would write the same
/// ids, which is what makes the replace idempotent if it is ever retried.
fn community_id(tenant: &TenantId, version: u64, position: usize) -> String {
    let mut hasher = Sha256::new();
    for part in [
        "kb-community",
        tenant.as_str(),
        &version.to_string(),
        &position.to_string(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let hex = hex::encode(&digest[..16]);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Narrow one stored row to a scope.
///
/// Both member lists are filtered: a community that overlaps the scope by one
/// document must not hand the arm the chunks of the other documents. Chunk ids
/// carry the document id ingest embedded (`{documentId}_{index}`), which is how
/// a chunk is matched to the scope without a second table.
fn narrow(
    record: CommunityRecord,
    visible: &std::collections::HashSet<&str>,
) -> Option<CommunityRecord> {
    let document_ids: Vec<DocumentId> = record
        .document_ids
        .into_iter()
        .filter(|document| visible.contains(document.as_str()))
        .collect();
    if document_ids.is_empty() {
        return None;
    }
    let chunk_ids: Vec<ChunkId> = record
        .chunk_ids
        .into_iter()
        .filter(|chunk| {
            chunk
                .document_part()
                .is_some_and(|document| visible.contains(document))
        })
        .collect();
    Some(CommunityRecord {
        document_ids,
        chunk_ids,
        ..record
    })
}

#[async_trait]
impl CommunityStore for PgCommunityStore {
    async fn load_communities(
        &self,
        scope: &DocumentScope,
    ) -> Result<Vec<CommunityRecord>, PortError> {
        // SEC-2a: a predicate scope deliberately has no id list, and the
        // community SQL is an `ANY(ids)` lookup. The arm stays off rather than
        // running unfiltered; both graph and global arms are deprecated (DEC-6).
        if scope.is_predicate() {
            return Ok(Vec::new());
        }
        let documents: Vec<String> = scope
            .document_ids()
            .iter()
            .map(ToString::to_string)
            .collect();
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(SELECT_SCOPED_COMMUNITIES)
            .bind(scope.tenant().as_str())
            .bind(&documents)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg scoped communities: {e}")))?;

        let visible: std::collections::HashSet<&str> = scope
            .document_ids()
            .iter()
            .map(DocumentId::as_str)
            .collect();
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let version: i64 = row
                .try_get("version")
                .map_err(|e| PortError::Unavailable(format!("pg community version: {e}")))?;
            let record = CommunityRecord {
                id: row
                    .try_get("id")
                    .map_err(|e| PortError::Unavailable(format!("pg community id: {e}")))?,
                version: u64::try_from(version).unwrap_or(0),
                summary: row
                    .try_get("summary")
                    .map_err(|e| PortError::Unavailable(format!("pg community summary: {e}")))?,
                entity_keys: row.try_get("entity_keys").unwrap_or_default(),
                document_ids: row
                    .try_get::<Vec<String>, _>("document_ids")
                    .unwrap_or_default()
                    .into_iter()
                    .map(DocumentId)
                    .collect(),
                chunk_ids: row
                    .try_get::<Vec<String>, _>("chunk_ids")
                    .unwrap_or_default()
                    .into_iter()
                    .map(ChunkId)
                    .collect(),
                model: row.try_get("model").unwrap_or_default(),
            };
            if let Some(narrowed) = narrow(record, &visible) {
                out.push(narrowed);
            }
        }
        Ok(out)
    }

    async fn replace_communities(
        &self,
        tenant: &TenantId,
        communities: Vec<NewCommunity>,
    ) -> Result<u64, PortError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg begin: {e}")))?;
        let current: i64 = sqlx::query_scalar(SELECT_COMMUNITY_VERSION)
            .bind(tenant.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg community version: {e}")))?;
        let version = u64::try_from(current).unwrap_or(0).saturating_add(1);

        sqlx::query(DELETE_TENANT_COMMUNITIES)
            .bind(tenant.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg delete communities: {e}")))?;

        for (position, community) in communities.iter().enumerate() {
            let document_ids: Vec<String> = community
                .document_ids
                .iter()
                .map(ToString::to_string)
                .collect();
            let chunk_ids: Vec<String> = community
                .chunk_ids
                .iter()
                .map(ToString::to_string)
                .collect();
            sqlx::query(INSERT_COMMUNITY)
                .bind(community_id(tenant, version, position))
                .bind(tenant.as_str())
                .bind(i64::try_from(version).unwrap_or(i64::MAX))
                .bind(&community.summary)
                .bind(&community.entity_keys)
                .bind(&document_ids)
                .bind(&chunk_ids)
                .bind(&community.model)
                .execute(&mut *tx)
                .await
                .map_err(|e| PortError::Unavailable(format!("pg insert community: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg commit: {e}")))?;
        Ok(version)
    }

    async fn community_version(&self, tenant: &TenantId) -> Result<u64, PortError> {
        let version: i64 = sqlx::query_scalar(SELECT_COMMUNITY_VERSION)
            .bind(tenant.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg community version: {e}")))?;
        Ok(u64::try_from(version).unwrap_or(0))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn record() -> CommunityRecord {
        CommunityRecord {
            id: "c1".into(),
            version: 1,
            summary: "summary".into(),
            entity_keys: vec!["alpha".into()],
            document_ids: vec![
                DocumentId::new_unchecked("doc-a"),
                DocumentId::new_unchecked("doc-b"),
            ],
            chunk_ids: vec![
                ChunkId::new_unchecked("doc-a_0"),
                ChunkId::new_unchecked("doc-b_3"),
            ],
            model: "test/model".into(),
        }
    }

    #[test]
    fn narrowing_keeps_only_the_scopes_documents_and_chunks() {
        let visible: HashSet<&str> = ["doc-a"].into_iter().collect();
        let narrowed = narrow(record(), &visible).expect("one visible document");
        assert_eq!(
            narrowed.document_ids,
            vec![DocumentId::new_unchecked("doc-a")]
        );
        assert_eq!(narrowed.chunk_ids, vec![ChunkId::new_unchecked("doc-a_0")]);
        assert_eq!(narrowed.entity_keys, vec!["alpha".to_string()]);
    }

    #[test]
    fn a_community_entirely_out_of_scope_is_dropped() {
        let visible: HashSet<&str> = ["doc-z"].into_iter().collect();
        assert!(narrow(record(), &visible).is_none());
    }

    /// The scope rule is the `"documentIds" && $2` predicate. If a future edit
    /// drops it, this fails before the leak matrix does.
    #[test]
    fn every_community_statement_binds_the_tenant_and_none_interpolates() {
        // Reads and deletes carry the tenant as a predicate, so no row of
        // another tenant can ever be returned or removed.
        for statement in [
            SELECT_SCOPED_COMMUNITIES,
            SELECT_COMMUNITY_VERSION,
            DELETE_TENANT_COMMUNITIES,
            SELECT_TENANT_MENTIONS,
        ] {
            assert!(
                statement.contains("\"tenantId\" = $1"),
                "a community statement lost its tenant predicate:\n{statement}"
            );
            // Every placeholder is a `$n`; a `format!`-style `{}` or an inline
            // value would mean a caller's input reached the SQL text.
            assert!(!statement.contains("{}"), "interpolated SQL:\n{statement}");
        }
        // The insert names the tenant as a bound column, and the scope read
        // overlaps the member documents with the caller's scope.
        assert!(INSERT_COMMUNITY.contains("\"tenantId\""));
        assert!(INSERT_COMMUNITY.contains("$8"));
        assert!(SELECT_SCOPED_COMMUNITIES.contains(r#""documentIds" && $2"#));
        assert!(SELECT_TENANT_MENTIONS.contains(r#"e."tenantId" = m."tenantId""#));
    }

    #[test]
    fn community_ids_are_deterministic_and_versioned() {
        let tenant = TenantId::new_unchecked("tenant-a");
        assert_eq!(community_id(&tenant, 1, 0), community_id(&tenant, 1, 0));
        assert_ne!(community_id(&tenant, 1, 0), community_id(&tenant, 2, 0));
        assert_ne!(community_id(&tenant, 1, 0), community_id(&tenant, 1, 1));
        assert_eq!(community_id(&tenant, 1, 0).len(), 36);
    }
}
