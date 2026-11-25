//! Reads the scope-backfill needs but the document-store port does not expose.
//!
//! `kb-server scope backfill` has to walk every tenant; the port's `list_page`
//! is deliberately tenant-scoped, so the tenant list is a store-adapter detail.

use kb_ports::PortError;
use sqlx::{PgPool, Row};

use crate::queries::SELECT_BACKFILL_TENANTS;

/// One page of live documents with their knowledge bases, for the backfill.
///
/// Deliberately ignores access lists and versions: the backfill is an operator
/// command that only writes the `knowledge_base_id` payload, and an ACL'd or
/// superseded document needs that payload as much as any other. Nothing it
/// reads is returned to an API caller.
const SELECT_BACKFILL_DOCUMENT_KBS: &str = r#"
    SELECT d.id,
           COALESCE(
               array_agg(dg."knowledgeBaseId") FILTER (WHERE dg."knowledgeBaseId" IS NOT NULL),
               '{}'
           ) AS kb_ids
      FROM "Document" d
 LEFT JOIN "DocumentGroup" dg ON dg."documentId" = d.id
     WHERE d."tenantId" = $1
       AND d."deletedAt" IS NULL
       AND ($2::text IS NULL OR d.id > $2)
  GROUP BY d.id
  ORDER BY d.id
     LIMIT $3
"#;

/// Live documents of `tenant` after `after` (by id), each with its knowledge
/// bases, at most `limit`.
///
/// # Errors
///
/// Fails on a database error.
pub async fn document_kb_page(
    pool: &PgPool,
    tenant: &str,
    after: Option<&str>,
    limit: i64,
) -> Result<Vec<(String, Vec<String>)>, PortError> {
    let rows = sqlx::query(SELECT_BACKFILL_DOCUMENT_KBS)
        .bind(tenant)
        .bind(after)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg backfill documents: {e}")))?;
    rows.iter()
        .map(|row| {
            let id: String = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("pg backfill id: {e}")))?;
            let kb_ids: Vec<String> = row
                .try_get("kb_ids")
                .map_err(|e| PortError::Unavailable(format!("pg backfill kb ids: {e}")))?;
            Ok((id, kb_ids))
        })
        .collect()
}

/// Every tenant with at least one live document, in stable order.
///
/// # Errors
///
/// Fails on a database error; an empty result is a valid answer.
pub async fn list_tenants(pool: &PgPool) -> Result<Vec<String>, PortError> {
    let rows = sqlx::query(SELECT_BACKFILL_TENANTS)
        .fetch_all(pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg backfill tenants: {e}")))?;
    rows.iter()
        .map(|row| {
            row.try_get::<String, _>("tenantId")
                .map_err(|e| PortError::Unavailable(format!("pg backfill tenant id: {e}")))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_tenant_query_excludes_soft_deleted_documents() {
        assert!(SELECT_BACKFILL_TENANTS.contains("\"deletedAt\" IS NULL"));
        assert!(SELECT_BACKFILL_TENANTS.contains("DISTINCT \"tenantId\""));
    }

    #[test]
    fn the_document_page_ignores_access_lists_and_versions() {
        assert!(SELECT_BACKFILL_DOCUMENT_KBS.contains("\"deletedAt\" IS NULL"));
        assert!(!SELECT_BACKFILL_DOCUMENT_KBS.contains("aclPrincipals"));
        assert!(!SELECT_BACKFILL_DOCUMENT_KBS.contains("isLatest"));
    }
}
