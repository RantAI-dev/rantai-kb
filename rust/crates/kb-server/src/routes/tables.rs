//! `/v1/tables` — list, read and query a document's stored tables (`TBL-1`).
//!
//! Authorisation is the document routes' authorisation: the caller's tenant
//! and, for a key that is limited to knowledge bases, the document's knowledge
//! bases. A table in another tenant answers exactly the same 404 as a table
//! that does not exist, so a caller cannot probe ids.
//!
//! A query is a typed [`kb_core::tables::TableQuery`], never SQL. A refused
//! query answers `400` with the [`kb_core::tables::QueryError`] JSON itself, so
//! a client can react to `unknown_column` (and read `available`) instead of
//! parsing a prose message.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kb_core::tables::{DEFAULT_MIN_GROUP, QueryPolicy, TableQuery};
use kb_ports::ids::DocumentId;
use kb_ports::tables::{TableId, TableQueryFailure, TableRecord};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::state::AppState;

/// A table as the API returns it: the record with its manifest, camelCase like
/// the rest of this API.
///
/// `gridKey` is the caller's own blob key. It is included because the record is
/// the contract and an operator debugging a stored table needs it; it names no
/// other tenant's data.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDto {
    pub id: String,
    pub tenant_id: String,
    pub document_id: String,
    pub index: u32,
    pub version: u32,
    pub source: String,
    pub sheet: Option<String>,
    pub page_span: Option<(u32, u32)>,
    pub title: Option<String>,
    pub caption: Option<String>,
    pub header: Vec<String>,
    pub manifest: kb_core::tables::TableManifest,
    pub grid_key: String,
    pub created_at: String,
}

impl From<TableRecord> for TableDto {
    fn from(record: TableRecord) -> Self {
        let source = serde_json::to_value(record.source)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "csv".to_string());
        Self {
            id: record.id.0,
            tenant_id: record.tenant_id.0,
            document_id: record.document_id.0,
            index: record.index,
            version: record.version,
            source,
            sheet: record.sheet,
            page_span: record.page_span,
            title: record.title,
            caption: record.caption,
            header: record.header,
            manifest: record.manifest,
            grid_key: record.grid_key,
            created_at: record.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(rename = "documentId")]
    pub document_id: Option<String>,
}

/// `GET /v1/tables?documentId=…` — the document's tables, current version.
///
/// A missing `documentId` lists every table of the tenant: the same scope the
/// tenant-wide reads use, and the shape a caller needs to discover what it may
/// query after a search.
pub async fn list_tables(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<TableDto>>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let Some(store) = store(&state)? else {
        return Err(ApiError::NotFound);
    };
    let document = match query.document_id.filter(|id| !id.trim().is_empty()) {
        None => None,
        Some(id) => {
            // Resolve through the document route's rules first: a document
            // outside the key's knowledge bases is a 404 here too.
            let doc = crate::routes::find_visible_document(&state, &auth, &id).await?;
            Some(doc.id)
        }
    };
    let records = store
        .list_tables(&auth.tenant_id, document.as_ref())
        .await
        .map_err(ApiError::from)?;
    Ok(Json(records.into_iter().map(TableDto::from).collect()))
}

/// `GET /v1/tables/{id}` — one table with its manifest.
pub async fn get_table(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Json<TableDto>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let Some(store) = store(&state)? else {
        return Err(ApiError::NotFound);
    };
    let record = visible_table(&state, &auth, &store, &id).await?;
    Ok(Json(TableDto::from(record)))
}

/// `POST /v1/tables/{id}/query` — run a typed query and return its rows and
/// receipt.
pub async fn query_table(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let Some(store) = store(&state)? else {
        return Err(ApiError::NotFound);
    };
    let record = visible_table(&state, &auth, &store, &id).await?;
    // The document's privacy setting decides whether this table accepts plain
    // rows at all. A deleted document has no policy to read, and its table is
    // gone with it.
    let meta = state
        .documents
        .find_meta_by_id(&auth.tenant_id, &record.document_id, false)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    if !crate::routes::key_can_see_document(&auth, &meta) {
        return Err(ApiError::NotFound);
    }
    let policy = query_policy(meta.metadata.as_ref());
    let query: TableQuery = match serde_json::from_slice(&body) {
        Ok(query) => query,
        Err(err) => {
            return Err(ApiError::BadRequest(format!(
                "Invalid table query: {err}"
            )));
        }
    };
    match store
        .query(&auth.tenant_id, &record.id, &query, &policy)
        .await
    {
        Ok(result) => Ok(Json(result).into_response()),
        Err(TableQueryFailure::Invalid(error)) => {
            // The QueryError JSON is the body, so its `error` tag and its
            // fields (`column`, `available`, `reason`) reach the caller intact.
            Ok((StatusCode::BAD_REQUEST, Json(error)).into_response())
        }
        Err(TableQueryFailure::Store(err)) => Err(ApiError::from(err)),
    }
}

/// The privacy policy a document's tables answer under.
///
/// `tablePrivacy: "aggregate-only"` in the document's metadata is what the
/// upload route records; anything else (absent, another value) leaves the
/// table open. The minimum group is the contract default, which is what
/// paragon's privacy rule uses.
#[must_use]
pub fn query_policy(metadata: Option<&serde_json::Value>) -> QueryPolicy {
    let aggregate_only = metadata
        .and_then(|m| m.get("tablePrivacy"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("aggregate-only"));
    QueryPolicy {
        aggregate_only,
        min_group: if aggregate_only {
            DEFAULT_MIN_GROUP
        } else {
            0
        },
    }
}

fn store(state: &AppState) -> Result<Option<Arc<dyn kb_ports::TableStore>>, ApiError> {
    Ok(state.tables.clone())
}


/// One table inside the caller's tenant and knowledge-base limit.
async fn visible_table(
    state: &AppState,
    auth: &kb_ports::AuthContext,
    store: &Arc<dyn kb_ports::TableStore>,
    id: &str,
) -> Result<TableRecord, ApiError> {
    let record = store
        .get_table(&auth.tenant_id, &TableId(id.to_string()))
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    // The tenant is already enforced by the store; this is the key's
    // knowledge-base limit, and it must answer 404, not 403.
    let meta = state
        .documents
        .find_meta_by_id(&auth.tenant_id, &record.document_id, false)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    if !crate::routes::key_can_see_document(auth, &meta) {
        return Err(ApiError::NotFound);
    }
    Ok(record)
}

/// The tables belonging to the documents a search answer named, and the titles
/// of the table chunks in it (`TBL-1`).
///
/// One `list_tables` call per distinct document, not per chunk, and none at all
/// when the state has no table store or the answer has no chunks. A store
/// failure here is logged and leaves the answer unchanged: the search found
/// chunks, and losing the convenience of `tables` must not turn a working
/// answer into a 500.
pub(crate) async fn attach_tables(
    state: &AppState,
    tenant: &kb_ports::TenantId,
    response: &mut crate::dto::SearchResponseDto,
) {
    let Some(store) = state.tables.as_ref() else {
        return;
    };
    let mut document_ids: Vec<DocumentId> = Vec::new();
    for chunk in response.chunks.as_deref().unwrap_or_default() {
        // Every hit's document may carry tables, not only the table hits: a
        // caller asking "revenue by region" may get the prose chunk.
        if !document_ids.iter().any(|d| d.0 == chunk.document_id) {
            document_ids.push(DocumentId::new_unchecked(chunk.document_id.clone()));
        }
    }
    let mut titles: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    for document in &document_ids {
        match store.list_tables(tenant, Some(document)).await {
            Ok(records) => {
                for record in records {
                    titles.insert(record.id.0.clone(), record.title.clone());
                    response.tables.push(crate::dto::TableRefDto {
                        table_id: record.id.0,
                        title: record.title,
                        document_id: record.document_id.0,
                    });
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, %document, "could not list a document's tables for a search answer");
            }
        }
    }
    if let Some(chunks) = response.chunks.as_mut() {
        for chunk in chunks {
            if let Some(table_id) = chunk.table_id.as_ref() {
                chunk.table_title = titles.get(table_id).cloned().flatten();
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_aggregate_only_document_gets_the_privacy_policy() {
        let policy = query_policy(Some(&json!({"tablePrivacy": "aggregate-only"})));
        assert!(policy.aggregate_only);
        assert_eq!(policy.min_group, DEFAULT_MIN_GROUP);
    }

    #[test]
    fn an_open_or_missing_setting_reports_whole_groups() {
        for metadata in [None, Some(json!({})), Some(json!({"tablePrivacy": "open"}))] {
            let policy = query_policy(metadata.as_ref());
            assert!(!policy.aggregate_only);
            assert_eq!(policy.min_group, 0);
        }
    }

    #[test]
    fn the_record_maps_to_camel_case_and_keeps_the_manifest() {
        let record = TableRecord {
            id: TableId("doc_t0".to_string()),
            tenant_id: kb_ports::TenantId::new_unchecked("t"),
            document_id: DocumentId::new_unchecked("doc"),
            index: 0,
            version: 2,
            source: kb_core::tables::TableSource::Xlsx,
            sheet: Some("Sheet1".to_string()),
            page_span: None,
            title: Some("Revenue".to_string()),
            caption: None,
            header: vec!["Region".to_string()],
            manifest: kb_core::tables::TableManifest {
                columns: Vec::new(),
                derived_rows: Vec::new(),
                formulas_known: false,
                confidence: kb_core::tables::Confidence::High,
                n_rows: 1,
                n_cols: 1,
                truncated: false,
                notes: Vec::new(),
            },
            grid_key: "tables/t/doc/doc_t0/v2.parquet".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
        };
        let value = serde_json::to_value(TableDto::from(record)).unwrap();
        assert_eq!(value["id"], "doc_t0");
        assert_eq!(value["documentId"], "doc");
        assert_eq!(value["source"], "xlsx");
        assert_eq!(value["manifest"]["confidence"], "high");
        assert_eq!(value["version"], 2);
    }
}
