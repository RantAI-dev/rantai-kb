//! Reads the scope-backfill needs but the document-store port does not expose.
//!
//! `kb-server scope backfill` has to walk every tenant; the port's `list_page`
//! is deliberately tenant-scoped, so the tenant list is a store-adapter detail.

use kb_ports::PortError;
use sqlx::{PgPool, Row};

use crate::queries::SELECT_BACKFILL_TENANTS;

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
}
