//! Postgres implementation of the scope outbox port (SEC-2b).
//!
//! Every document write inserts its row through [`insert_outbox`] with the
//! *same* connection the write used, so the row and the change commit or roll
//! back together. Claiming uses `FOR UPDATE SKIP LOCKED` and increments
//! `attempts` inside one transaction; a worker that crashes after a claim loses
//! nothing but its lock.

use async_trait::async_trait;
use kb_ports::{
    DEFAULT_OUTBOX_MAX_ATTEMPTS, DocumentId, OutboxItem, OutboxLag, OutboxOp, PortError,
    ScopeOutbox, TenantId,
};
use sqlx::{PgConnection, PgPool, Row};

use crate::queries::{
    CLAIM_SCOPE_OUTBOX, COMPLETE_SCOPE_OUTBOX, FAIL_SCOPE_OUTBOX, INSERT_SCOPE_OUTBOX,
    SCOPE_OUTBOX_LAG,
};

/// Linear backoff per attempt, in seconds. The schema has no `nextAttemptAt`
/// column, so `createdAt + attempts × 2 s` is the retry clock.
const BACKOFF_STEP_SECS: f64 = 2.0;

/// Insert one repair row using the connection that performed the change.
///
/// This is public so the integration contract can prove the rollback half of
/// the rule: a row inserted on a transaction that is later rolled back must not
/// survive. Production callers always pass the transaction they already hold.
pub async fn insert_outbox(
    conn: &mut PgConnection,
    tenant: &TenantId,
    document: &DocumentId,
    op: OutboxOp,
) -> Result<(), PortError> {
    sqlx::query(INSERT_SCOPE_OUTBOX)
        .bind(document.as_str())
        .bind(tenant.as_str())
        .bind(op.as_str())
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .map_err(|e| PortError::Unavailable(format!("pg insert scope outbox: {e}")))
}

/// Claim on a caller-supplied connection. Exposed for the SKIP LOCKED contract
/// test, which needs two concurrent transactions it controls itself.
pub async fn claim_batch_on(
    conn: &mut PgConnection,
    limit: u32,
) -> Result<Vec<OutboxItem>, PortError> {
    let rows = sqlx::query(CLAIM_SCOPE_OUTBOX)
        .bind(DEFAULT_OUTBOX_MAX_ATTEMPTS)
        .bind(BACKOFF_STEP_SECS)
        .bind(i32::try_from(limit).unwrap_or(i32::MAX))
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg claim scope outbox: {e}")))?;

    rows.iter()
        .map(|row| {
            let op: String = row
                .try_get("op")
                .map_err(|e| PortError::Unavailable(format!("pg outbox op: {e}")))?;
            let id: i64 = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("pg outbox id: {e}")))?;
            let document: String = row
                .try_get("documentId")
                .map_err(|e| PortError::Unavailable(format!("pg outbox document: {e}")))?;
            let tenant: String = row
                .try_get("tenantId")
                .map_err(|e| PortError::Unavailable(format!("pg outbox tenant: {e}")))?;
            let attempts: i32 = row
                .try_get("attempts")
                .map_err(|e| PortError::Unavailable(format!("pg outbox attempts: {e}")))?;
            Ok(OutboxItem {
                id,
                tenant: TenantId(tenant),
                document: DocumentId(document),
                op: OutboxOp::parse(&op)?,
                attempts,
            })
        })
        .collect()
}

async fn claim_batch_pool(pool: &PgPool, limit: u32) -> Result<Vec<OutboxItem>, PortError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PortError::Unavailable(format!("pg tx begin (outbox claim): {e}")))?;
    let items = claim_batch_on(&mut tx, limit).await?;
    tx.commit()
        .await
        .map_err(|e| PortError::Unavailable(format!("pg tx commit (outbox claim): {e}")))?;
    Ok(items)
}

async fn lag_on(pool: &PgPool) -> Result<OutboxLag, PortError> {
    let row = sqlx::query(SCOPE_OUTBOX_LAG)
        .fetch_one(pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg scope outbox lag: {e}")))?;
    let pending: i64 = row
        .try_get("pending")
        .map_err(|e| PortError::Unavailable(format!("pg outbox pending: {e}")))?;
    let oldest: Option<f64> = row
        .try_get("oldest_age")
        .map_err(|e| PortError::Unavailable(format!("pg outbox age: {e}")))?;
    Ok(OutboxLag {
        pending: u64::try_from(pending).unwrap_or(0),
        oldest_pending_secs: oldest.and_then(|age| {
            if age.is_finite() && age >= 0.0 {
                Some(age as u64)
            } else {
                None
            }
        }),
    })
}

#[async_trait]
impl ScopeOutbox for crate::PgDocumentStore {
    async fn claim_batch(&self, limit: u32) -> Result<Vec<OutboxItem>, PortError> {
        claim_batch_pool(self.pool(), limit).await
    }

    async fn complete(&self, ids: &[i64]) -> Result<(), PortError> {
        if ids.is_empty() {
            return Ok(());
        }
        sqlx::query(COMPLETE_SCOPE_OUTBOX)
            .bind(ids)
            .execute(self.pool())
            .await
            .map(|_| ())
            .map_err(|e| PortError::Unavailable(format!("pg complete scope outbox: {e}")))
    }

    async fn fail(&self, id: i64, error: &str) -> Result<(), PortError> {
        sqlx::query(FAIL_SCOPE_OUTBOX)
            .bind(id)
            .bind(error)
            .execute(self.pool())
            .await
            .map(|_| ())
            .map_err(|e| PortError::Unavailable(format!("pg fail scope outbox: {e}")))
    }

    async fn lag(&self) -> Result<OutboxLag, PortError> {
        lag_on(self.pool()).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use kb_ports::OutboxOp;

    use super::*;

    #[test]
    fn the_claim_skips_locked_rows_and_increments_attempts() {
        assert!(CLAIM_SCOPE_OUTBOX.contains("FOR UPDATE SKIP LOCKED"));
        assert!(CLAIM_SCOPE_OUTBOX.contains("\"attempts\" = \"attempts\" + 1"));
        assert!(CLAIM_SCOPE_OUTBOX.contains("\"attempts\" < $1::integer"));
        // The backoff uses the only clock the schema has, not an input value.
        assert!(CLAIM_SCOPE_OUTBOX.contains("\"createdAt\" <="));
    }

    #[test]
    fn the_insert_carries_tenant_document_and_op() {
        assert!(INSERT_SCOPE_OUTBOX.contains("\"documentId\""));
        assert!(INSERT_SCOPE_OUTBOX.contains("\"tenantId\""));
        assert!(INSERT_SCOPE_OUTBOX.contains("op"));
        assert_eq!(OutboxOp::Deleted.as_str(), "deleted");
    }
}
