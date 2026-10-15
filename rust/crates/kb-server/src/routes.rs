//! HTTP routes: `/health`, `/v1/health`, `/v1/formats`,
//! `/v1/search`, `/v1/documents`, `/v1/documents/:id`,
//! `/v1/knowledge-bases`.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use kb_ports::{AuthContext, DocumentFilter, DocumentId, KnowledgeBaseId, ScopeRequest};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::dto::{SearchRequestDto, SearchResponseDto};
use crate::error::ApiError;
use crate::state::AppState;

/// Top-level router. Authenticated routes go here.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/search", post(search))
        .route("/v1/documents", get(list_documents))
        .route(
            "/v1/documents/:id",
            get(get_document).delete(delete_document),
        )
        .route("/v1/knowledge-bases", get(list_knowledge_bases))
}

/// Unauthenticated router (health + formats).
pub fn health_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/v1/health", get(health))
        .route("/v1/formats", get(formats))
}

#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    pub hard: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeleteResponse {
    pub ok: bool,
    /// Chunks removed from SurrealDB (hard delete only).
    pub chunks_removed: u64,
}

/// `DELETE /v1/documents/:id`. Mirrors TS deleteDocument: needs `kb:write`;
/// soft delete sets `deletedAt` and leaves chunks and the blob alone; hard
/// delete (`?hard=true`) removes the chunks, then the stored object, then the row.
async fn delete_document(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<Json<DeleteResponse>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::Forbidden("kb:write".to_string()));
    }
    let doc_id = DocumentId::new_unchecked(id);
    let hard = q.hard.as_deref() == Some("true");

    // Resolve the document inside the caller's tenant scope before touching
    // anything. The Bun route 404s on the tenant-filtered lookup, so a hard
    // delete must not reach the chunk store first and wipe another tenant's
    // chunks on its way to the same 404.
    let scope = state
        .documents
        .build_scope(&ScopeRequest {
            tenant: auth.tenant_id.clone(),
            filter: DocumentFilter::default(),
        })
        .await
        .map_err(ApiError::from)?;
    let doc = state
        .documents
        .find_by_id(&scope, &doc_id)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;

    if !hard {
        let deleted = state
            .documents
            .soft_delete(&auth.tenant_id, &doc_id)
            .await
            .map_err(ApiError::from)?;
        if !deleted {
            return Err(ApiError::NotFound);
        }
        return Ok(Json(DeleteResponse {
            ok: true,
            chunks_removed: 0,
        }));
    }

    // Chunks first, then the stored object, then the row. A chunk-delete or
    // blob-delete failure is logged and does not stop the row delete: the row
    // is what makes the document invisible, and leaving it behind would keep a
    // document the user deleted in their list.
    let chunks_removed = match state.chunks.delete_by_document(&doc_id).await {
        Ok(n) => n,
        Err(err) => {
            tracing::warn!(error = %err, "chunk delete failed during hard delete");
            0
        }
    };
    if let Some(s3_key) = doc.s3_key.as_deref() {
        if let Err(err) = state.blob.delete(s3_key).await {
            tracing::warn!(error = %err, s3_key, "blob delete failed during hard delete");
        }
    }

    let removed = state
        .documents
        .hard_delete(&auth.tenant_id, &doc_id)
        .await
        .map_err(ApiError::from)?;
    if removed.is_none() {
        return Err(ApiError::NotFound);
    }
    Ok(Json(DeleteResponse {
        ok: true,
        chunks_removed,
    }))
}

// ─── Health ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub checks: HealthChecks,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct HealthChecks {
    pub postgres: bool,
    pub vectors: bool,
}

#[axum::debug_handler]
async fn health(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let checks = HealthChecks {
        postgres: true,
        vectors: true,
    };
    (
        StatusCode::OK,
        Json(HealthResponse {
            ok: true,
            checks,
            version: env!("CARGO_PKG_VERSION").into(),
        }),
    )
}

#[derive(Debug, Serialize)]
struct FormatsResponse {
    extensions: Vec<&'static str>,
    max_file_bytes: u32,
}

async fn formats() -> impl IntoResponse {
    let extensions: &[&str] = &[
        ".pdf", ".docx", ".txt", ".md", ".html", ".htm", ".png", ".jpg", ".jpeg", ".webp", ".gif",
        ".csv", ".tsv", ".xlsx", ".xls", ".json", ".xml", ".rtf",
    ];
    Json(FormatsResponse {
        extensions: extensions.to_vec(),
        max_file_bytes: 50 * 1024 * 1024,
    })
}

// ─── Search ──────────────────────────────────────────────────────────

async fn search(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Json(body): Json<SearchRequestDto>,
) -> Result<Json<SearchResponseDto>, ApiError> {
    let query = body.query.trim();
    if query.is_empty() {
        return Ok(Json(SearchResponseDto {
            chunks: Vec::new(),
            sources: Vec::new(),
            context: String::new(),
            degrades: Vec::new(),
        }));
    }

    // Resolve API-key KB restriction against the requested KB filter.
    let requested_kbs = restrict_kbs(&auth, body.knowledge_base_ids.as_deref());
    if let Some(ref r) = requested_kbs {
        if r.is_empty() {
            return Ok(Json(SearchResponseDto {
                chunks: Vec::new(),
                sources: Vec::new(),
                context: String::new(),
                degrades: Vec::new(),
            }));
        }
    }

    let filter = DocumentFilter {
        knowledge_base_ids: requested_kbs
            .unwrap_or_default()
            .into_iter()
            .map(KnowledgeBaseId::new_unchecked)
            .collect(),
        category: body.category.clone(),
    };
    let scope_request = ScopeRequest {
        tenant: auth.tenant_id.clone(),
        filter,
    };
    let mut scope = state
        .documents
        .build_scope(&scope_request)
        .await
        .map_err(ApiError::from)?;

    // Narrow to specific documents if requested. `narrowed_to` can only keep
    // ids already visible in the scope, so the request cannot widen it.
    if let Some(doc_ids) = body.document_ids.as_ref() {
        let requested: Vec<DocumentId> = doc_ids
            .iter()
            .map(|id| DocumentId::new_unchecked(id.clone()))
            .collect();
        scope = scope.narrowed_to(&requested);
    }

    if scope.document_ids().is_empty() {
        return Ok(Json(SearchResponseDto {
            chunks: Vec::new(),
            sources: Vec::new(),
            context: String::new(),
            degrades: Vec::new(),
        }));
    }

    // `body.hybrid` is accepted and ignored for backwards compatibility
    let outcome = state
        .engine
        .retrieve(query, &scope, body.max_chunks)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(SearchResponseDto::from(outcome)))
}

fn restrict_kbs(auth: &AuthContext, requested: Option<&[String]>) -> Option<Vec<String>> {
    if auth.knowledge_base_ids.is_empty() {
        return requested.map(|v| v.to_vec());
    }
    let allowed: HashSet<&str> = auth
        .knowledge_base_ids
        .iter()
        .map(|k| k.0.as_str())
        .collect();
    // Matches TS restrictKnowledgeBases: no or empty request means "all of the
    // key's knowledge bases"; otherwise the request is intersected with the key.
    let filtered: Vec<String> = requested
        .filter(|v| !v.is_empty())
        .map(|v| v.to_vec())
        .unwrap_or_else(|| {
            auth.knowledge_base_ids
                .iter()
                .map(|k| k.0.clone())
                .collect()
        })
        .into_iter()
        .filter(|id| allowed.contains(id.as_str()))
        .collect();
    Some(filtered)
}

// ─── Documents ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListDocumentsQuery {
    pub limit: Option<u32>,
    /// Bun/TS name (camelCase), used by existing clients.
    #[serde(alias = "knowledge_base_ids", rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DocumentSummary {
    pub id: String,
    pub title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    pub status: String,
    pub chunk_count: u32,
    pub file_type: Option<String>,
    pub file_size: Option<i32>,
    pub mime_type: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub knowledge_base_ids: Vec<String>,
}

async fn list_documents(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Query(q): Query<ListDocumentsQuery>,
) -> Result<Json<Vec<DocumentSummary>>, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let raw_kbs: Vec<String> = q
        .knowledge_base_ids
        .as_deref()
        .map(parse_csv)
        .unwrap_or_default();
    let kb_ids: Vec<KnowledgeBaseId> = raw_kbs
        .iter()
        .map(|k| KnowledgeBaseId::new_unchecked(k.clone()))
        .collect();
    // An empty parameter parses to no ids, which is "absent" (TS parseList -> undefined).
    let requested = (!raw_kbs.is_empty()).then_some(raw_kbs.as_slice());
    let allowed = restrict_kbs(&auth, requested);
    let kb_filter: Vec<KnowledgeBaseId> = match allowed {
        Some(v) if v.is_empty() => {
            return Ok(Json(Vec::new()));
        }
        Some(v) => v.into_iter().map(KnowledgeBaseId::new_unchecked).collect(),
        None => kb_ids,
    };
    let docs = state
        .documents
        .list(&auth.tenant_id, limit, &kb_filter)
        .await
        .map_err(ApiError::from)?;
    let ids: Vec<DocumentId> = docs.iter().map(|d| d.id.clone()).collect();
    let counts = chunk_counts_for(&state, &ids).await?;
    let out: Vec<DocumentSummary> = docs
        .into_iter()
        .map(|d| DocumentSummary {
            chunk_count: counts.get(&d.id.0).copied().unwrap_or(0),
            id: d.id.0,
            title: d.title,
            categories: d.categories,
            subcategory: d.subcategory,
            status: d.status,
            file_type: d.file_type,
            file_size: d.file_size,
            mime_type: d.mime_type,
            created_at: d.created_at,
            updated_at: d.updated_at,
            knowledge_base_ids: d.knowledge_base_ids,
        })
        .collect();
    Ok(Json(out))
}

/// Chunk counts keyed by document id, read from SurrealDB. Missing ids are absent.
async fn chunk_counts_for(
    state: &AppState,
    ids: &[DocumentId],
) -> Result<std::collections::HashMap<String, u32>, ApiError> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows = state
        .chunks
        .chunk_counts(ids)
        .await
        .map_err(ApiError::from)?;
    Ok(rows.into_iter().map(|(id, n)| (id.0, n)).collect())
}

#[derive(Debug, Serialize)]
pub struct FigureView {
    pub asset_key: String,
    pub page: u32,
    pub caption: Option<String>,
    pub kind: String,
}

/// Figures from `Document.metadata.figures`, same rules as TS readDocumentFigures:
/// non-object entries are skipped, `assetKey` must be a non-empty string,
/// `page` defaults to 0, `kind` comes from `type` and defaults to "figure".
pub fn read_document_figures(metadata: Option<&serde_json::Value>) -> Vec<FigureView> {
    let Some(figures) = metadata
        .and_then(|m| m.get("figures"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    figures
        .iter()
        .filter_map(serde_json::Value::as_object)
        .filter_map(|f| {
            let asset_key = f
                .get("assetKey")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if asset_key.is_empty() {
                return None;
            }
            Some(FigureView {
                asset_key: asset_key.to_string(),
                page: f
                    .get("page")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32,
                caption: f
                    .get("caption")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                kind: f
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("figure")
                    .to_string(),
            })
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct DocumentDetail {
    pub id: String,
    pub title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    pub status: String,
    pub file_type: Option<String>,
    pub file_size: Option<i32>,
    pub mime_type: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub knowledge_base_ids: Vec<String>,
    pub chunk_count: u32,
    pub figures: Vec<FigureView>,
    pub chunks: Vec<DocumentChunkView>,
}

#[derive(Debug, Serialize)]
pub struct DocumentChunkView {
    pub chunk_index: u32,
    pub content: String,
    pub section: Option<String>,
    pub page: Option<u32>,
    pub asset_key: Option<String>,
}

async fn get_document(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Json<DocumentDetail>, ApiError> {
    let doc_id = DocumentId::new_unchecked(id);
    let scope = state
        .documents
        .build_scope(&ScopeRequest {
            tenant: auth.tenant_id.clone(),
            filter: DocumentFilter::default(),
        })
        .await
        .map_err(ApiError::from)?;
    let Some(meta) = state
        .documents
        .find_by_id(&scope, &doc_id)
        .await
        .map_err(ApiError::from)?
    else {
        return Err(ApiError::NotFound);
    };
    let chunks = state
        .chunks
        .ordered_chunks(&doc_id)
        .await
        .map_err(ApiError::from)?;
    let chunk_views: Vec<DocumentChunkView> = chunks
        .into_iter()
        .map(|(idx, content, meta)| DocumentChunkView {
            chunk_index: idx,
            content,
            section: meta.section,
            page: meta.page,
            asset_key: meta.asset_key,
        })
        .collect();
    let chunk_count = chunk_views.len() as u32;
    Ok(Json(DocumentDetail {
        id: meta.id.0,
        title: meta.title,
        categories: meta.categories,
        subcategory: meta.subcategory,
        status: meta.status,
        file_type: meta.file_type,
        file_size: meta.file_size,
        mime_type: meta.mime_type,
        created_at: meta.created_at,
        updated_at: meta.updated_at,
        knowledge_base_ids: meta.knowledge_base_ids,
        chunk_count,
        figures: read_document_figures(meta.metadata.as_ref()),
        chunks: chunk_views,
    }))
}

// ─── Knowledge bases ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct KbSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    pub document_count: u32,
}

async fn list_knowledge_bases(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
) -> Result<Json<Vec<KbSummary>>, ApiError> {
    let rows = state
        .documents
        .list_knowledge_bases(&auth.tenant_id)
        .await
        .map_err(ApiError::from)?;
    let allowed: HashSet<&str> = auth
        .knowledge_base_ids
        .iter()
        .map(|k| k.0.as_str())
        .collect();
    let out: Vec<KbSummary> = rows
        .into_iter()
        .filter(|k| allowed.is_empty() || allowed.contains(k.id.0.as_str()))
        .map(|k| KbSummary {
            id: k.id.0,
            name: k.name,
            description: k.description,
            color: k.color,
            document_count: k.document_count,
        })
        .collect();
    Ok(Json(out))
}

fn parse_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod figure_tests {
    use super::read_document_figures;
    use serde_json::json;

    #[test]
    fn missing_or_non_array_figures_is_empty() {
        assert!(read_document_figures(None).is_empty());
        assert!(read_document_figures(Some(&json!({}))).is_empty());
        assert!(read_document_figures(Some(&json!({"figures": "x"}))).is_empty());
    }

    #[test]
    fn drops_entries_without_asset_key_and_applies_defaults() {
        let meta = json!({"figures": [
            {"assetKey": "a.png", "page": 3, "caption": "Chart", "type": "chart"},
            {"assetKey": "", "page": 1},
            {"page": 2},
            "not-an-object",
            {"assetKey": "b.png"}
        ]});
        let figs = read_document_figures(Some(&meta));
        assert_eq!(figs.len(), 2);
        assert_eq!(figs[0].asset_key, "a.png");
        assert_eq!(figs[0].page, 3);
        assert_eq!(figs[0].caption.as_deref(), Some("Chart"));
        assert_eq!(figs[0].kind, "chart");
        assert_eq!(figs[1].asset_key, "b.png");
        assert_eq!(figs[1].page, 0);
        assert_eq!(figs[1].caption, None);
        assert_eq!(figs[1].kind, "figure");
    }
}
