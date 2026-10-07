//! Typed error for the HTTP layer. Maps cleanly into `IntoResponse`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kb_ports::{EngineError, PortError};
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(&'static str),
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("upstream unavailable: {0}")]
    Unavailable(String),
    #[error("upstream returned malformed data: {0}")]
    Malformed(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Unauthorized => Self::Unauthorized,
            EngineError::Forbidden(scope) => Self::Forbidden(scope),
            EngineError::NotFound => Self::NotFound,
            EngineError::RetrievalFailed(msg) => Self::Unavailable(msg),
        }
    }
}

impl From<PortError> for ApiError {
    fn from(e: PortError) -> Self {
        match e {
            PortError::Invalid(msg) => Self::BadRequest(msg),
            PortError::Unavailable(msg) => Self::Unavailable(msg),
            PortError::Malformed(msg) => Self::Malformed(msg),
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
        match &self {
            ApiError::Unauthorized => render_error(
                StatusCode::UNAUTHORIZED,
                "Unauthorized — supply a key via Authorization: Bearer or X-Api-Key",
                None,
            ),
            ApiError::Forbidden(scope) => render_error(
                StatusCode::FORBIDDEN,
                &format!("API key lacks scope {scope}"),
                None,
            ),
            ApiError::NotFound => render_error(StatusCode::NOT_FOUND, "Not found", None),
            ApiError::BadRequest(msg) => {
                render_error(StatusCode::BAD_REQUEST, "Bad request", Some(msg.as_str()))
            }
            ApiError::Unavailable(msg) => render_error(
                StatusCode::BAD_GATEWAY,
                "Upstream unavailable",
                Some(msg.as_str()),
            ),
            ApiError::Malformed(msg) => render_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Upstream returned malformed data",
                Some(msg.as_str()),
            ),
            ApiError::Internal(msg) => render_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal error",
                Some(msg.as_str()),
            ),
        }
    }
}
