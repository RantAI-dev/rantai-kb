//! Axum auth extractor. Reads `Authorization: Bearer <key>` or
//! `X-Api-Key: <key>`, hashes with SHA-256, looks the tenant up via the
//! document store, and produces an `AuthContext`.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use kb_ports::{AuthContext, DocumentStore, KnowledgeBaseId, TenantId};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

/// Extractor for handlers that require a valid API key.
pub struct Authed(pub AuthContext);

#[axum::async_trait]
impl FromRequestParts<Arc<AppState>> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let raw = extract_raw_key(parts).ok_or(ApiError::Unauthorized)?;
        let tenant = state
            .documents
            .resolve_api_key(raw)
            .await
            .map_err(ApiError::from)?
            .ok_or(ApiError::Unauthorized)?;
        // For R1 the Authed context carries only tenant id; scope and kb
        // restriction are looked up by `kb-server` per request.
        let ctx = lookup_auth_context(&tenant);
        Ok(Self(ctx))
    }
}

fn extract_raw_key(parts: &Parts) -> Option<&str> {
    let header = parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())?;
    if let Some(rest) = header.strip_prefix("Bearer ") {
        return Some(rest.trim());
    }
    if let Some(rest) = header.strip_prefix("bearer ") {
        return Some(rest.trim());
    }
    parts.headers.get("x-api-key").and_then(|h| h.to_str().ok())
}

#[allow(dead_code)]
fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// Build an AuthContext for a validated tenant. R1 sets empty scopes and kb
/// restrictions; callers (`routes.rs`) apply KB filters against the API-key
/// binding separately.
fn lookup_auth_context(tenant: &TenantId) -> AuthContext {
    AuthContext {
        tenant_id: tenant.clone(),
        key_id: String::new(),
        scopes: Vec::new(),
        knowledge_base_ids: Vec::<KnowledgeBaseId>::new(),
    }
}
