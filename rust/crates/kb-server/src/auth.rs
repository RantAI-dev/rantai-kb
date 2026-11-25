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

/// Scope that permits a key to assert principals on a request (SEC-3).
pub const ASSERT_PRINCIPALS_SCOPE: &str = "kb:assert-principals";
/// Header a caller uses to assert the principals it holds.
pub const PRINCIPALS_HEADER: &str = "x-kb-principals";
/// Maximum number of principals one request may assert or one ACL may hold.
pub const MAX_PRINCIPALS: usize = 100;
/// Maximum length of one principal.
pub const MAX_PRINCIPAL_LEN: usize = 200;

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
        let mut auth = state
            .documents
            .resolve_api_key(raw)
            .await
            .map_err(ApiError::from)?
            .ok_or(ApiError::Unauthorized)?;

        if !has_scope(&auth, "kb:read") {
            return Err(ApiError::missing_scope("kb:read"));
        }

        // SEC-3: principals are asserted per request, never stored on the key.
        // A leaked ordinary key must not be able to claim `group:admin`.
        let asserted = validate_principals(parse_principals_header(parts))?;
        if !asserted.is_empty() && !has_scope(&auth, ASSERT_PRINCIPALS_SCOPE) {
            return Err(ApiError::missing_scope(ASSERT_PRINCIPALS_SCOPE));
        }
        auth.principals = asserted;

        Ok(Self(auth))
    }
}

/// Check if the auth context has the requested scope.
/// Empty scopes means all scopes are permitted (full access key).
#[must_use]
pub fn has_scope(auth: &AuthContext, scope: &str) -> bool {
    auth.scopes.is_empty() || auth.scopes.iter().any(|s| s == scope || s == "*")
}

/// Read the `X-KB-Principals` header as a comma-separated list. Blank entries
/// are ignored, so a trailing comma is harmless and an absent/empty header is
/// the empty set (default-deny for ACL'd documents).
pub(crate) fn parse_principals_header(parts: &Parts) -> Vec<String> {
    parts
        .headers
        .get(PRINCIPALS_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Validate an asserted principal list (SEC-3), shared by the header, the
/// search body, the upload form and PATCH.
///
/// Each entry is non-empty, at most 200 characters, and matches
/// `^[A-Za-z0-9_.:@-]+$`; at most 100 entries are accepted. A violation is a
/// 400 — never a silently dropped entry, which could widen what a caller sees.
pub(crate) fn validate_principals(values: Vec<String>) -> Result<Vec<String>, ApiError> {
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= MAX_PRINCIPAL_LEN
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:@-".contains(&b))
    };
    if values.len() > MAX_PRINCIPALS {
        return Err(ApiError::BadRequest(format!(
            "'principals' may contain at most {MAX_PRINCIPALS} entries"
        )));
    }
    for value in &values {
        if !valid(value) {
            return Err(ApiError::BadRequest(format!(
                "Invalid principal {value:?}: expected 1..={MAX_PRINCIPAL_LEN} characters from [A-Za-z0-9_.:@-]"
            )));
        }
    }
    Ok(values)
}

/// Whether the caller holds the scope needed to assert principals.
#[must_use]
pub(crate) fn can_assert_principals(auth: &kb_ports::AuthContext) -> bool {
    has_scope(auth, ASSERT_PRINCIPALS_SCOPE)
}

pub(crate) fn extract_raw_key(parts: &Parts) -> Option<&str> {
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
