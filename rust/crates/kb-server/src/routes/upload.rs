//! `POST /v1/documents` — multipart upload.
//!
//! Mirrors `ingestDocument` in `src/service/api.ts`: the file is validated, the
//! original is written to object storage, a document row and its knowledge-base
//! links are created, and a pending ingest job is enqueued. The response is
//! `202` because nothing is indexed yet — a worker picks the job up.

use std::sync::Arc;

use axum::Json;
use axum::extract::{FromRequest, Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kb_core::policy::{FigureMode, MimeRegistry};
use kb_ports::{CreateJobInput, DocumentId, KnowledgeBaseId, NewDocument, document_path};
use serde::Serialize;
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

struct Upload {
    file_name: String,
    content_type: Option<String>,
    bytes: Vec<u8>,
    fields: Vec<(String, String)>,
}

impl Upload {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// First field with a name, trimmed-empty treated as absent, matching the
    /// `|| default` chain the TypeScript route uses.
    fn field_or<'a>(&'a self, name: &str, fallback: &'a str) -> &'a str {
        match self.field(name) {
            Some(v) if !v.is_empty() => v,
            _ => fallback,
        }
    }
}

/// Parse the optional `metadata` multipart field: a JSON object, or nothing.
///
/// The whole object is stored as `Document.metadata`; the keys are not
/// interpreted here (a filter or a boost names the key it wants later). A
/// malformed value is the route's own 400, so a caller learns what to fix
/// instead of getting a parser message.
fn parse_upload_metadata(value: Option<&str>) -> Result<Option<serde_json::Value>, ApiError> {
    let Some(raw) = value.filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
        ApiError::BadRequest(format!(
            "Invalid 'metadata': expected a JSON object (e.g. {{\"effectiveDate\": \"2025-03-01\"}}): {e}"
        ))
    })?;
    if !parsed.is_object() {
        return Err(ApiError::BadRequest(
            "Invalid 'metadata': expected a JSON object of key/value pairs".to_string(),
        ));
    }
    Ok(Some(parsed))
}

/// Accepted extension check, including the extensionless case: `slice(lastIndexOf("."))`
/// returns the whole name when there is no dot, so "README" is tested as "readme".
fn is_accepted_extension(file_name: &str) -> bool {
    let ext = match file_name.rfind('.') {
        Some(idx) => &file_name[idx..],
        None => file_name,
    }
    .to_lowercase();
    MimeRegistry::kb_accepted_extensions()
        .iter()
        .any(|accepted| *accepted == ext)
}

/// `title` defaults to the filename without its extension.
fn title_from_file_name(file_name: &str) -> String {
    match file_name.rfind('.') {
        Some(idx) if idx > 0 => file_name[..idx].to_string(),
        Some(_) | None => file_name.to_string(),
    }
}

/// The multipart form `POST /v1/documents` accepts.
///
/// The file is the part named `file`; every other part is a text field. The
/// schema names them all — with the names the form really uses, camelCase
/// included — so a reader of the spec knows the whole form without reading
/// the route.
#[derive(Debug, ToSchema)]
pub struct UploadForm {
    /// The document itself. Its extension decides whether it is accepted.
    #[schema(value_type = String, format = Binary)]
    pub file: String,
    /// The document's title. Defaults to the file name without its extension.
    #[schema(value_type = Option<String>)]
    pub title: Option<String>,
    /// The categories the document is filed under, as a JSON array or a
    /// comma-separated list.
    #[schema(value_type = Option<String>)]
    pub categories: Option<String>,
    /// The subcategory, when the document has one.
    #[schema(value_type = Option<String>)]
    pub subcategory: Option<String>,
    /// The caller's own reference for this document.
    #[schema(value_type = Option<String>, rename = "externalRef")]
    pub external_ref: Option<String>,
    /// A JSON object of extra metadata, stored as the document's JSONB
    /// metadata and read by the `metadata` search filter.
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<serde_json::Value>,
    /// `aggregate-only` restricts this document's tables to whole-group
    /// aggregates; `open` (the default) leaves them open.
    #[schema(value_type = Option<String>, rename = "tablePrivacy")]
    pub table_privacy: Option<String>,
    /// The kind of document, as the extractor should read it.
    #[schema(value_type = Option<String>, rename = "documentType")]
    pub document_type: Option<String>,
    /// How hard to look for figures: `auto`, `all`, `none` or `off`.
    #[schema(value_type = Option<String>)]
    pub figures: Option<String>,
    /// The knowledge bases to file the document under, as a JSON array or a
    /// comma-separated list. Ids the tenant does not own are a 400.
    #[schema(value_type = Option<String>, rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Option<String>,
}

/// Upload a document.
///
/// `POST /v1/documents` — accepts the upload, returns `202` with the document
/// and job ids. Needs the `kb:write` scope. The answer is `202` because
/// nothing is indexed yet: a worker picks the job up and the caller polls
/// `GET /v1/jobs/{id}`.
#[utoipa::path(
    post,
    path = "/v1/documents",
    tag = "documents",
    request_body(content = UploadForm, content_type = "multipart/form-data"),
    responses(
        (status = 202, description = "The document was accepted and a job was queued", body = IngestResponse),
        (status = 400, description = "No file, an unsupported extension, or an unknown knowledge base", body = ErrorBody),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = ErrorBody),
        (status = 413, description = "The upload is larger than the accepted limit", body = ErrorBody),
        (status = 415, description = "The file extension is not on the accepted list", body = ErrorBody),
        (status = 500, description = "The store could not be written", body = ErrorBody),
        (status = 502, description = "The file could not be stored", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn ingest_document(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    request: axum::extract::Request,
) -> Result<Response, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    let upload = read_upload(request, &state).await?;
    let max_bytes = crate::routes::max_file_bytes();

    if !is_accepted_extension(&upload.file_name) {
        return Err(ApiError::UnsupportedMedia(format!(
            "\"{}\" has an unsupported type. See GET /v1/formats for the accepted list.",
            upload.file_name
        )));
    }
    let size = upload.bytes.len() as u64;
    if size > max_bytes {
        return Err(ApiError::PayloadTooLarge(
            crate::routes::payload_too_large_message(Some(&upload.file_name), Some(size)),
        ));
    }

    let title = upload
        .field_or("title", &title_from_file_name(&upload.file_name))
        .to_string();
    let categories = crate::routes::parse_list(upload.field("categories").unwrap_or(""));
    let subcategory = upload
        .field("subcategory")
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let external_ref = upload
        .field("externalRef")
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    // Additive: an optional JSON object for the document's JSONB metadata, the
    // same store `RET-3` filters and `RET-4` boosts read. Absent keeps exactly
    // the metadata an upload had before this field existed.
    let mut metadata = parse_upload_metadata(upload.field("metadata"))?;
    // Additive (`TBL-1`): `tablePrivacy: "aggregate-only"` asks the table
    // routes to answer only whole-group aggregates for this document's tables.
    // It lives in the document's JSONB metadata, so no column and no migration
    // are added to the shared `Document` table.
    if let Some(privacy) = upload
        .field("tablePrivacy")
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if privacy != "aggregate-only" && privacy != "open" {
            return Err(ApiError::BadRequest(
                "'tablePrivacy' must be 'aggregate-only' or 'open'".to_string(),
            ));
        }
        let object = metadata.get_or_insert_with(|| json!({}));
        if let Some(object) = object.as_object_mut() {
            object.insert("tablePrivacy".to_string(), json!(privacy));
        }
    }
    let document_type = upload
        .field("documentType")
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let figure_mode = parse_figure_mode(upload.field("figures"));
    let knowledge_base_ids = requested_knowledge_bases(&auth, upload.field("knowledgeBaseIds"));
    // Every requested knowledge base must belong to the caller's tenant. Without
    // this, a foreign id hit the DocumentGroup foreign key: another tenant's KB
    // accepted the link (its documentCount moved), and a non-existent one
    // surfaced as a 500, which told the caller which ids exist. Checked before
    // the blob is stored so a rejected upload leaves nothing behind. Unknown and
    // foreign ids get the same answer on purpose.
    if !knowledge_base_ids.is_empty() {
        let owned = state
            .documents
            .filter_owned_kb_ids(&auth.tenant_id, &knowledge_base_ids)
            .await
            .map_err(ApiError::from)?;
        let unknown: Vec<&str> = knowledge_base_ids
            .iter()
            .filter(|k| !owned.contains(k))
            .map(|k| k.0.as_str())
            .collect();
        if !unknown.is_empty() {
            return Err(ApiError::BadRequest(format!(
                "Unknown knowledgeBaseIds: {}",
                unknown.join(", ")
            )));
        }
    }

    let document_id = DocumentId::new_unchecked(Uuid::new_v4().to_string());
    let s3_key = document_path(Some(&auth.tenant_id.0), &document_id.0, &upload.file_name);
    let content_type = upload
        .content_type
        .as_deref()
        .filter(|v| !v.is_empty())
        .unwrap_or("application/octet-stream");

    if let Err(err) = state
        .blob
        .upload(&s3_key, upload.bytes.clone(), content_type)
        .await
    {
        tracing::error!(error = %err, s3_key, "upload failed");
        return Err(ApiError::BadGateway(
            "Failed to store the uploaded file. Please try again.".to_string(),
        ));
    }

    state
        .documents
        .create_document(
            &NewDocument {
                id: document_id.clone(),
                tenant_id: auth.tenant_id.clone(),
                title: title.clone(),
                content: String::new(),
                categories: categories.clone(),
                subcategory: subcategory.clone(),
                s3_key: Some(s3_key.clone()),
                file_size: Some(size as i32),
                mime_type: upload.content_type.clone().filter(|v| !v.is_empty()),
                status: "processing".to_string(),
                external_ref: external_ref.clone(),
                metadata: metadata.clone(),
            },
            &knowledge_base_ids,
        )
        .await
        .map_err(ApiError::from)?;

    // The job carries the parsed parameters, not the raw form: the worker has
    // no access to the request that created the document.
    let mut params = json!({
        "title": title,
        "categories": categories,
        "subcategory": subcategory,
        "figureMode": figure_mode.as_str(),
        "useCombined": true,
    });
    if let Some(document_type) = document_type {
        params["documentType"] = json!(document_type);
    }
    let job_id = state
        .jobs
        .create_job(&CreateJobInput {
            tenant_id: auth.tenant_id.clone(),
            user_id: external_ref,
            filename: upload.file_name.clone(),
            file_size: Some(size as i32),
            mime_type: upload.content_type.clone().filter(|v| !v.is_empty()),
            s3_key: Some(s3_key),
            document_id: Some(document_id.clone()),
            params,
        })
        .await
        .map_err(ApiError::from)?;

    let body = IngestResponse {
        id: document_id.0.clone(),
        job_id,
        status: "processing",
        title,
        knowledge_base_ids: knowledge_base_ids.iter().map(|k| k.0.clone()).collect(),
    };
    Ok((StatusCode::ACCEPTED, Json(body)).into_response())
}

/// The body `POST /v1/documents` answers with.
#[derive(Debug, Serialize, ToSchema)]
pub struct IngestResponse {
    /// The new document's id.
    pub id: String,
    /// The job that will ingest it.
    #[serde(rename = "jobId")]
    pub job_id: String,
    /// Always `processing` — the document is not indexed yet.
    pub status: &'static str,
    /// The document's title.
    pub title: String,
    /// The knowledge bases the document was filed under.
    #[serde(rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Vec<String>,
}

/// Figure mode from the `figures` form field, defaulting to `auto` exactly as
/// `parseFigureMode` does for an absent or unknown value.
fn parse_figure_mode(value: Option<&str>) -> FigureMode {
    kb_core::policy::parse_figure_mode(value, None)
}

/// Restrict the requested knowledge bases to the key's own. Only the key's
/// restriction is applied here, matching the TypeScript route: the links it
/// creates are checked against Postgres' foreign key, so an id this tenant does
/// not own fails the insert rather than silently creating a link.
fn requested_knowledge_bases(
    auth: &kb_ports::AuthContext,
    requested: Option<&str>,
) -> Vec<KnowledgeBaseId> {
    let requested = crate::routes::parse_list(requested.unwrap_or(""));
    let restricted = crate::routes::restrict_kbs(auth, Some(&requested)).unwrap_or_default();
    restricted
        .into_iter()
        .map(KnowledgeBaseId::new_unchecked)
        .collect()
}

async fn read_upload(
    request: axum::extract::Request,
    state: &AppState,
) -> Result<Upload, ApiError> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|_| ApiError::BadRequest("No file provided (field: file)".to_string()))?;
    let mut upload = Upload {
        file_name: String::new(),
        content_type: None,
        bytes: Vec::new(),
        fields: Vec::new(),
    };
    let mut saw_file = false;
    loop {
        let field = multipart.next_field().await.map_err(|err| {
            if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::PayloadTooLarge(crate::routes::payload_too_large_message(None, None))
            } else {
                ApiError::BadRequest(format!("Malformed multipart body: {err}"))
            }
        })?;
        let Some(field) = field else { break };
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" {
            saw_file = true;
            upload.file_name = field.file_name().unwrap_or_default().to_string();
            upload.content_type = field.content_type().map(str::to_string);
            upload.bytes = field
                .bytes()
                .await
                .map_err(|err| {
                    if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
                        ApiError::PayloadTooLarge(crate::routes::payload_too_large_message(
                            None, None,
                        ))
                    } else {
                        ApiError::BadRequest(format!("Could not read the uploaded file: {err}"))
                    }
                })?
                .to_vec();
        } else {
            let value = field.text().await.map_err(|err| {
                ApiError::BadRequest(format!("Could not read form field {name}: {err}"))
            })?;
            upload.fields.push((name, value));
        }
    }
    if !saw_file || upload.file_name.is_empty() {
        return Err(ApiError::BadRequest(
            "No file provided (field: file)".to_string(),
        ));
    }
    Ok(upload)
}

#[cfg(test)]
mod tests {
    use super::{is_accepted_extension, title_from_file_name};

    #[test]
    fn extensions_are_matched_case_insensitively() {
        assert!(is_accepted_extension("report.PDF"));
        // Only the final extension counts, and it must be on the list.
        assert!(!is_accepted_extension("archive.tar.gz"));
        assert!(!is_accepted_extension("tool.exe"));
        // No dot: the whole name is tested, as `slice(lastIndexOf("."))` does.
        assert!(!is_accepted_extension("README"));
    }

    #[test]
    fn title_strips_only_the_last_extension() {
        assert_eq!(title_from_file_name("report.pdf"), "report");
        assert_eq!(title_from_file_name("archive.tar.gz"), "archive.tar");
        assert_eq!(title_from_file_name("noextension"), "noextension");
        assert_eq!(title_from_file_name(".hidden"), ".hidden");
    }
}
