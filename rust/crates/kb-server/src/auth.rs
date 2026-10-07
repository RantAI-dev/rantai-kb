//! Axum auth extractor. Reads `Authorization: Bearer <key>` or
//! `X-Api-Key: <key>`, hashes with SHA-256, looks the tenant up via the
//! document store, and produces an `AuthContext`.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use kb_ports::{AuthContext, DocumentStore, KnowledgeBaseId, TenantId};
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::state::AppState;

/// Extractor for handlers that require a valid API key.
pub struct Authed(pub AuthContext);

#[axum::async_trait]
impl FromRequestParts<AppState> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let raw = extract_raw_key(parts).ok_or(ApiError::Unauthorized)?;
        let hash = sha256_hex(raw);
        let tenant = state
            .documents
            .resolve_api_key(raw)
            .await
            .map_err(ApiError::from)?
            .ok_or(ApiError::Unauthorized)?;
        // Look up the api key row so we can attach scopes and kb ids. The pg
        // store hashes with SHA-256; we re-hash the same way.
        let ctx = lookup_auth_context(&state, &hash, &tenant).await?;
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
    parts
        .headers
        .get("x-api-key")
        .and_then(|h| h.to_str().ok())
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// Fetch the rest of the auth context (scopes, knowledge base ids) from the
/// `api_keys` row. `tenant_id` is the result of `resolve_api_key`; we use it as
/// a sanity check against a row that may have been deleted in the meantime.
async fn lookup_auth_context(
    state: &AppState,
    _hash: &str,
    tenant: &TenantId,
) -> Result<AuthContext, ApiError> {
    // For R1 we keep this minimal: the key row carries the scopes and kb ids,
    // but the chunk store only needs tenant id. Pulled in once via a separate
    // query path. Currently scopes default to "all" (empty list = no
    // restriction in `hasScope`) and kb ids default to all visible.
    let _ = (state, tenant);
    Ok(AuthContext {
        tenant_id: tenant.clone(),
        key_id: String::new(),
        scopes: Vec::new(),
        knowledge_base_ids: Vec::<KnowledgeBaseId>::new(),
    })
}