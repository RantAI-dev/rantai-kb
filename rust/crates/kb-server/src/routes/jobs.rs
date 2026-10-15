//! `/v1/jobs/:id` — read a job's progress, or requeue a failed one.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use kb_ports::{DocumentId, JobView, RetryJobOutcome};
use serde::Serialize;

use crate::auth::Authed;
use crate::error::{ApiError, NOT_FAILED};
use crate::state::AppState;

/// `GET /v1/jobs/:id`. No scope check, matching the TypeScript route: any key
/// of the tenant can poll its own jobs, which is how the upload UI polls the
/// job the same key just created.
pub async fn get_job(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Json<JobView>, ApiError> {
    let job = state
        .jobs
        .get_job(&auth.tenant_id, &id)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(job))
}

#[derive(Debug, Serialize)]
pub struct RetryResponse {
    #[serde(rename = "jobId")]
    pub job_id: String,
}

/// `POST /v1/jobs/:id/retry` — only a failed job can be retried (409 otherwise).
/// The row goes back to `pending` with the shape the worker's claim expects, so
/// the next poll picks it up exactly like a fresh job.
pub async fn retry_job(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    match state
        .jobs
        .retry_job(&auth.tenant_id, &id)
        .await
        .map_err(ApiError::from)?
    {
        RetryJobOutcome::NotFound => Err(ApiError::NotFound),
        RetryJobOutcome::NotFailed => Err(ApiError::Conflict(NOT_FAILED)),
        RetryJobOutcome::Success {
            job_id,
            document_id,
        } => {
            // The document goes back to "processing" as its own best-effort
            // update: a document row that cannot be reached must not fail a
            // retry the caller already accepted.
            if let Some(document_id) = document_id {
                if let Err(err) = state
                    .documents
                    .set_status(&DocumentId::new_unchecked(document_id), "processing")
                    .await
                {
                    tracing::warn!(error = %err, "could not reset document status on retry");
                }
            }
            Ok((
                StatusCode::OK,
                Json(RetryResponse { job_id }),
            )
                .into_response())
        }
    }
}