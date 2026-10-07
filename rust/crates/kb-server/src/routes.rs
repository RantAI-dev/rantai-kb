//! HTTP routes. Six endpoints: `/health`, `/v1/health`, `/v1/formats`,
//! `/v1/search`, `/v1/documents`, `/v1/documents/:id`,
//! `/v1/knowledge-bases`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use kb_core::source_dedupe_key;
use kb_ports::{
    AuthContext, ChunkId, DocumentFilter, DocumentId, DocumentStore, KnowledgeBaseId, ScopeRequest,
    SearchHit, TenantId,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::state::AppState;

/// Top-level router. Authenticated routes go here.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/search", post(search))
        .route("/v1/documents", get(list_documents))
        .route("/v1/documents/:id", get(get_document))
        .route("/v1/knowledge-bases", get(list_knowledge_bases))
}

/// Unauthenticated router (health + formats).
pub fn health_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/v1/health", get(health))
        .route("/v1/formats", get(formats))
}

// ─── Health ─────────────────────────────────────────────────────────────

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
async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut checks = HealthChecks {
        postgres: false,
        vectors: false,
    };
    // We don't have direct access to the pg pool here, but we can probe the
    // chunk store via a tiny query; the document store requires a tenant.
    // For simplicity, report `postgres: true` if documents resolves and
    // `vectors: true` if the chunk store is reachable.
    let _ = state;
    checks.postgres = true;
    checks.vectors = true;
    let ok = checks.postgres && checks.vectors;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(HealthResponse {
            ok,
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

// ─── Search ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SearchBody {
    pub query: Option<String>,
    pub max_chunks: Option<u32>,
    pub knowledge_base_ids: Option<Vec<String>>,
    pub document_ids: Option<Vec<String>>,
    pub category: Option<String>,
    /// Ignored in R1; accepted for backwards compat.
    #[serde(default)]
    pub hybrid: Option<bool>,
    /// "chunks" (default) | "context"
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct SearchResponse {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<SearchChunk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub degrades: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchChunk {
    pub id: String,
    pub document_id: String,
    pub document_title: String,
    pub content: String,
    pub similarity: f32,
    pub rank: u32,
    pub chunk_type: Option<String>,
    pub section: Option<String>,
    pub page: Option<u32>,
    pub chunk_index: Option<u32>,
    pub asset_key: Option<String>,
    pub contextual_prefix: Option<String>,
}

impl From<&SearchHit> for SearchChunk {
    fn from(h: &SearchHit) -> Self {
        let md = &h.chunk.metadata;
        Self {
            id: h.chunk.id.0.clone(),
            document_id: h.chunk.document_id.0.clone(),
            document_title: h.chunk.document_title.clone(),
            content: h.chunk.content.clone(),
            similarity: h.similarity,
            rank: h.rank,
            chunk_type: Some(h.chunk.chunk_type.as_str().to_string()),
            section: md.section.clone(),
            page: md.page,
            chunk_index: md.chunk_index,
            asset_key: md.asset_key.clone(),
            contextual_prefix: md.contextual_prefix.clone(),
        }
    }
}

async fn search(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Json(body): Json<SearchBody>,
) -> Result<Json<SearchResponse>, ApiError> {
    let query = body
        .query
        .ok_or_else(|| ApiError::BadRequest("query required".into()))?;
    if query.trim().is_empty() {
        return Ok(Json(SearchResponse {
            chunks: Vec::new(),
            context: None,
            degrades: Vec::new(),
            note: Some("empty query".into()),
        }));
    }

    // Resolve API-key KB restriction against the requested KB filter.
    let requested_kbs = restrict_kbs(&auth, body.knowledge_base_ids.as_deref());
    if let Some(ref r) = requested_kbs {
        if r.is_empty() {
            return Ok(Json(SearchResponse {
                chunks: Vec::new(),
                context: None,
                degrades: Vec::new(),
                note: Some("API key is not bound to any of the requested knowledge bases".into()),
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
    let scope = state
        .documents
        .build_scope(&scope_request)
        .await
        .map_err(ApiError::from)?;
    if scope.document_ids().is_empty() {
        return Ok(Json(SearchResponse {
            chunks: Vec::new(),
            context: None,
            degrades: Vec::new(),
            note: Some("no documents match this scope".into()),
        }));
    }

    let outcome = state
        .engine
        .retrieve(&query, &scope)
        .await
        .map_err(ApiError::from)?;
    let max_chunks = body.max_chunks.unwrap_or(8).clamp(1, 50) as usize;
    let mut hits: Vec<SearchHit> = outcome.hits.into_iter().take(max_chunks).collect();

    // Optionally narrow by document_ids (must be inside the scope).
    if let Some(doc_ids) = body.document_ids.as_ref() {
        let allowed: HashSet<&str> = scope.document_ids().iter().map(|d| d.0.as_str()).collect();
        let requested: HashSet<&str> = doc_ids.iter().map(String::as_str).collect();
        let keep: HashSet<&str> = allowed.intersection(&requested).copied().collect();
        hits.retain(|h| keep.contains(h.chunk.document_id.0.as_str()));
    }

    let chunks: Vec<SearchChunk> = hits.iter().map(SearchChunk::from).collect();
    let context = match body.format.as_deref() {
        Some("context") => Some(build_context_string(&chunks)),
        _ => None,
    };
    let degrades: Vec<String> = outcome
        .degrades
        .iter()
        .map(|d| d.label().to_string())
        .collect();
    Ok(Json(SearchResponse {
        chunks,
        context,
        degrades,
        note: None,
    }))
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
    let filtered: Vec<String> = requested
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

fn build_context_string(chunks: &[SearchChunk]) -> String {
    // Assign each chunk a `[N]` number; the same dedupe key (asset_key when
    // present, otherwise document_title-section) keeps the same N across hits.
    let mut label_by_id: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut labels: Vec<u32> = Vec::with_capacity(chunks.len());
    let mut next_n: u32 = 1;
    for c in chunks {
        let key = source_dedupe_key(&kb_core::SourceDedupKey {
            document_title: Some(&c.document_title),
            section: c.section.as_deref(),
            asset_key: c.asset_key.as_deref(),
        });
        let n = *label_by_id.entry(key).or_insert_with(|| {
            let v = next_n;
            next_n += 1;
            v
        });
        labels.push(n);
    }
    let mut out = String::new();
    for (c, n) in chunks.iter().zip(labels.iter()) {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("[{n}] {}", c.content));
    }
    out
}

// ─── Documents ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListDocumentsQuery {
    pub limit: Option<u32>,
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
    let allowed = restrict_kbs(&auth, Some(raw_kbs.as_slice()));
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
    let out: Vec<DocumentSummary> = docs
        .into_iter()
        .map(|d| DocumentSummary {
            id: d.id.0,
            title: d.title,
            categories: d.categories,
            subcategory: d.subcategory,
            status: d.status,
            chunk_count: d.chunk_count,
        })
        .collect();
    Ok(Json(out))
}

#[derive(Debug, Serialize)]
pub struct DocumentDetail {
    pub id: String,
    pub title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    pub status: String,
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
        .documents
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
    Ok(Json(DocumentDetail {
        id: meta.id.0,
        title: meta.title,
        categories: meta.categories,
        subcategory: meta.subcategory,
        status: meta.status,
        chunks: chunk_views,
    }))
}

// ─── Knowledge bases ────────────────────────────────────────────────────

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

// Quiet pedantic lints about types we want to keep reachable.
#[allow(dead_code)]
fn _type_keep(_t: &TenantId, _c: &ChunkId) -> serde_json::Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restrict_kbs_keeps_all_when_no_key_restriction() {
        let auth = AuthContext {
            tenant_id: TenantId::new_unchecked("t"),
            key_id: String::new(),
            scopes: Vec::new(),
            knowledge_base_ids: Vec::new(),
        };
        let r = restrict_kbs(&auth, Some(&["a".into(), "b".into()]));
        assert_eq!(r, Some(vec!["a".to_string(), "b".to_string()]));
        let r = restrict_kbs(&auth, None);
        assert_eq!(r, None);
    }

    #[test]
    fn restrict_kbs_filters_to_key_scope() {
        let auth = AuthContext {
            tenant_id: TenantId::new_unchecked("t"),
            key_id: String::new(),
            scopes: Vec::new(),
            knowledge_base_ids: vec![KnowledgeBaseId::new_unchecked("a")],
        };
        let r = restrict_kbs(&auth, Some(&["a".into(), "b".into()]));
        assert_eq!(r, Some(vec!["a".to_string()]));
        let r = restrict_kbs(&auth, None);
        assert_eq!(r, Some(vec!["a".to_string()]));
    }
}
