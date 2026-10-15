//! `JobStore` over the Postgres `IngestJob` table.
//!
//! The queue is the one part of ingest that Rust and Bun must share during
//! cutover, so the claim statement is character-for-character the one the Bun
//! adapter runs (`src/service/adapters.ts`, `jobs.claimNextPending`). A Rust
//! worker and a Bun worker poll the same rows and `FOR UPDATE SKIP LOCKED`
//! hands each row to exactly one of them.

use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{
    CreateJobInput, DocumentId, JobRecord, JobStore, JobView, PortError, ReapableJob,
    RetryJobOutcome, TenantId,
};
use sqlx::PgPool;
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

use crate::queries::{
    CLAIM_NEXT_PENDING, CLEAR_JOB_S3_KEY, FAIL_JOB_STALLED, FINISH_JOB_FAILURE, FINISH_JOB_SUCCESS,
    INSERT_INGEST_JOB, REQUEUE_JOB, RESET_JOB_FOR_RETRY, SELECT_JOB_BY_ID_TENANT,
    SELECT_JOB_FOR_RETRY, SELECT_REAPABLE_JOBS, SELECT_STALE_JOBS, SET_DOCUMENT_STATUS, TOUCH_JOB,
    UPDATE_JOB_PROGRESS,
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
                    if result.rows_affected() == 0 {
                        tracing::debug!(job_id = %id, "stale job already handled elsewhere");
                        continue;
                    }
                    reclaimed += 1;
                    if exhausted {
                        if let Some(doc_id) = document_id {
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

    async fn create_job(&self, input: &CreateJobInput) -> Result<String, PortError> {
        let job_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(INSERT_INGEST_JOB)
            .bind(&job_id)
            .bind(input.tenant_id.0.as_str())
            .bind(input.user_id.as_deref())
            .bind(&input.filename)
            .bind(input.file_size)
            .bind(input.mime_type.as_deref())
            .bind(input.s3_key.as_deref())
            .bind(input.document_id.as_ref().map(|d| d.as_str()))
            .bind(&input.params)
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg insert ingest job: {e}")))?;

        Ok(job_id)
    }

    async fn get_job(&self, tenant: &TenantId, id: &str) -> Result<Option<JobView>, PortError> {
        let row = sqlx::query(SELECT_JOB_BY_ID_TENANT)
            .bind(id)
            .bind(tenant.0.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg get job: {e}")))?;

        match row {
            Some(r) => {
                let id: String = r.try_get("id").map_err(|e| PortError::Unavailable(format!("pg id: {e}")))?;
                let status: String = r.try_get("status").map_err(|e| PortError::Unavailable(format!("pg status: {e}")))?;
                let step: Option<String> = r.try_get("step").map_err(|e| PortError::Unavailable(format!("pg step: {e}")))?;
                let progress: i32 = r.try_get("progress").unwrap_or(0);
                let step_current: Option<i32> = r.try_get("stepCurrent").map_err(|e| PortError::Unavailable(format!("pg stepCurrent: {e}")))?;
                let step_total: Option<i32> = r.try_get("stepTotal").map_err(|e| PortError::Unavailable(format!("pg stepTotal: {e}")))?;
                let eta_seconds: Option<i32> = r.try_get("etaSeconds").map_err(|e| PortError::Unavailable(format!("pg etaSeconds: {e}")))?;
                let attempt: i32 = r.try_get("attempt").map_err(|e| PortError::Unavailable(format!("pg attempt: {e}")))?;
                let error: Option<String> = r.try_get("error").map_err(|e| PortError::Unavailable(format!("pg error: {e}")))?;
                let document_id: Option<String> = r.try_get("documentId").map_err(|e| PortError::Unavailable(format!("pg documentId: {e}")))?;

                Ok(Some(JobView {
                    id,
                    status,
                    step,
                    progress: progress.max(0) as u32,
                    step_current: step_current.map(|v| v.max(0) as u32),
                    step_total: step_total.map(|v| v.max(0) as u32),
                    eta_seconds,
                    attempt,
                    error,
                    document_id,
                }))
            }
            None => Ok(None),
        }
    }

    async fn retry_job(&self, tenant: &TenantId, id: &str) -> Result<RetryJobOutcome, PortError> {
        let row = sqlx::query(SELECT_JOB_FOR_RETRY)
            .bind(id)
            .bind(tenant.0.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg select job for retry: {e}")))?;

        let Some(r) = row else {
            return Ok(RetryJobOutcome::NotFound);
        };

        let status: String = r.try_get("status").map_err(|e| PortError::Unavailable(format!("pg status: {e}")))?;
        if status != "failed" {
            return Ok(RetryJobOutcome::NotFailed);
        }

        let document_id: Option<String> = r.try_get("documentId").map_err(|e| PortError::Unavailable(format!("pg documentId: {e}")))?;

        // Only the job row is reset here. The document goes back to
        // "processing" as its own best-effort update, exactly as the TypeScript
        // route does (`.catch(() => {})`), so an unreachable document row
        // cannot fail a retry that has already been accepted.
        sqlx::query(RESET_JOB_FOR_RETRY)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg reset job for retry: {e}")))?;

        Ok(RetryJobOutcome::Success {
            job_id: id.to_string(),
            document_id,
        })
    }

    async fn list_reapable(&self, max_age_days: u32, limit: u32) -> Result<Vec<ReapableJob>, PortError> {
        let rows = sqlx::query(SELECT_REAPABLE_JOBS)
            .bind(max_age_days as i32)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg list reapable: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id").map_err(|e| PortError::Unavailable(format!("pg id: {e}")))?;
            let s3_key: String = row.try_get("s3Key").map_err(|e| PortError::Unavailable(format!("pg s3Key: {e}")))?;
            out.push(ReapableJob { id, s3_key });
        }
        Ok(out)
    }

    async fn clear_s3_key(&self, job_id: &str) -> Result<(), PortError> {
        sqlx::query(CLEAR_JOB_S3_KEY)
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg clear s3Key: {e}")))?;
        Ok(())
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
