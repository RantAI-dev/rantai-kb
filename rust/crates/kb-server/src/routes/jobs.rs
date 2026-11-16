//! `/v1/jobs/:id` — read a job's progress, or requeue a failed one.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kb_ports::{DocumentId, JobView, RetryJobOutcome};
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::Authed;
use crate::error::{ApiError, NOT_FAILED};
use crate::state::AppState;

/// Read an ingest job's progress.
///
/// `GET /v1/jobs/{id}`. No scope check, matching the TypeScript route: any key
/// of the tenant can poll its own jobs, which is how the upload UI polls the
/// job the same key just created.
#[utoipa::path(
    get,
    path = "/v1/jobs/{id}",
    tag = "jobs",
    params(("id" = String, Path, description = "The job's id.")),
    responses(
        (status = 200, description = "The job's progress", body = crate::openapi::mirrors::JobView),
        (status = 401, description = "No key, or a key that does not resolve", body = crate::error::ErrorBody),
        (status = 403, description = "The key lacks the kb:read scope", body = crate::error::ErrorBody),
        (status = 404, description = "No such job, or its document is not visible to this key", body = crate::error::ErrorBody),
        (status = 500, description = "The store could not be read", body = crate::error::ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
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
    // A job names the document it ingests; a key limited to some knowledge
    // bases must not learn about (or watch) one outside them. Unlimited keys
    // are not looked up at all, so their behaviour is unchanged.
    if let Some(document_id) = job.document_id.as_deref() {
        crate::routes::ensure_document_visible(&state, &auth, document_id).await?;
    }
    Ok(Json(job))
}

/// The body `POST /v1/jobs/{id}/retry` answers with.
#[derive(Debug, Serialize, ToSchema)]
pub struct RetryResponse {
    /// The job that was requeued.
    #[serde(rename = "jobId")]
    pub job_id: String,
}

/// Requeue a failed ingest job.
///
/// `POST /v1/jobs/{id}/retry` — only a failed job can be retried (409
/// otherwise). The row goes back to `pending` with the shape the worker's
/// claim expects, so the next poll picks it up exactly like a fresh job.
#[utoipa::path(
    post,
    path = "/v1/jobs/{id}/retry",
    tag = "jobs",
    params(("id" = String, Path, description = "The job's id.")),
    responses(
        (status = 200, description = "The job was requeued", body = RetryResponse),
        (status = 401, description = "No key, or a key that does not resolve", body = crate::error::ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = crate::error::ErrorBody),
        (status = 404, description = "No such job, or its document is not visible to this key", body = crate::error::ErrorBody),
        (status = 409, description = "The job is not in the failed state", body = crate::error::ErrorBody),
        (status = 500, description = "The store could not be written", body = crate::error::ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn retry_job(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    // A limited key must answer 404, and requeue nothing, for a job whose
    // document is outside its knowledge bases. Only limited keys pay this
    // lookup: an unlimited key goes straight to the retry as before.
    if !auth.knowledge_base_ids.is_empty() {
        let job = state
            .jobs
            .get_job(&auth.tenant_id, &id)
            .await
            .map_err(ApiError::from)?
            .ok_or(ApiError::NotFound)?;
        if let Some(document_id) = job.document_id.as_deref() {
            crate::routes::find_document(&state, &auth, document_id, false).await?;
        }
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
            Ok((StatusCode::OK, Json(RetryResponse { job_id })).into_response())
        }
    }
}
