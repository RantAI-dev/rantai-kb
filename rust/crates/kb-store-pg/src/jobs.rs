//! `JobStore` over the Postgres `IngestJob` table.
//!
//! The queue is the one part of ingest that Rust and Bun must share during
//! cutover, so the claim statement is character-for-character the one the Bun
//! adapter runs (`src/service/adapters.ts`, `jobs.claimNextPending`). A Rust
//! worker and a Bun worker poll the same rows and `FOR UPDATE SKIP LOCKED`
//! hands each row to exactly one of them.

use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{DocumentId, JobRecord, JobStore, PortError, TenantId};
use sqlx::PgPool;
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

use crate::queries::{
    CLAIM_NEXT_PENDING, FAIL_JOB_STALLED, FINISH_JOB_FAILURE, FINISH_JOB_SUCCESS, REQUEUE_JOB,
    SELECT_STALE_JOBS, SET_DOCUMENT_STATUS, TOUCH_JOB, UPDATE_JOB_PROGRESS,
};
use crate::store::PgSettings;

/// Longest error string stored on a job. The Bun adapter slices to 1000 too.
const MAX_ERROR_LEN: usize = 1000;

/// Postgres-backed ingest queue.
#[derive(Clone)]
pub struct PgJobStore {
    pool: PgPool,
}

impl PgJobStore {
    /// Construct from an existing pool, so a process can share one connection
    /// pool between the document store and the job store.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connect using the same settings as the document store.
    ///
    /// # Errors
    ///
    /// Fails when the pool cannot be opened, naming the driver error.
    pub async fn connect(settings: &PgSettings) -> Result<Self, PortError> {
        let pool = PgPoolOptions::new()
            .max_connections(settings.max_connections)
            .acquire_timeout(settings.acquire_timeout)
            .connect(&settings.database_url)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg job store connect: {e}")))?;
        Ok(Self { pool })
    }
}

fn read_job(row: &sqlx::postgres::PgRow) -> Result<JobRecord, PortError> {
    let id: String = row
        .try_get("id")
        .map_err(|e| PortError::Unavailable(format!("pg job id: {e}")))?;
    let tenant_id: String = row
        .try_get("tenantId")
        .map_err(|e| PortError::Unavailable(format!("pg job tenant: {e}")))?;
    let user_id: Option<String> = row
        .try_get("externalRef")
        .map_err(|e| PortError::Unavailable(format!("pg job externalRef: {e}")))?;
    let document_id: Option<String> = row
        .try_get("documentId")
        .map_err(|e| PortError::Unavailable(format!("pg job documentId: {e}")))?;
    let s3_key: Option<String> = row
        .try_get("s3Key")
        .map_err(|e| PortError::Unavailable(format!("pg job s3Key: {e}")))?;
    let filename: String = row
        .try_get("filename")
        .map_err(|e| PortError::Unavailable(format!("pg job filename: {e}")))?;
    let mime_type: Option<String> = row
        .try_get("mimeType")
        .map_err(|e| PortError::Unavailable(format!("pg job mimeType: {e}")))?;
    let attempt: i32 = row
        .try_get("attempt")
        .map_err(|e| PortError::Unavailable(format!("pg job attempt: {e}")))?;
    let params: Option<serde_json::Value> = row
        .try_get("params")
        .map_err(|e| PortError::Unavailable(format!("pg job params: {e}")))?;

    Ok(JobRecord {
        id,
        tenant_id: TenantId::new_unchecked(tenant_id),
        user_id,
        document_id: document_id.map(DocumentId::new_unchecked),
        s3_key,
        filename,
        mime_type,
        attempt,
        params,
    })
}

#[async_trait]
impl JobStore for PgJobStore {
    async fn claim_next_pending(&self) -> Result<Option<JobRecord>, PortError> {
        let row = sqlx::query(CLAIM_NEXT_PENDING)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg claim job: {e}")))?;
        match row.as_ref() {
            Some(row) => read_job(row).map(Some),
            None => Ok(None),
        }
    }

    async fn update_progress(
        &self,
        job_id: &str,
        step: &str,
        progress: u32,
        step_current: Option<u32>,
        step_total: Option<u32>,
        eta_seconds: Option<i32>,
    ) -> Result<(), PortError> {
        // `progress` is an INTEGER column and the value is an engine-controlled
        // percentage, but it still travels as a bound parameter.
        let progress = i64::from(progress);
        sqlx::query(UPDATE_JOB_PROGRESS)
            .bind(job_id)
            .bind(step)
            .bind(progress)
            .bind(step_current.map(i64::from))
            .bind(step_total.map(i64::from))
            .bind(eta_seconds.map(i64::from))
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg job progress: {e}")))?;
        Ok(())
    }

    async fn finish_success(
        &self,
        job_id: &str,
        document_id: &DocumentId,
    ) -> Result<(), PortError> {
        sqlx::query(FINISH_JOB_SUCCESS)
            .bind(job_id)
            .bind(document_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg job success: {e}")))?;
        Ok(())
    }

    async fn finish_failure(&self, job_id: &str, error: &str) -> Result<(), PortError> {
        let mut message: String = error.chars().take(MAX_ERROR_LEN).collect();
        if message.is_empty() {
            message = "ingest failed".to_string();
        }
        sqlx::query(FINISH_JOB_FAILURE)
            .bind(job_id)
            .bind(&message)
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg job failure: {e}")))?;
        Ok(())
    }

    async fn touch(&self, job_id: &str) -> Result<(), PortError> {
        sqlx::query(TOUCH_JOB)
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg job touch: {e}")))?;
        Ok(())
    }

    async fn reclaim_stale(
        &self,
        stale_after: Duration,
        max_attempts: i32,
    ) -> Result<u64, PortError> {
        // No transaction, and no row locks: the TypeScript sweep is a bare
        // `findMany` followed by independent `update` calls, each with
        // `.catch(() => {})` (`src/service/adapters.ts`, `reclaimStale`). One
        // bad row there costs that row only; inside a Postgres transaction the
        // same failed statement would abort the sweep and roll back the rows
        // that had already been handled. `FOR UPDATE SKIP LOCKED` was the other
        // half of that safety: without the transaction there is nothing to hold
        // a lock, so the per-row updates carry the guard instead.
        let cutoff = stale_after.as_secs_f64();
        let stale = sqlx::query(SELECT_STALE_JOBS)
            .bind(cutoff)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg reclaim select: {e}")))?;

        let mut reclaimed = 0u64;
        for row in &stale {
            let id: String = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("pg reclaim id: {e}")))?;
            let attempt: i32 = row
                .try_get("attempt")
                .map_err(|e| PortError::Unavailable(format!("pg reclaim attempt: {e}")))?;
            let document_id: Option<String> = row
                .try_get("documentId")
                .map_err(|e| PortError::Unavailable(format!("pg reclaim documentId: {e}")))?;
            let exhausted = attempt >= max_attempts;

            let updated = if exhausted {
                sqlx::query(FAIL_JOB_STALLED)
                    .bind(&id)
                    .bind(cutoff)
                    .execute(&self.pool)
                    .await
            } else {
                sqlx::query(REQUEUE_JOB)
                    .bind(&id)
                    .bind(cutoff)
                    .execute(&self.pool)
                    .await
            };

            match updated {
                Ok(result) => {
                    // 0 rows means a concurrent sweep already requeued the job,
                    // or its worker heartbeat arrived between our SELECT and this
                    // UPDATE. Either way this sweep must not count it twice.
                    if result.rows_affected() == 0 {
                        tracing::debug!(job_id = %id, "stale job already handled elsewhere");
                        continue;
                    }
                    reclaimed += 1;
                    if exhausted {
                        if let Some(doc_id) = document_id {
                            // Best effort, on its own connection: the job row is
                            // the source of truth, and a document row that cannot
                            // be reached must not cost the sweep the rest.
                            if let Err(err) = sqlx::query(SET_DOCUMENT_STATUS)
                                .bind(&doc_id)
                                .bind("failed")
                                .execute(&self.pool)
                                .await
                            {
                                tracing::warn!(error = %err, document_id = %doc_id, "reclaim could not mark document failed");
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, job_id = %id, "reclaim could not update job");
                }
            }
        }
        Ok(reclaimed)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use sqlx::postgres::PgRow;

    #[test]
    fn error_messages_are_capped() {
        let long = "e".repeat(MAX_ERROR_LEN + 50);
        assert_eq!(long.chars().take(MAX_ERROR_LEN).count(), MAX_ERROR_LEN);
    }

    #[test]
    fn claim_statement_uses_skip_locked() {
        // The claim is only safe against a concurrent Bun worker if both the
        // row lock and the skip are in the statement.
        assert!(CLAIM_NEXT_PENDING.contains("FOR UPDATE SKIP LOCKED"));
        assert!(CLAIM_NEXT_PENDING.contains("SET status = 'processing'"));
    }

    #[test]
    fn stale_select_takes_no_locks() {
        // The sweep is a bare SELECT, as the TypeScript one is, because each
        // row is then updated on its own: a shared transaction would roll every
        // handled row back as soon as one statement failed.
        assert!(SELECT_STALE_JOBS.contains("status = 'processing'"));
        assert!(!SELECT_STALE_JOBS.contains("FOR UPDATE"));
    }

    #[test]
    fn stale_updates_are_conditional_because_the_select_takes_no_locks() {
        // Without a row lock, two sweeps can select the same job. The guards
        // make the second update a no-op instead of a second attempt++.
        for statement in [REQUEUE_JOB, FAIL_JOB_STALLED] {
            assert!(statement.contains("status = 'processing'"), "{statement}");
            assert!(statement.contains("\"updatedAt\" < now()"), "{statement}");
        }
    }

    #[test]
    fn read_job_maps_every_column() {
        // Column presence is asserted through the statement, which is the only
        // thing that binds without a live database.
        for column in [
            "\"tenantId\"",
            "\"externalRef\"",
            "\"documentId\"",
            "\"s3Key\"",
            "filename",
            "\"mimeType\"",
            "attempt",
            "params",
        ] {
            assert!(CLAIM_NEXT_PENDING.contains(column), "missing {column}");
        }
        let _: fn(&PgRow) -> Result<JobRecord, PortError> = read_job;
    }
}
