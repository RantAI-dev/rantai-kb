//! API key authentication extractor.
//!
//! Expects `Authorization: Bearer <key>` or `X-API-Key: <key>`.
//! Rejects immediately if neither is present. Resolves tenant and scopes
//! via `DocumentStore::resolve_api_key`.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use kb_ports::AuthContext;

use crate::error::ApiError;
use crate::state::AppState;

/// Extracted, authenticated context. Only exists if the presented API key
/// is valid and has at least `kb:read` scope.
pub struct Authed(pub AuthContext);

#[axum::async_trait]
impl FromRequestParts<Arc<AppState>> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let raw = extract_raw_key(parts).ok_or(ApiError::Unauthorized)?;
        let auth = state
            .documents
            .resolve_api_key(raw)
            .await
            .map_err(ApiError::from)?
            .ok_or(ApiError::Unauthorized)?;

        if !has_scope(&auth, "kb:read") {
            return Err(ApiError::missing_scope("kb:read"));
        }

        Ok(Self(auth))
    }
}

/// Check if the auth context has the requested scope.
/// Empty scopes means all scopes are permitted (full access key).
#[must_use]
pub fn has_scope(auth: &AuthContext, scope: &str) -> bool {
    auth.scopes.is_empty() || auth.scopes.iter().any(|s| s == scope || s == "*")
}

fn extract_raw_key<'a>(parts: &'a Parts) -> Option<&'a str> {
    if let Some(auth) = parts.headers.get("Authorization") {
        if let Ok(val) = auth.to_str() {
            // `header.toLowerCase().startsWith("bearer ")` in TypeScript: the
            // scheme is matched case-insensitively, and the value that follows
            // is the original text trimmed.
            if val.len() > 7 && val[..7].eq_ignore_ascii_case("bearer ") {
                let trimmed = val[7..].trim();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }
    }

    if let Some(val) = parts.headers.get("x-api-key") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    None
}
