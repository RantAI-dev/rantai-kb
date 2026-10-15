//! `/v1/knowledge-bases` and `/v1/categories`.
//!
//! Both are tenant-scoped CRUD. Deleting a knowledge base removes its links in
//! the same transaction as the base itself and never reaches a document:
//! `Document` has no relation to `KnowledgeBase` at all, only `DocumentGroup`
//! does.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use kb_ports::{
    CategoryRecord, CreateCategory, CreateKnowledgeBase, KnowledgeBaseId, KnowledgeBaseRecord,
    PatchCategory, PatchKnowledgeBase,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Authed;
use crate::dto::parse_json;
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
struct KnowledgeBaseBody {
    name: Option<String>,
    description: Option<String>,
    color: Option<String>,
}

/// The row `prisma.knowledgeBase.create` returns, field for field.
#[derive(Debug, Serialize)]
pub struct KnowledgeBaseView {
    pub id: String,
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

impl From<KnowledgeBaseRecord> for KnowledgeBaseView {
    fn from(k: KnowledgeBaseRecord) -> Self {
        Self {
            id: k.id.0,
            tenant_id: k.tenant_id.0,
            name: k.name,
            description: k.description,
            color: k.color,
            created_at: k.created_at,
            updated_at: k.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct KnowledgeBaseEnvelope {
    #[serde(rename = "knowledgeBase")]
    knowledge_base: KnowledgeBaseView,
}

/// `POST /v1/knowledge-bases` — 201 with the created row.
pub async fn create_knowledge_base(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    body: Bytes,
) -> Result<Response, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    let body: KnowledgeBaseBody = parse_json(&body);
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return Err(ApiError::BadRequest("Body must include 'name'".to_string()));
    };
    let created = state
        .documents
        .create_knowledge_base(
            &auth.tenant_id,
            &CreateKnowledgeBase {
                id: KnowledgeBaseId::new_unchecked(Uuid::new_v4().to_string()),
                name,
                description: body.description,
                color: body.color,
            },
        )
        .await
        .map_err(ApiError::from)?;
    Ok((
        StatusCode::CREATED,
        Json(KnowledgeBaseView::from(created)),
    )
        .into_response())
}

/// `PATCH /v1/knowledge-bases/:id`. Absent fields are untouched; an empty body
/// returns the row unchanged.
pub async fn patch_knowledge_base(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<KnowledgeBaseEnvelope>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    let kb_id = KnowledgeBaseId::new_unchecked(id);
    let body: KnowledgeBaseBody = parse_json(&body);
    // The store resolves the id inside the tenant before writing, so a foreign
    // id is a 404 that changed nothing.
    let updated = state
        .documents
        .patch_knowledge_base(
            &auth.tenant_id,
            &kb_id,
            &PatchKnowledgeBase {
                name: body.name,
                description: body.description.map(Some),
                color: body.color.map(Some),
            },
        )
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(KnowledgeBaseEnvelope {
        knowledge_base: KnowledgeBaseView::from(updated),
    }))
}

/// `DELETE /v1/knowledge-bases/:id` — removes the base and its document links.
pub async fn delete_knowledge_base(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    let removed = state
        .documents
        .delete_knowledge_base(&auth.tenant_id, &KnowledgeBaseId::new_unchecked(id))
        .await
        .map_err(ApiError::from)?;
    if !removed {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Serialize)]
struct CategoryView {
    id: String,
    name: String,
    label: String,
    color: Option<String>,
}

impl From<CategoryRecord> for CategoryView {
    fn from(c: CategoryRecord) -> Self {
        Self {
            id: c.id,
            name: c.name,
            label: c.label,
            color: c.color,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CategoryListResponse {
    pub categories: Vec<CategoryView>,
}

#[derive(Debug, Serialize)]
pub struct CategoryEnvelope {
    category: CategoryView,
}

#[derive(Debug, Deserialize, Default)]
struct CategoryBody {
    name: Option<String>,
    label: Option<String>,
    color: Option<String>,
}

/// `GET /v1/categories` — the tenant's categories, ordered by name.
pub async fn list_categories(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
) -> Result<Json<CategoryListResponse>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let categories = state
        .documents
        .list_categories(&auth.tenant_id)
        .await
        .map_err(ApiError::from)?
        .into_iter()
        .map(CategoryView::from)
        .collect();
    Ok(Json(CategoryListResponse { categories }))
}

/// `POST /v1/categories` — 201 with the created category.
pub async fn create_category(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    body: Bytes,
) -> Result<Response, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    let body: CategoryBody = parse_json(&body);
    let (Some(name), Some(label)) = (
        body.name.filter(|v| !v.is_empty()),
        body.label.filter(|v| !v.is_empty()),
    ) else {
        return Err(ApiError::BadRequest(
            "Body must include 'name' and 'label'".to_string(),
        ));
    };
    let created = state
        .documents
        .create_category(
            &auth.tenant_id,
            &CreateCategory {
                id: Uuid::new_v4().to_string(),
                name,
                label,
                color: body.color,
            },
        )
        .await
        .map_err(ApiError::from)?;
    Ok((
        StatusCode::CREATED,
        Json(CategoryEnvelope {
            category: CategoryView::from(created),
        }),
    )
        .into_response())
}

/// `PATCH /v1/categories/:id`.
pub async fn patch_category(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<CategoryEnvelope>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    let body: CategoryBody = parse_json(&body);
    let updated = state
        .documents
        .patch_category(
            &auth.tenant_id,
            &id,
            &PatchCategory {
                name: body.name,
                label: body.label,
                color: body.color.map(Some),
            },
        )
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(CategoryEnvelope {
        category: CategoryView::from(updated),
    }))
}

/// `DELETE /v1/categories/:id`.
pub async fn delete_category(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    let removed = state
        .documents
        .delete_category(&auth.tenant_id, &id)
        .await
        .map_err(ApiError::from)?;
    if !removed {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}