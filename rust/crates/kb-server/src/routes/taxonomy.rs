//! `/v1/knowledge-bases` and `/v1/categories`.
//!
//! Both are tenant-scoped CRUD. Deleting a knowledge base removes its links in
//! the same transaction as the base itself and never reaches a document:
//! `Document` has no relation to `KnowledgeBase` at all, only `DocumentGroup`
//! does.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kb_ports::{
    CategoryRecord, CreateCategory, CreateKnowledgeBase, KnowledgeBaseId, KnowledgeBaseRecord,
    PatchCategory, PatchKnowledgeBase,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::Authed;
use crate::dto::parse_json;
use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

/// The body `POST /v1/knowledge-bases` and `PATCH /v1/knowledge-bases/{id}`
/// accept.
#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct KnowledgeBaseBody {
    /// The base's name. Required when creating one.
    pub name: Option<String>,
    /// The base's description.
    pub description: Option<String>,
    /// The base's display colour.
    pub color: Option<String>,
}

/// The row `prisma.knowledgeBase.create` returns, field for field.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeBaseView {
    /// The base's id.
    pub id: String,
    /// The tenant that owns the base.
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    /// The base's name.
    pub name: String,
    /// The base's description, when it has one.
    pub description: Option<String>,
    /// The base's display colour, when it has one.
    pub color: Option<String>,
    /// When the base was created.
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// When the base was last changed.
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

/// The body `PATCH /v1/knowledge-bases/{id}` answers with.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeBaseEnvelope {
    /// The base, after the patch.
    #[serde(rename = "knowledgeBase")]
    knowledge_base: KnowledgeBaseView,
}

/// Create a knowledge base.
///
/// `POST /v1/knowledge-bases` — 201 with the created row. Needs the `kb:write`
/// scope. A base without a name is a 400.
#[utoipa::path(
    post,
    path = "/v1/knowledge-bases",
    tag = "taxonomy",
    request_body = KnowledgeBaseBody,
    responses(
        (status = 201, description = "The created knowledge base", body = KnowledgeBaseView),
        (status = 400, description = "The body has no 'name'", body = ErrorBody),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = ErrorBody),
        (status = 500, description = "The store could not be written", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
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
    Ok((StatusCode::CREATED, Json(KnowledgeBaseView::from(created))).into_response())
}

/// Change a knowledge base.
///
/// `PATCH /v1/knowledge-bases/{id}`. Absent fields are untouched; an empty
/// body returns the row unchanged.
#[utoipa::path(
    patch,
    path = "/v1/knowledge-bases/{id}",
    tag = "taxonomy",
    params(("id" = String, Path, description = "The knowledge base's id.")),
    request_body = KnowledgeBaseBody,
    responses(
        (status = 200, description = "The knowledge base, after the patch", body = KnowledgeBaseEnvelope),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = ErrorBody),
        (status = 404, description = "No such knowledge base in this tenant", body = ErrorBody),
        (status = 500, description = "The store could not be written", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
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

/// Delete a knowledge base.
///
/// `DELETE /v1/knowledge-bases/{id}` — removes the base and its document
/// links.
#[utoipa::path(
    delete,
    path = "/v1/knowledge-bases/{id}",
    tag = "taxonomy",
    params(("id" = String, Path, description = "The knowledge base's id.")),
    responses(
        (status = 200, description = "The knowledge base was removed", body = crate::routes::OkResponse),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = ErrorBody),
        (status = 404, description = "No such knowledge base in this tenant", body = ErrorBody),
        (status = 500, description = "The store could not be written", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
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

/// One category on the wire. The TypeScript service sends the row as-is, so
/// the field names are the column names.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryView {
    /// Row id.
    pub id: String,
    /// Machine name, unique per tenant.
    pub name: String,
    /// Human label.
    pub label: String,
    /// Optional display colour.
    pub color: Option<String>,
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

/// The body `GET /v1/categories` answers with.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryListResponse {
    /// The tenant's categories, ordered by name.
    pub categories: Vec<CategoryView>,
}

/// The body `POST /v1/categories` and `PATCH /v1/categories/{id}` answer with.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryEnvelope {
    /// The category.
    category: CategoryView,
}

/// The body `POST /v1/categories` and `PATCH /v1/categories/{id}` accept.
#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct CategoryBody {
    /// The category's machine name. Required when creating one.
    pub name: Option<String>,
    /// The category's human label. Required when creating one.
    pub label: Option<String>,
    /// The category's display colour.
    pub color: Option<String>,
}

/// List the tenant's categories.
///
/// `GET /v1/categories` — the tenant's categories, ordered by name.
#[utoipa::path(
    get,
    path = "/v1/categories",
    tag = "taxonomy",
    responses(
        (status = 200, description = "The categories", body = CategoryListResponse),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:read scope", body = ErrorBody),
        (status = 500, description = "The store could not be read", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
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

/// Create a category.
///
/// `POST /v1/categories` — 201 with the created category.
#[utoipa::path(
    post,
    path = "/v1/categories",
    tag = "taxonomy",
    request_body = CategoryBody,
    responses(
        (status = 201, description = "The created category", body = CategoryEnvelope),
        (status = 400, description = "The body has no 'name' or no 'label'", body = ErrorBody),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = ErrorBody),
        (status = 500, description = "The store could not be written", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
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

/// Change a category.
///
/// `PATCH /v1/categories/{id}`.
#[utoipa::path(
    patch,
    path = "/v1/categories/{id}",
    tag = "taxonomy",
    params(("id" = String, Path, description = "The category's id.")),
    request_body = CategoryBody,
    responses(
        (status = 200, description = "The category, after the patch", body = CategoryEnvelope),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = ErrorBody),
        (status = 404, description = "No such category in this tenant", body = ErrorBody),
        (status = 500, description = "The store could not be written", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
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

/// Delete a category.
///
/// `DELETE /v1/categories/{id}`.
#[utoipa::path(
    delete,
    path = "/v1/categories/{id}",
    tag = "taxonomy",
    params(("id" = String, Path, description = "The category's id.")),
    responses(
        (status = 200, description = "The category was removed", body = crate::routes::OkResponse),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = ErrorBody),
        (status = 404, description = "No such category in this tenant", body = ErrorBody),
        (status = 500, description = "The store could not be written", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
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
