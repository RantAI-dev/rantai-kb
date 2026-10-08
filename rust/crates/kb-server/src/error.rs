//! API error types and their HTTP mappings.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kb_ports::{EngineError, PortError};
use serde_json::{Value, json};

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    Forbidden(String),
    NotFound,
    BadRequest(String),
    Unavailable(String),
    Malformed(String),
    Internal(String),
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Unauthorized => Self::Unauthorized,
            EngineError::Forbidden(scope) => Self::Forbidden(scope.to_string()),
            EngineError::NotFound(_) => Self::NotFound,
            EngineError::RetrievalFailed(msg) => Self::Unavailable(msg),
            EngineError::ScopeViolation(_) => Self::Forbidden("kb:scope".to_string()),
        }
    }
}

impl From<PortError> for ApiError {
    fn from(e: PortError) -> Self {
        match e {
            PortError::Invalid(msg) => Self::BadRequest(msg),
            PortError::Unavailable(msg) => Self::Unavailable(msg),
            PortError::Malformed(msg) => Self::Malformed(msg),
            PortError::Storage(msg) => Self::Unavailable(msg),
            PortError::InvalidArgument(msg) => Self::BadRequest(msg),
            PortError::Timeout => Self::Unavailable("operation timed out".to_string()),
        }
    }
}

fn render_error(status: StatusCode, error: &str, detail: Option<&str>) -> Response {
    let mut body = json!({ "error": error });
    if let Some(d) = detail {
        if let Value::Object(ref mut map) = body {
            map.insert("detail".into(), Value::String(d.into()));
        }
    }
    (status, Json(body)).into_response()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => {
                render_error(StatusCode::UNAUTHORIZED, "invalid or missing api key", None)
            }
            Self::Forbidden(scope) => render_error(
                StatusCode::FORBIDDEN,
                "insufficient permissions",
                Some(&format!("missing scope: {scope}")),
            ),
            Self::NotFound => render_error(StatusCode::NOT_FOUND, "not found", None),
            Self::BadRequest(msg) => {
                render_error(StatusCode::BAD_REQUEST, "bad request", Some(&msg))
            }
            Self::Unavailable(msg) => render_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "service unavailable",
                Some(&msg),
            ),
            Self::Malformed(msg) => {
                render_error(StatusCode::BAD_REQUEST, "malformed request", Some(&msg))
            }
            Self::Internal(msg) => render_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error",
                Some(&msg),
            ),
        }
    }
}
