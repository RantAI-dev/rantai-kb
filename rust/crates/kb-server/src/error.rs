//! API error types and their HTTP mappings.
//!
//! Every body is `{"error": "<message>"}` and every message is the string the
//! TypeScript service returns, because clients of this API are written against
//! it. Anything the routes do not recognise is an `Internal` "Internal error",
//! which is also what `handleRequest`'s catch-all produces there.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kb_ports::{EngineError, PortError};
use serde_json::json;

/// Message returned for any request without a usable key
/// (`handleRequest`, src/service/api.ts:876).
pub const UNAUTHORIZED: &str =
    "Unauthorized — supply a key via Authorization: Bearer or X-Api-Key";

/// Message returned when a document or knowledge base is not visible to the
/// caller's tenant. Deliberately the same text everywhere, so a caller cannot
/// tell "wrong key" from "wrong document" and probe for valid ids.
pub const NOT_FOUND: &str = "Not found";

/// Message returned when a job exists but is not in `failed` state.
pub const NOT_FAILED: &str = "Only a failed job can be retried";

#[derive(Debug)]
pub enum ApiError {
    /// 401. No key, or a key that does not resolve to a tenant.
    Unauthorized,
    /// 403. The key authenticated but lacks the scope the route needs.
    Forbidden(&'static str),
    /// 404. Nothing visible to this tenant matches.
    NotFound,
    /// 400. The request itself is malformed or missing a required field.
    BadRequest(String),
    /// 404. The document exists but has no stored original.
    NoStoredFile,
    /// 409. The resource is in a state this route cannot act on.
    Conflict(&'static str),
    /// 413. The upload is larger than the accepted limit.
    PayloadTooLarge(String),
    /// 415. The file extension is not on the accepted list.
    UnsupportedMedia(String),
    /// 502. A backing store the service depends on failed.
    BadGateway(String),
    /// 500. Any failure the service did not anticipate. Logged, never echoed.
    Internal(String),
}

impl ApiError {
    /// The scope a caller must hold for a route, rendered the way
    /// `hasScope` failures read in TypeScript.
    #[must_use]
    pub fn missing_scope(scope: &'static str) -> Self {
        Self::Forbidden(scope)
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Unauthorized => Self::Unauthorized,
            EngineError::Forbidden(scope) => Self::Forbidden(match scope {
                "kb:read" => "kb:read",
                _ => "kb:write",
            }),
            EngineError::NotFound(_) => Self::NotFound,
            // A retrieval failure is logged and reported as the generic
            // internal error the TypeScript catch-all produces.
            EngineError::RetrievalFailed(msg) => Self::Internal(msg),
            EngineError::ScopeViolation(tenant) => Self::Internal(tenant.0),
        }
    }
}

impl From<PortError> for ApiError {
    fn from(e: PortError) -> Self {
        // Store failures are never echoed to the caller: the TypeScript
        // service answers 500 "Internal error" for anything its try/catch sees.
        Self::Internal(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, UNAUTHORIZED.to_string()),
            Self::Forbidden(scope) => (
                StatusCode::FORBIDDEN,
                format!("API key lacks scope {scope}"),
            ),
            Self::NotFound => (StatusCode::NOT_FOUND, NOT_FOUND.to_string()),
            Self::NoStoredFile => (
                StatusCode::NOT_FOUND,
                "Document has no stored file".to_string(),
            ),
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            Self::Conflict(msg) => (StatusCode::CONFLICT, msg.to_string()),
            Self::PayloadTooLarge(msg) => (StatusCode::PAYLOAD_TOO_LARGE, msg),
            Self::BadGateway(msg) => (StatusCode::BAD_GATEWAY, msg),
            Self::UnsupportedMedia(msg) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, msg),
            Self::Internal(msg) => {
                tracing::error!(error = %msg, "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal error".to_string())
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}