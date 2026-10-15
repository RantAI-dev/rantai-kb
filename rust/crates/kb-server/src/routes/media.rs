//! Document sub-resources: the stored original, figure assets, and the entity
//! graph.
//!
//! All three take a caller-supplied document id, and all three resolve it
//! through Postgres *before* touching object storage or SurrealDB. That check
//! is the whole tenant boundary here: neither store has a tenant of its own, so
//! an id that reached them unchecked would read another tenant's bytes or graph.

use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use kb_ports::{EntityView, RelationView};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::state::AppState;

/// `GET /v1/documents/:id/raw` — streams the stored original.
pub async fn get_document_raw(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let doc = crate::routes::find_visible_document(&state, &auth, &id).await?;
    let meta = state
        .documents
        .find_for_download(&auth.tenant_id, &doc.id)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    let s3_key = meta.s3_key.ok_or(ApiError::NoStoredFile)?;

    let bytes = match state.blob.download(&s3_key).await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(error = %err, document_id = %id, "raw download failed");
            return Err(ApiError::BadGateway(
                "Failed to load the stored file".to_string(),
            ));
        }
    };
    Ok(stream_bytes(
        meta.mime_type
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or("application/octet-stream"),
        None,
        bytes,
    ))
}

#[derive(Debug, Deserialize)]
pub struct AssetQuery {
    /// Storage key of the figure crop.
    pub key: Option<String>,
}

/// `GET /v1/documents/:id/assets?key=…` — streams one figure crop.
pub async fn get_document_asset(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
    Query(q): Query<AssetQuery>,
) -> Result<Response, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let doc = crate::routes::find_visible_document(&state, &auth, &id).await?;

    // SECURITY: this prefix check — not the ownership lookup above — is what
    // stops one tenant from fetching another tenant's (or another one of their
    // OWN documents') figure images by guessing an assetKey. `startsWith` is a
    // literal string test, not a path resolution, so a key containing ".."
    // still starts with the prefix as a string and must be rejected as well.
    // Every failure answers exactly the same 404 as a missing document, so a
    // caller cannot tell "wrong key" from "wrong document" and probe for valid
    // keys.
    let prefix = format!("documents/{}/{}/assets/", auth.tenant_id.0, doc.id.0);
    let Some(key) = q.key.as_deref() else {
        return Err(ApiError::NotFound);
    };
    if !key.starts_with(&prefix) || key.contains("..") || key.contains('\\') {
        return Err(ApiError::NotFound);
    }

    let bytes = match state.blob.download(key).await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(error = %err, document_id = %id, "asset download failed");
            return Err(ApiError::NotFound);
        }
    };
    Ok(stream_bytes(asset_content_type(key), None, bytes))
}

/// Content type for a figure asset, derived from the stored filename. Figure
/// crops are always uploaded as PNG today, but this stays extension-driven so
/// it keeps working if that changes.
fn asset_content_type(key: &str) -> &'static str {
    let ext = key
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

fn stream_bytes(content_type: &str, file_name: Option<&str>, bytes: Vec<u8>) -> Response {
    let mut res = (StatusCode::OK, Body::from(bytes)).into_response();
    let headers = res.headers_mut();
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(CONTENT_TYPE, value);
    }
    if let Some(name) = file_name {
        if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
            headers.insert(CONTENT_DISPOSITION, value);
        }
    }
    res
}

#[derive(Debug, Serialize)]
pub struct IntelligenceResponse {
    pub entities: Vec<EntityView>,
    pub relations: Vec<RelationView>,
    pub status: &'static str,
    pub stats: IntelligenceStats,
}

#[derive(Debug, Serialize)]
pub struct IntelligenceStats {
    #[serde(rename = "totalEntities")]
    pub total_entities: usize,
    #[serde(rename = "totalRelations")]
    pub total_relations: usize,
    #[serde(rename = "entityTypes")]
    pub entity_types: usize,
    #[serde(rename = "relationTypes")]
    pub relation_types: usize,
}

/// `GET /v1/documents/:id/intelligence` — entities and relations extracted for
/// a document.
pub async fn get_document_intelligence(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Json<IntelligenceResponse>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let doc = crate::routes::find_visible_document(&state, &auth, &id).await?;
    let data = state
        .entities
        .get_document_intelligence(&doc.id)
        .await
        .map_err(ApiError::from)?;

    let entity_types: std::collections::HashSet<&str> =
        data.entities.iter().map(|e| e.r#type.as_str()).collect();
    let relation_types: std::collections::HashSet<&str> = data
        .relations
        .iter()
        .map(|r| r.relation_type.as_str())
        .collect();
    let stats = IntelligenceStats {
        total_entities: data.entities.len(),
        total_relations: data.relations.len(),
        entity_types: entity_types.len(),
        relation_types: relation_types.len(),
    };
    Ok(Json(IntelligenceResponse {
        entities: data.entities,
        relations: data.relations,
        status: derive_intelligence_status(&doc.status),
        stats,
    }))
}

/// `Document.status` is only ever ready | processing | failed. Entity
/// extraction runs as an internal step of the job that also chunks and embeds,
/// and its own failures are swallowed as non-fatal, so "ready" means the job
/// reached its terminal state whether or not anything was found. "pending" is
/// never returned: a document is already "processing" from the moment it is
/// created, before any job is claimed.
fn derive_intelligence_status(document_status: &str) -> &'static str {
    match document_status {
        "processing" => "processing",
        "failed" => "failed",
        _ => "completed",
    }
}

#[cfg(test)]
mod tests {
    use super::{asset_content_type, derive_intelligence_status};

    #[test]
    fn asset_content_type_follows_the_extension() {
        assert_eq!(asset_content_type("a/b/fig-1.png"), "image/png");
        assert_eq!(asset_content_type("a/b/fig-1.JPG"), "image/jpeg");
        assert_eq!(asset_content_type("a/b/fig-1.webp"), "image/webp");
        assert_eq!(asset_content_type("a/b/fig-1.gif"), "image/gif");
        assert_eq!(
            asset_content_type("a/b/fig-1.bin"),
            "application/octet-stream"
        );
        assert_eq!(
            asset_content_type("noextension"),
            "application/octet-stream"
        );
    }

    #[test]
    fn intelligence_status_never_reports_pending() {
        assert_eq!(derive_intelligence_status("processing"), "processing");
        assert_eq!(derive_intelligence_status("failed"), "failed");
        assert_eq!(derive_intelligence_status("ready"), "completed");
        assert_eq!(derive_intelligence_status("anything else"), "completed");
    }
}
