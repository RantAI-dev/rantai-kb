//! HTTP routes.
//!
//! The route table, the CORS behaviour, and the read path live here; the write
//! paths are split out per area (`upload`, `media`, `jobs`, `taxonomy`) so no
//! module has to hold the whole contract.
//!
//! Two behaviours are shared by every route and are therefore expressed once:
//! * `OPTIONS` is answered with the exact CORS header set before authentication,
//!   and every response carries `access-control-allow-origin: *`.
//! * Anything unmatched authenticates first and then 404s, so an unknown path
//!   cannot be used to probe which tenants exist.

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Query, Request, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_MAX_AGE, HeaderValue,
};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use kb_core::outline::{OutlineChunk, build_outline};
use kb_core::policy::MimeRegistry;
use kb_core::versions::normalise_timestamp;
use kb_engine::timing::{Stopwatch, SystemClock};
use kb_ports::{
    AuthContext, DocumentFilter, DocumentId, DocumentMeta, DocumentScope, KnowledgeBaseId,
    MetadataFilter, ScopeRequest, UnitRange,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::auth::Authed;
use crate::dto::{
    BoostStatsDto, SearchRequestDto, SearchResponseDto, SearchStatsDto, StageTimingsDto,
    splice_json_member,
};
use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

// The route modules are public because their handlers carry the
// `#[utoipa::path]` attributes the OpenAPI document is built from.
pub mod content;
pub mod jobs;
pub mod media;
pub mod structure;
pub mod tables;
pub mod taxonomy;
pub mod upload;

/// Largest accepted upload, in bytes: `KB_MAX_FILE_BYTES`, read once per
/// process. The default is 256 MiB (see [`MimeRegistry::kb_max_file_bytes`]).
/// Large manuals and textbooks need more: ViDoRe's 943-page computer science
/// book is 50.9 MB, and the 50 MB the Bun service accepted (`src/lib/files/
/// mime-types.ts`) was its own inherited default rather than a considered
/// limit.
pub fn max_file_bytes() -> u64 {
    static LIMIT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| parse_max_file_bytes(std::env::var("KB_MAX_FILE_BYTES").ok().as_deref()))
}

/// `KB_MAX_FILE_BYTES` parsing: a positive integer, else the default.
fn parse_max_file_bytes(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(MimeRegistry::kb_max_file_bytes)
}

/// The body a `413` answers with.
///
/// A caller that is told only "too large" can do nothing but guess; one that
/// is told the limit in bytes, the limit in MiB and the name of the variable
/// that moves it can act. So the message names all three, and the file that
/// was refused when the route knows it.
pub fn payload_too_large_message(file_name: Option<&str>, size: Option<u64>) -> String {
    let max = max_file_bytes();
    let max_mib = max.div_ceil(1024 * 1024);
    let subject = match file_name {
        Some(name) => format!("\"{name}\""),
        None => "The upload".to_string(),
    };
    let size_text = match size {
        Some(bytes) => format!(" is {:.1} MiB", bytes as f64 / (1024.0 * 1024.0)),
        None => String::new(),
    };
    format!(
        "{subject}{size_text} — the limit is {max_mib} MiB ({max} bytes). \
         Raise KB_MAX_FILE_BYTES to accept a larger file."
    )
}

/// Upper bound on a multipart upload body. The file itself is capped at
/// [`max_file_bytes`] by the upload route; the slack covers the part headers,
/// the form fields and the multipart framing around a file exactly at the limit.
pub fn upload_body_limit() -> usize {
    usize::try_from(max_file_bytes() + 1024 * 1024).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod max_file_bytes_tests {
    use super::{parse_max_file_bytes, payload_too_large_message};

    #[test]
    fn unset_or_invalid_keeps_the_256_mib_default() {
        let default = 256 * 1024 * 1024;
        assert_eq!(parse_max_file_bytes(None), default);
        assert_eq!(parse_max_file_bytes(Some("lots")), default);
        assert_eq!(parse_max_file_bytes(Some("0")), default);
        assert_eq!(parse_max_file_bytes(Some(" 104857600 ")), 104_857_600);
    }

    #[test]
    fn the_413_body_names_the_limit_in_bytes_and_mib_and_the_variable() {
        // The limit is the process-wide default here (no `KB_MAX_FILE_BYTES`
        // in the test environment), so the message is checked against it.
        let message = payload_too_large_message(Some("book.pdf"), Some(300 * 1024 * 1024));
        assert!(message.contains("KB_MAX_FILE_BYTES"), "{message}");
        assert!(message.contains("256 MiB"), "{message}");
        assert!(
            message.contains(&(256 * 1024 * 1024).to_string()),
            "{message}"
        );
        assert!(message.contains("300.0 MiB"), "{message}");
        assert!(message.contains("book.pdf"), "{message}");
    }

    #[test]
    fn the_413_body_works_without_a_file_name() {
        let message = payload_too_large_message(None, None);
        assert!(message.contains("KB_MAX_FILE_BYTES"), "{message}");
        assert!(message.contains("256 MiB"), "{message}");
        assert!(
            message.contains(&(256 * 1024 * 1024).to_string()),
            "{message}"
        );
    }
}

// ─── The route table ────────────────────────────────────────────────────────

/// One route of the API, as the OpenAPI document names it.
///
/// The table is written once, in [`route_table!`], and it builds the router
/// *and* this list: a route cannot exist without a spec entry, and a spec
/// entry cannot exist without a route. `tests/openapi_tests.rs` checks the
/// list against the published document, so a route added to one and not the
/// other fails the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSpec {
    /// The HTTP method.
    pub method: Method,
    /// The path, in axum's spelling (`/v1/documents/:id`).
    pub path: &'static str,
    /// The OpenAPI tag the operation is filed under.
    pub tag: &'static str,
}

/// The route table.
///
/// `public` holds the handful of routes that answer without a key; `api`
/// holds everything under `/v1` that needs one. Each entry gives the path,
/// the OpenAPI tag, and the handler per method.
///
/// The macro builds three things from that one declaration: the
/// authenticated router, the public router, and the route list the OpenAPI
/// document is checked against.
macro_rules! route_table {
    (
        public {
            $(
                $public_path:literal => $public_tag:literal {
                    $( $public_method:ident => $public_handler:path ),* $(,)?
                }
            ),* $(,)?
        }
        api {
            $(
                $api_path:literal => $api_tag:literal {
                    $( $api_method:ident => $api_handler:path ),* $(,)?
                }
            ),* $(,)?
        }
    ) => {
        /// Every public route, in the order the table declares them.
        pub const PUBLIC_ROUTES: &[RouteSpec] = &[
            $(
                $(
                    RouteSpec {
                        method: Method::$public_method,
                        path: $public_path,
                        tag: $public_tag,
                    },
                )*
            )*
        ];

        /// Every authenticated route, in the order the table declares them.
        pub const API_ROUTES: &[RouteSpec] = &[
            $(
                $(
                    RouteSpec {
                        method: Method::$api_method,
                        path: $api_path,
                        tag: $api_tag,
                    },
                )*
            )*
        ];

        /// Every route the API serves.
        pub const ALL_ROUTES: &[RouteSpec] = &[
            $(
                $(
                    RouteSpec {
                        method: Method::$public_method,
                        path: $public_path,
                        tag: $public_tag,
                    },
                )*
            )*
            $(
                $(
                    RouteSpec {
                        method: Method::$api_method,
                        path: $api_path,
                        tag: $api_tag,
                    },
                )*
            )*
        ];

        /// The authenticated router, built from the table above.
        pub fn router() -> Router<Arc<AppState>> {
            Router::new()
            $(
                .route($api_path, {
                    let method_router = axum::routing::MethodRouter::<Arc<AppState>>::new();
                    $( let method_router = method_router.on(axum::routing::MethodFilter::$api_method, $api_handler); )*
                    method_router
                })
            )*
            .layer(DefaultBodyLimit::max(upload_body_limit()))
        }

        /// The public router (health + formats), built from the table above.
        pub fn health_router() -> Router<Arc<AppState>> {
            Router::new()
            $(
                .route($public_path, {
                    let method_router = axum::routing::MethodRouter::<Arc<AppState>>::new();
                    $( let method_router = method_router.on(axum::routing::MethodFilter::$public_method, $public_handler); )*
                    method_router
                })
            )*
        }
    };
}

route_table! {
    public {
        "/health" => "health" {
            GET => health,
        },
        "/v1/health" => "health" {
            GET => health_v1,
        },
        "/v1/formats" => "health" {
            GET => formats,
        },
    }
    api {
        "/v1/search" => "search" {
            POST => search,
        },
        "/v1/documents" => "documents" {
            GET => list_documents,
            POST => upload::ingest_document,
        },
        "/v1/documents/:id" => "documents" {
            GET => get_document,
            PATCH => patch_document,
            DELETE => delete_document,
        },
        "/v1/documents/:id/raw" => "media" {
            GET => media::get_document_raw,
        },
        "/v1/documents/:id/structure" => "structure" {
            GET => structure::get_document_structure,
        },
        "/v1/documents/:id/content" => "structure" {
            GET => content::get_document_content,
        },
        "/v1/documents/:id/assets" => "media" {
            GET => media::get_document_asset,
        },
        "/v1/documents/:id/intelligence" => "media" {
            GET => media::get_document_intelligence,
        },
        "/v1/jobs/:id" => "jobs" {
            GET => jobs::get_job,
        },
        "/v1/jobs/:id/retry" => "jobs" {
            POST => jobs::retry_job,
        },
        "/v1/knowledge-bases" => "taxonomy" {
            GET => list_knowledge_bases,
            POST => taxonomy::create_knowledge_base,
        },
        "/v1/knowledge-bases/:id" => "taxonomy" {
            PATCH => taxonomy::patch_knowledge_base,
            DELETE => taxonomy::delete_knowledge_base,
        },
        "/v1/categories" => "taxonomy" {
            GET => taxonomy::list_categories,
            POST => taxonomy::create_category,
        },
        "/v1/categories/:id" => "taxonomy" {
            PATCH => taxonomy::patch_category,
            DELETE => taxonomy::delete_category,
        },
        "/v1/events" => "events" {
            GET => crate::events::events_stream,
        },
        "/v1/tables" => "tables" {
            GET => tables::list_tables,
        },
        "/v1/tables/:id" => "tables" {
            GET => tables::get_table,
        },
        "/v1/tables/:id/query" => "tables" {
            POST => tables::query_table,
        },
    }
}

/// CORS headers, verbatim from `CORS_HEADERS` in `src/service/api.ts`.
fn cors_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,POST,PATCH,DELETE,OPTIONS"),
    );
    headers.insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization,content-type,x-api-key"),
    );
    headers.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
    headers
}

/// Answer preflights before authentication and stamp the allow-origin header on
/// every response, which is what `handleRequest` does through its `json()` and
/// stream helpers.
pub async fn cors(req: Request, next: Next) -> Response {
    if req.method() == Method::OPTIONS {
        return (StatusCode::NO_CONTENT, cors_headers()).into_response();
    }
    let mut res = next.run(req).await;
    if !res.headers().contains_key(ACCESS_CONTROL_ALLOW_ORIGIN) {
        res.headers_mut()
            .insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    }
    res
}

/// Fallback for any path the router does not know: authenticate, then 404.
pub async fn not_found(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (mut parts, _body) = req.into_parts();
    if Authed::from_request_parts(&mut parts, &state)
        .await
        .is_err()
    {
        return ApiError::Unauthorized.into_response();
    }
    ApiError::NotFound.into_response()
}

// ─── Health ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// True when every dependency answered. False means at least one is down;
    /// the `checks` say which.
    pub ok: bool,
    /// One boolean per dependency.
    pub checks: HealthChecks,
    /// The server's version.
    pub version: String,
    /// Additive (R3): the `KB_TIER` preset name, or null when none is set.
    pub tier: Option<String>,
    /// Additive (R3): non-local model-endpoint hosts, sorted and de-duplicated.
    pub egress: Vec<String>,
    /// Additive (`#36`): the state of the embedder, the reranker and the OCR
    /// backend.
    ///
    /// `ready`, `starting` or `unavailable` each. A client that uploads
    /// scanned documents can hold the upload until the OCR backend says
    /// `ready` instead of discovering the outage in the job queue.
    pub backends: crate::backends::BackendHealth,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthChecks {
    /// Whether Postgres answered.
    pub postgres: bool,
    /// Whether the vector store answered.
    pub vectors: bool,
}

/// Liveness and readiness: are the dependencies answering?
///
/// `GET /health` — the liveness probe. Public: a health check must not need a
/// key, or an operator cannot use it to find out why the service is not
/// answering.
#[utoipa::path(
    get,
    path = "/health",
    tag = "health",
    responses(
        (status = 200, description = "Every dependency answered", body = HealthResponse),
        (status = 503, description = "At least one dependency did not answer", body = HealthResponse),
    ),
    security(()),
)]
pub async fn health(State(state): State<Arc<AppState>>) -> Response {
    // Each dependency is probed independently and reported as a boolean: a
    // liveness endpoint that fails the whole request tells an operator nothing
    // about which side is down.
    let postgres = state.documents.health_check().await.is_ok();
    let vectors = state.index.health_check().await.is_ok();
    let ok = postgres && vectors;
    // The model backends are reported separately (`#36`): they do not decide
    // whether the service is alive, but a client has to know whether the OCR
    // backend can serve a scanned document before it uploads one.
    let backends = state.backends.health().await;
    let body = HealthResponse {
        ok,
        checks: HealthChecks { postgres, vectors },
        version: state.version.clone(),
        tier: state.tier.clone(),
        egress: state.egress.clone(),
        backends,
    };
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body)).into_response()
}

/// Liveness and readiness, under the versioned path.
///
/// `GET /v1/health` — the same probe as `GET /health`. A separate handler so
/// that each path is its own OpenAPI operation; the body is the same one
/// `/health` returns.
#[utoipa::path(
    get,
    path = "/v1/health",
    tag = "health",
    responses(
        (status = 200, description = "Every dependency answered", body = HealthResponse),
        (status = 503, description = "At least one dependency did not answer", body = HealthResponse),
    ),
    security(()),
)]
pub async fn health_v1(State(state): State<Arc<AppState>>) -> Response {
    health(State(state)).await
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FormatsResponse {
    /// The file extensions the upload route accepts, each with its leading dot.
    pub extensions: Vec<String>,
    /// The largest accepted upload, in bytes.
    #[serde(rename = "maxFileBytes")]
    pub max_file_bytes: u64,
}

/// The upload formats the service accepts.
///
/// `GET /v1/formats` — the accepted extensions and the size limit. Public,
/// like the health probe: a client has to know the accepted list before it
/// has a key.
#[utoipa::path(
    get,
    path = "/v1/formats",
    tag = "health",
    responses(
        (status = 200, description = "The accepted extensions and the size limit", body = FormatsResponse),
    ),
    security(()),
)]
pub async fn formats() -> Json<FormatsResponse> {
    Json(FormatsResponse {
        extensions: MimeRegistry::kb_accepted_extensions()
            .iter()
            .map(|ext| (*ext).to_string())
            .collect(),
        max_file_bytes: max_file_bytes(),
    })
}

// ─── Search ──────────────────────────────────────────────────────────────────

/// The body every delete answers with.
///
/// One shape for every route that removes something: `{"ok": true}`. A delete
/// that removed nothing answers `404` instead, so `ok` is always `true` when
/// it is there at all.
#[derive(Debug, Serialize, ToSchema)]
pub struct OkResponse {
    /// Always `true`.
    pub ok: bool,
}

/// Parse one effective-date field on upload or PATCH. An absent or empty field
/// is `None`; anything that is not a date is the route's own 400, naming the
/// field so the caller can fix it.
pub(crate) fn parse_version_date(
    field: &str,
    value: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    normalise_timestamp(raw).map(Some).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "Invalid '{field}': expected YYYY-MM-DD or an ISO timestamp, got '{raw}'"
        ))
    })
}

/// The two additive version fields of a search body, validated at the boundary.
/// Both may be sent: `asOf` takes precedence in the store's scope resolution,
/// because a point-in-time question is more specific than "show me history".
fn parse_search_versions(body: &SearchRequestDto) -> Result<(bool, Option<String>), ApiError> {
    let include_history = match body.include_history.as_ref() {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(value)) => *value,
        Some(_) => {
            return Err(ApiError::BadRequest(
                "Invalid 'includeHistory': expected a boolean".to_string(),
            ));
        }
    };
    let as_of = match body.as_of.as_ref() {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => {
            Some(parse_version_date("asOf", Some(value))?.ok_or_else(|| {
                ApiError::BadRequest(
                    "Invalid 'asOf': expected YYYY-MM-DD or an ISO timestamp".to_string(),
                )
            })?)
        }
        Some(_) => {
            return Err(ApiError::BadRequest(
                "Invalid 'asOf': expected YYYY-MM-DD or an ISO timestamp".to_string(),
            ));
        }
    };
    Ok((include_history, as_of))
}

/// Ask a question; get the chunks, the sources and the assembled context.
///
/// `POST /v1/search` — the retrieval answer. Needs the `kb:read` scope. The
/// answer carries the chunks, the numbered sources, the assembled context and
/// the stage timings; `degrades` names the arms that did not run, so an empty
/// answer is never a silent one.
#[utoipa::path(
    post,
    path = "/v1/search",
    tag = "search",
    request_body = SearchRequestDto,
    responses(
        (status = 200, description = "The search answer", body = SearchResponseDto),
        (status = 400, description = "The body has no usable 'query'", body = ErrorBody),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:read scope", body = ErrorBody),
        (status = 500, description = "The retrieval pipeline failed", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn search(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    body: Bytes,
) -> Result<Response, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    // From here the request is ours to account for: scope build, the pipeline
    // and serialisation all land in `stageTimingsMs`.
    let route_sw = Stopwatch::start(&SystemClock);
    let body: SearchRequestDto = crate::dto::parse_json(&body);
    // `!body.query || typeof body.query !== "string"` in TypeScript: an absent,
    // non-string or blank query is the same 400.
    let Some(query) = body.query.as_ref().and_then(serde_json::Value::as_str) else {
        return Err(ApiError::BadRequest(
            "Body must include a 'query' string".to_string(),
        ));
    };
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err(ApiError::BadRequest(
            "Body must include a 'query' string".to_string(),
        ));
    }
    let requested_kbs = restrict_kbs(&auth, body.knowledge_base_ids.as_deref());
    let max_chunks = body.max_chunks.unwrap_or(8).clamp(1, 50);
    let metadata_filter = parse_metadata_filter(body.metadata.as_ref())?;
    let boost = parse_boost(body.boost.as_ref())?;
    let (include_history, as_of) = parse_search_versions(&body)?;

    // RET-15: both forms name one part of one document. Validate before any
    // arm runs, so a bad request never spends top-k and never gets an empty
    // answer that looks like a legitimate "no matches".
    if body.page_range.is_some() && body.section.is_some() {
        return Err(ApiError::BadRequest(
            "pageRange and section cannot be used together".to_string(),
        ));
    }
    if let Some(range) = body.page_range.as_ref()
        && range.start > range.end
    {
        return Err(ApiError::BadRequest(
            "pageRange.start must be <= pageRange.end".to_string(),
        ));
    }
    if (body.page_range.is_some() || body.section.is_some())
        && body.document_ids.as_ref().is_none_or(|ids| ids.len() != 1)
    {
        return Err(ApiError::BadRequest(
            "pageRange/section need exactly one documentIds entry".to_string(),
        ));
    }

    // A key bound to specific knowledge bases that asks for none of them gets
    // an explicit note rather than an empty result: "no results" and "you may
    // not see these" are different answers for a client.
    if let Some(ref r) = requested_kbs {
        if r.is_empty() {
            return Ok(Json(serde_json::json!({
                "chunks": [],
                "context": "",
                "note": "API key is not bound to any of the requested knowledge bases",
            }))
            .into_response());
        }
    }

    // The one filter every branch resolves into a scope. `externalRefs`,
    // `language` and `metadata` narrow the tenant's visible documents in
    // Postgres (`build_scope` -> `resolve_filter`), so every retrieval arm sees
    // the narrowed scope and none of them can spend top-k on an excluded
    // document and then drop it after ranking.
    let filter = DocumentFilter {
        knowledge_base_ids: requested_kbs
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(KnowledgeBaseId::new_unchecked)
            .collect(),
        category: body.category.clone(),
        external_refs: body.external_refs.clone(),
        language: body.language.clone(),
        metadata: metadata_filter,
        include_history,
        as_of,
    };

    // Branch 1: documentIds filtering (TypeScript api.ts:139)
    if let Some(doc_ids) = body.document_ids.as_ref() {
        let requested: Vec<DocumentId> = doc_ids
            .iter()
            .map(|id| DocumentId::new_unchecked(id.clone()))
            .collect();
        let scope_sw = Stopwatch::start(&SystemClock);
        let alive = state
            .documents
            .find_alive_meta_by_ids(&auth.tenant_id, &requested)
            .await
            .map_err(ApiError::from)?;
        if alive.is_empty() {
            return Ok(Json(serde_json::json!({
                "chunks": [],
                "context": "",
            }))
            .into_response());
        }
        // Narrow the *filtered* scope, not the whole tenant: the key's
        // knowledge-base limit and every metadata filter still apply, so
        // `documentIds` can only intersect them, never widen them.
        let filtered_scope = state
            .documents
            .build_scope(&ScopeRequest {
                tenant: auth.tenant_id.clone(),
                filter: filter.clone(),
            })
            .await
            .map_err(ApiError::from)?;
        let mut scope = filtered_scope.narrowed_to(&requested);
        // RET-15: the range lives on the scope, so every arm inherits it from
        // the same object that carries the tenant and document visibility.
        if scope.document_ids().len() == 1 {
            if let Some(range) = body.page_range.as_ref() {
                scope = scope.with_unit_range(UnitRange {
                    pages: Some((range.start, range.end)),
                    chunks: None,
                });
            } else if let Some(section) = body.section.as_ref() {
                let range = resolve_section_range(&state, &scope, &alive, section).await?;
                scope = scope.with_unit_range(range);
            }
        }
        let scope_build_ms = scope_sw.elapsed_ms();
        // RET-11: the narrowed scope is the security-relevant part of this
        // branch. Once it is built, the search is the same one Branch 3 runs —
        // BM25, reranker, page/tree/neighbour and figure arms included.
        return retrieve_and_finish(SearchTail {
            state: &state,
            auth: &auth,
            query: &query,
            scope: &scope,
            scope_build_ms,
            max_chunks,
            body: &body,
            boost,
            route_sw,
        })
        .await;
    }

    // Branch 2: hybrid retrieval (TypeScript api.ts:151)
    if body.hybrid == Some(true) {
        // Nothing runs on this branch — it is the Bun stub — but the shape of
        // `stats` is the one every branch answers with, so a client parsing it
        // does not have to know which branch it hit.
        return Ok(finish_search(
            SearchResponseDto {
                chunks: None,
                sources: Some(Vec::new()),
                context: Some(String::new()),
                results: Some(Vec::new()),
                // Same shape as every other branch, every number zero: nothing
                // ran here. `total` and `serialise` are stamped by
                // `finish_search`, which is the only place that can measure them.
                stats: Some(search_stats(
                    StageTimingsDto::default(),
                    0,
                    0,
                    None,
                    None,
                    None,
                )),
                note: None,
                // Nothing was searched, and an empty list here looks exactly
                // like "no matches". A client that sent `hybrid: true` expecting
                // hybrid retrieval (one did: the MOCO-KF adapter) must be told,
                // so the stub degrades visibly instead of answering silently.
                degrades: vec![HYBRID_STUB_DEGRADE.to_string()],
                tables: Vec::new(),
                multi_version_warning: Vec::new(),
            },
            route_sw,
        ));
    }

    // Branch 3: standard retrieval
    let scope_sw = Stopwatch::start(&SystemClock);
    let scope = state
        .documents
        .build_scope(&ScopeRequest {
            tenant: auth.tenant_id.clone(),
            filter,
        })
        .await
        .map_err(ApiError::from)?;
    let scope_build_ms = scope_sw.elapsed_ms();
    retrieve_and_finish(SearchTail {
        state: &state,
        auth: &auth,
        query: &query,
        scope: &scope,
        scope_build_ms,
        max_chunks,
        body: &body,
        boost,
        route_sw,
    })
    .await
}

/// Resolve a `section` id to the chunk (and page) range of the outline v2 that
/// `/v1/documents/:id/structure` serves. The same scoped, vector-free chunk
/// read is used, so a document outside the caller's scope contributes nothing.
async fn resolve_section_range(
    state: &AppState,
    scope: &DocumentScope,
    alive: &[DocumentMeta],
    requested: &str,
) -> Result<UnitRange, ApiError> {
    let Some(doc) = alive.first() else {
        return Err(ApiError::BadRequest(format!(
            "unknown section id: {requested}"
        )));
    };
    let chunk_meta = state
        .index
        .list_chunk_meta(scope, &doc.id)
        .await
        .map_err(ApiError::from)?;
    let chunks: Vec<OutlineChunk> = chunk_meta
        .into_iter()
        .map(|meta| OutlineChunk {
            chunk_index: meta.chunk_index,
            page: meta.page,
            section: meta.section,
            content: meta.content,
            char_start: meta.char_start,
            char_end: meta.char_end,
            overlap_prefix_len: meta.overlap_prefix_len,
        })
        .collect();
    let stored = doc
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("outline"))
        .and_then(|value| serde_json::from_value::<kb_core::Outline>(value.clone()).ok());
    let section = build_outline(&chunks, stored.as_ref())
        .into_iter()
        .find(|section| section.id == requested)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown section id: {requested}")))?;
    Ok(UnitRange {
        pages: section.page_start.zip(section.page_end),
        chunks: Some((section.chunk_start, section.chunk_end)),
    })
}

/// The inputs the two search branches share once their scope is resolved.
struct SearchTail<'a> {
    state: &'a Arc<AppState>,
    auth: &'a AuthContext,
    query: &'a str,
    scope: &'a kb_ports::DocumentScope,
    scope_build_ms: f64,
    max_chunks: u32,
    body: &'a SearchRequestDto,
    boost: Option<kb_core::BoostConfig>,
    route_sw: Stopwatch<'a>,
}

/// Run the standard retrieval pipeline over an already resolved scope and
/// finish the response.
///
/// The `documentIds` branch and the standard branch differ only in how they
/// build the scope. Sharing the tail here keeps them on the same arms, the same
/// floor and boost handling, and the same response shape — so a caller that
/// narrows a search to chosen documents gets the same answer quality, and a
/// leak fixed in one path is fixed in the other.
async fn retrieve_and_finish(tail: SearchTail<'_>) -> Result<Response, ApiError> {
    let SearchTail {
        state,
        auth,
        query,
        scope,
        scope_build_ms,
        max_chunks,
        body,
        boost,
        route_sw,
    } = tail;
    let outcome = state
        .engine
        .retrieve_inner(
            query,
            scope,
            Some(max_chunks),
            body.min_score,
            boost,
            body.adaptive_arms,
        )
        .await
        .map_err(ApiError::from)?;
    let mut response = SearchResponseDto::from(outcome);
    if let Some(stats) = response.stats.as_mut() {
        // The route knows two stages the engine does not: building the scope,
        // and everything above it in the handler.
        stats.stage_timings_ms.scope_build = scope_build_ms;
    }
    tables::attach_tables(state, &auth.tenant_id, &mut response).await;
    Ok(finish_search(response, route_sw))
}

/// What the `hybrid: true` stub reports. The standard search already fuses the
/// vector and keyword arms, so the fix for the caller is to omit the flag.
pub(crate) const HYBRID_STUB_DEGRADE: &str = "hybrid: `hybrid: true` selects the legacy \
Bun stub, which does not search; omit the flag (the default search already fuses the vector \
and keyword arms)";

/// The Bun-shaped `stats` for a set of stage timings.
///
/// The five legacy time fields are copies of the stages they have always
/// named — `embeddingTimeMs` is the query embed, `vectorSearchTimeMs` the
/// vector arm, `fusionTimeMs` the fusion — so a client that has been reading
/// zeros on the standard path starts reading the truth instead, and one that
/// reads the new breakdown gets the rest.
fn search_stats(
    stages: StageTimingsDto,
    vector_results: usize,
    total_results: usize,
    floor_score_kind: Option<String>,
    floor_dropped: Option<usize>,
    boost: Option<BoostStatsDto>,
) -> SearchStatsDto {
    SearchStatsDto {
        total_time_ms: stages.total,
        embedding_time_ms: stages.detail.query_embed,
        vector_search_time_ms: stages.detail.vector_arm,
        entity_search_time_ms: stages.detail.entity_arm,
        graph_search_time_ms: 0.0,
        fusion_time_ms: stages.fusion,
        vector_results,
        entity_results: 0,
        graph_results: 0,
        total_results,
        stage_timings_ms: stages,
        floor_score_kind,
        floor_dropped,
        boost,
        query_class: None,
        skipped_arms: None,
        arm_plan_fallback: None,
        tree_arm: None,
        global_arm: None,
        // Only the standard branch runs a reranker, and it builds its stats
        // from the engine's outcome, which carries the count.
        reranked_count: None,
    }
}

/// Serialise a search response and measure it, once.
///
/// This is the only place the total and the serialisation cost are stamped:
/// both are measured here, over a body that has already been assembled, and
/// both are then spliced into it — so the numbers reported are the numbers paid
/// for the body that carries them. Logging the stages at debug level happens
/// here too, under the same names the response uses.
fn finish_search(response: SearchResponseDto, route_sw: Stopwatch<'_>) -> Response {
    let body_sw = Stopwatch::start(&SystemClock);
    let dto = SearchResponseDto {
        stats: None,
        ..response
    };
    let bytes = match serde_json::to_vec(&dto) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(error = %err, "could not serialise a search response");
            return ApiError::Internal("could not serialise a search response".to_string())
                .into_response();
        }
    };
    let serialise_ms = body_sw.elapsed_ms();
    let stats = response.stats.unwrap_or_else(|| {
        search_stats(
            StageTimingsDto {
                serialise: serialise_ms,
                total: route_sw.elapsed_ms(),
                ..StageTimingsDto::default()
            },
            0,
            0,
            None,
            None,
            None,
        )
    });
    let mut stages = stats.stage_timings_ms;
    stages.serialise = serialise_ms;
    stages.total = route_sw.elapsed_ms();
    // The legacy time fields are recomputed from the final stages; every
    // additive member the engine reported is carried over as it is. Rebuilding
    // without them dropped `queryClass`, `skippedArms`, `armPlanFallback`,
    // `treeArm` and `globalArm` from every live response.
    let SearchStatsDto {
        vector_results,
        total_results,
        floor_score_kind,
        floor_dropped,
        boost,
        query_class,
        skipped_arms,
        arm_plan_fallback,
        tree_arm,
        global_arm,
        reranked_count,
        ..
    } = stats;
    let stats = SearchStatsDto {
        query_class,
        skipped_arms,
        arm_plan_fallback,
        tree_arm,
        global_arm,
        reranked_count,
        ..search_stats(
            stages,
            vector_results,
            total_results,
            floor_score_kind,
            floor_dropped,
            boost,
        )
    };
    let stats_json = match serde_json::to_vec(&stats) {
        Ok(json) => json,
        Err(err) => {
            tracing::error!(error = %err, "could not serialise search stats");
            return ApiError::Internal("could not serialise search stats".to_string())
                .into_response();
        }
    };
    let body = splice_json_member(bytes, "stats", &stats_json);

    for (stage, ms) in crate::dto::accounted_pairs(&stats.stage_timings_ms) {
        tracing::debug!(
            stage,
            ms,
            total_ms = stats.stage_timings_ms.total,
            "search stage"
        );
    }

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

/// Validate a `metadata` search filter and normalise its values.
///
/// Only top-level keys are accepted, each key must match
/// `[A-Za-z0-9_.-]{1,64}`, and every value must be a string, number, boolean or
/// an array of those. Anything else is a 400 like the other request-shape
/// errors on this route; the values are compared as Postgres `->>` renders
/// them, so `2024` and `"2024"` both match the JSON number/string.
fn parse_metadata_filter(
    value: Option<&serde_json::Value>,
) -> Result<Vec<MetadataFilter>, ApiError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(object) = value.as_object() else {
        return Err(ApiError::BadRequest(
            "'metadata' must be an object of key/value pairs".to_string(),
        ));
    };
    let mut out = Vec::with_capacity(object.len());
    for (key, value) in object {
        if !kb_core::valid_metadata_key(key) {
            return Err(ApiError::BadRequest(format!(
                "Invalid metadata key '{key}': keys must match [A-Za-z0-9_.-]{{1,64}}"
            )));
        }
        let values = metadata_scalar_values(value).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "Invalid value for metadata key '{key}': values must be strings, numbers, booleans or arrays of those"
            ))
        })?;
        out.push(MetadataFilter {
            key: key.clone(),
            values,
        });
    }
    Ok(out)
}

/// Validate and normalise the optional `boost` object (`RET-4`).
///
/// The request shape is:
///
/// ```json
/// {"recency":   {"field": "createdAt|updatedAt|metadata.<key>",
///                "halfLifeDays": 365, "weight": 0.5},
///  "authority": {"field": "metadata.<key>", "weight": 0.5}}
/// ```
///
/// Every field is required within its object, unknown members are a 400 (a
/// typo must not silently mean "no boost"), `halfLifeDays` must be finite and
/// `> 0`, `weight` must be in `(0, 1]`, and a metadata key must match
/// `[A-Za-z0-9_.-]{1,64}` — the same rule as the `metadata` filter. An absent,
/// `null` or empty object means no boost, which is the off state.
fn parse_boost(
    value: Option<&serde_json::Value>,
) -> Result<Option<kb_core::BoostConfig>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(ApiError::BadRequest(
            "'boost' must be an object with optional 'recency' and 'authority' members".to_string(),
        ));
    };
    for key in object.keys() {
        if key != "recency" && key != "authority" {
            return Err(ApiError::BadRequest(format!(
                "Unknown boost member '{key}': expected 'recency' or 'authority'"
            )));
        }
    }
    let recency = match object.get("recency") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(parse_recency_boost(value)?),
    };
    let authority = match object.get("authority") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(parse_authority_boost(value)?),
    };
    if recency.is_none() && authority.is_none() {
        return Ok(None);
    }
    Ok(Some(kb_core::BoostConfig { recency, authority }))
}

fn parse_recency_boost(value: &serde_json::Value) -> Result<kb_core::RecencyBoost, ApiError> {
    let object = boost_object(value, "recency", &["field", "halfLifeDays", "weight"])?;
    let field = match object
        .get("field")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
    {
        "createdAt" => kb_core::RecencyField::CreatedAt,
        "updatedAt" => kb_core::RecencyField::UpdatedAt,
        other => match other.strip_prefix("metadata.") {
            Some(key) if kb_core::valid_metadata_key(key) => {
                kb_core::RecencyField::Metadata(key.to_string())
            }
            _ => {
                return Err(ApiError::BadRequest(
                    "Invalid boost.recency.field: expected 'createdAt', 'updatedAt' or \
                     'metadata.<key>' with a key matching [A-Za-z0-9_.-]{1,64}"
                        .to_string(),
                ));
            }
        },
    };
    let half_life_days = positive_number(object.get("halfLifeDays"), "boost.recency.halfLifeDays")?;
    let weight = boost_weight(object.get("weight"), "boost.recency.weight")?;
    Ok(kb_core::RecencyBoost {
        field,
        half_life_days,
        weight,
    })
}

fn parse_authority_boost(value: &serde_json::Value) -> Result<kb_core::AuthorityBoost, ApiError> {
    let object = boost_object(value, "authority", &["field", "weight"])?;
    let field = match object
        .get("field")
        .and_then(serde_json::Value::as_str)
        .and_then(|field| field.strip_prefix("metadata."))
    {
        Some(key) if kb_core::valid_metadata_key(key) => key.to_string(),
        _ => {
            return Err(ApiError::BadRequest(
                "Invalid boost.authority.field: expected 'metadata.<key>' with a key matching \
                 [A-Za-z0-9_.-]{1,64}"
                    .to_string(),
            ));
        }
    };
    let weight = boost_weight(object.get("weight"), "boost.authority.weight")?;
    Ok(kb_core::AuthorityBoost { field, weight })
}

fn boost_object<'a>(
    value: &'a serde_json::Value,
    name: &str,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>, ApiError> {
    let Some(object) = value.as_object() else {
        return Err(ApiError::BadRequest(format!(
            "boost.{name} must be an object with {}",
            allowed
                .iter()
                .map(|key| format!("'{key}'"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "Unknown boost.{name} member '{key}': expected one of {}",
                allowed
                    .iter()
                    .map(|key| format!("'{key}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    Ok(object)
}

fn positive_number(value: Option<&serde_json::Value>, name: &str) -> Result<f64, ApiError> {
    match value.and_then(serde_json::Value::as_f64) {
        Some(number) if number.is_finite() && number > 0.0 => Ok(number),
        _ => Err(ApiError::BadRequest(format!(
            "Invalid {name}: expected a number greater than 0"
        ))),
    }
}

fn boost_weight(value: Option<&serde_json::Value>, name: &str) -> Result<f64, ApiError> {
    match value.and_then(serde_json::Value::as_f64) {
        Some(number) if number.is_finite() && number > 0.0 && number <= 1.0 => Ok(number),
        _ => Err(ApiError::BadRequest(format!(
            "Invalid {name}: expected a number in (0, 1]"
        ))),
    }
}

fn metadata_scalar_values(value: &serde_json::Value) -> Option<Vec<String>> {
    match value {
        serde_json::Value::Array(items) => items.iter().map(metadata_scalar).collect(),
        scalar => metadata_scalar(scalar).map(|value| vec![value]),
    }
}

fn metadata_scalar(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Narrow a caller-supplied knowledge-base filter to what the key may see.
///
/// `None` means the caller named no bases and the key is unrestricted, which
/// the routes read as "no filter". An explicit empty list stays empty: in
/// TypeScript that is a truthy value of length zero, so a search answers with
/// the "not bound to any of the requested knowledge bases" note rather than
/// searching everything.
pub(crate) fn restrict_kbs(
    auth: &AuthContext,
    requested: Option<&[String]>,
) -> Option<Vec<String>> {
    if auth.knowledge_base_ids.is_empty() {
        return requested.map(<[String]>::to_vec);
    }
    let allowed: HashSet<&str> = auth
        .knowledge_base_ids
        .iter()
        .map(|k| k.0.as_str())
        .collect();
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

// ─── Documents ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListDocumentsQuery {
    /// How many documents to return. 1–200; anything else is the default (100).
    // Kept as a string so an unparsable value degrades to the default instead
    // of rejecting the request with a body the API never produces.
    pub limit: Option<String>,
    /// The knowledge bases to list, as a JSON array or a comma-separated list.
    // Bun/TS name (camelCase), used by existing clients.
    #[serde(alias = "knowledge_base_ids", rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Option<String>,
    /// Only documents with this exact external reference.
    // Exact-match filter on `Document.externalRef` (R1, additive). Scoped to
    // the caller's tenant and to live rows, like every other list filter.
    #[serde(rename = "externalRef")]
    pub external_ref: Option<String>,
    /// The cursor of the previous page, as `nextCursor` returned it. Send it
    /// back unchanged to get the next page.
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DocumentListResponse {
    /// The documents, newest first.
    pub documents: Vec<DocumentSummary>,
    /// The cursor for the next page; null on the last page.
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
    /// How many documents match the filter, over every page.
    pub total: u64,
    /// The page size that was applied.
    pub limit: u32,
    /// True when the requested limit was above the cap and was clamped to it.
    #[serde(rename = "limitClamped")]
    pub limit_clamped: bool,
}

/// `GET /v1/documents` row, field for field as `listDocuments` selects it.
#[derive(Debug, Serialize, ToSchema)]
pub struct DocumentSummary {
    /// The document's id.
    pub id: String,
    /// The document's title.
    pub title: String,
    /// The categories the document is filed under.
    pub categories: Vec<String>,
    /// The subcategory, when it has one.
    pub subcategory: Option<String>,
    /// The file's type, as the store recorded it.
    #[serde(rename = "fileType")]
    pub file_type: Option<String>,
    /// The file's size in bytes.
    #[serde(rename = "fileSize")]
    pub file_size: Option<i32>,
    /// `processing`, `waiting_for_ocr`, `needs_ocr`, `ready` or `failed`.
    pub status: String,
    /// Additive: the caller's opaque document reference, or null.
    #[serde(rename = "externalRef")]
    pub external_ref: Option<String>,
    /// When the document was created.
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// The knowledge bases the document belongs to.
    #[serde(rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Vec<String>,
    /// How many pages the document has (`#29`).
    #[serde(rename = "pages")]
    pub pages: Option<u32>,
    /// How much text the document really produced, in characters (`#29`).
    #[serde(rename = "charsExtracted")]
    pub chars_extracted: Option<u64>,
    /// What kept the document from being fully searchable, one line each
    /// (`#29`).
    #[serde(rename = "degrades")]
    pub degrades: Vec<String>,
    /// The SHA-256 of the uploaded bytes, when the document was stored with
    /// one. Null for a document uploaded before the hash was recorded.
    #[serde(rename = "sha256")]
    pub sha256: Option<String>,
    /// Version metadata (`RET-16`). Absent on documents created before the
    /// version columns existed.
    #[serde(rename = "docKey", skip_serializing_if = "Option::is_none")]
    pub doc_key: Option<String>,
    #[serde(rename = "version", skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(
        rename = "supersedesDocumentId",
        skip_serializing_if = "Option::is_none"
    )]
    pub supersedes_document_id: Option<String>,
    #[serde(rename = "effectiveFrom", skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
    #[serde(rename = "effectiveTo", skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<String>,
    /// True for the current version; unversioned documents are current too.
    #[serde(rename = "isLatest")]
    pub is_latest: bool,
}

/// List the caller's documents.
///
/// `GET /v1/documents` — the caller's documents. Needs the `kb:read` scope. A
/// key bound to some knowledge bases sees only the documents in them.
///
/// The list is paged: the response carries `nextCursor` and `total`, and the
/// caller sends the cursor back to continue. A `limit` above the cap is
/// clamped, never refused — and the response says so (`limitClamped`), because
/// a caller that asked for 500 and got 200 has to know which it got.
#[utoipa::path(
    get,
    path = "/v1/documents",
    tag = "documents",
    params(ListDocumentsQuery),
    responses(
        (status = 200, description = "The documents", body = DocumentListResponse),
        (status = 400, description = "The cursor is not one this server issued", body = ErrorBody),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:read scope", body = ErrorBody),
        (status = 500, description = "The store could not be read", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn list_documents(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Query(q): Query<ListDocumentsQuery>,
) -> Result<Json<DocumentListResponse>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    // A limit above the cap is clamped, not refused: the callers that ask for
    // 500 documents keep working. `clamped` says the request had to change,
    // so the response can report the limit that was actually applied.
    let requested_limit = q
        .limit
        .as_deref()
        .and_then(|v| v.trim().parse::<u32>().ok());
    let page_size = kb_core::pagination::resolve_page_size(requested_limit);
    // A cursor this server did not issue is a `400`, not an empty page: an
    // empty page is a valid answer that would look like the end of the list.
    let cursor = match q.cursor.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(token) => Some(kb_core::pagination::decode(token).ok_or_else(|| {
            ApiError::BadRequest("Invalid 'cursor': not a cursor this server issued.".to_string())
        })?),
        None => None,
    };
    let raw_kbs: Vec<String> = q
        .knowledge_base_ids
        .as_deref()
        .map(parse_list)
        .unwrap_or_default();
    // An empty parameter parses to no ids, which is "absent".
    let requested = (!raw_kbs.is_empty()).then_some(raw_kbs.as_slice());
    let kb_filter: Vec<KnowledgeBaseId> = match restrict_kbs(&auth, requested) {
        Some(v) => {
            if v.is_empty() {
                // A restricted key asked for bases it does not hold. Answering
                // with the whole tenant (what an empty filter means) would
                // hand it documents it may not see.
                return Ok(Json(DocumentListResponse {
                    documents: Vec::new(),
                    next_cursor: None,
                    total: 0,
                    limit: page_size.limit,
                    limit_clamped: page_size.clamped,
                }));
            }
            v.into_iter().map(KnowledgeBaseId::new_unchecked).collect()
        }
        None => Vec::new(),
    };
    let page = state
        .documents
        .list_page(
            &auth.tenant_id,
            page_size.limit,
            &kb_filter,
            q.external_ref.as_deref(),
            cursor.as_ref(),
        )
        .await
        .map_err(ApiError::from)?;
    let documents = page
        .documents
        .into_iter()
        .map(|d| {
            let quality = read_document_quality(d.metadata.as_ref());
            DocumentSummary {
                id: d.id.0,
                title: d.title,
                categories: d.categories,
                subcategory: d.subcategory,
                file_type: d.file_type,
                file_size: d.file_size,
                status: d.status,
                external_ref: d.external_ref,
                created_at: d.created_at,
                knowledge_base_ids: d.knowledge_base_ids,
                pages: quality.pages,
                chars_extracted: quality.chars_extracted,
                degrades: quality.degrades,
                sha256: d.sha256,
                doc_key: d.doc_key,
                version: d.version,
                supersedes_document_id: d.supersedes_document_id,
                effective_from: d.effective_from,
                effective_to: d.effective_to,
                is_latest: d.is_latest,
            }
        })
        .collect();
    Ok(Json(DocumentListResponse {
        documents,
        next_cursor: page.next_cursor,
        total: page.total,
        limit: page_size.limit,
        limit_clamped: page_size.clamped,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FigureView {
    /// The figure's storage key.
    #[serde(rename = "assetKey")]
    pub asset_key: String,
    /// The 0-based page the figure is on.
    pub page: u32,
    /// The figure's caption, when it has one.
    pub caption: Option<String>,
    /// The figure's kind (`figure`, `chart`, `photo`, …).
    pub kind: String,
}

/// Figures from `Document.metadata.figures`, same rules as TS readDocumentFigures:
/// non-object entries are skipped, `assetKey` must be a non-empty string,
/// `page` defaults to 0, `kind` comes from `type` and defaults to "figure".
#[must_use]
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

/// The extraction quality of a document, as the ingest worker recorded it
/// (`#29`).
///
/// Additive: a document ingested before the gate existed has no numbers, and
/// the fields are then null / empty rather than zero, so a caller can tell
/// "not measured" from "measured and empty".
#[derive(Debug, Clone, Default)]
pub struct DocumentQuality {
    /// How many pages the document has.
    pub pages: Option<u32>,
    /// How much text the document produced, in characters.
    pub chars_extracted: Option<u64>,
    /// What kept the document from being fully searchable, one line each.
    ///
    /// Empty when nothing was wrong — or when the gate is off and no
    /// complaint was recorded.
    pub degrades: Vec<String>,
}

/// The quality numbers and the complaints from `Document.metadata`.
///
/// The ingest worker writes `metadata.quality` (the numbers) and
/// `metadata.degrades` (the complaints) when it indexes a document; a
/// document indexed before that has neither.
#[must_use]
pub fn read_document_quality(metadata: Option<&serde_json::Value>) -> DocumentQuality {
    let quality = metadata.and_then(|m| m.get("quality"));
    let pages = quality
        .and_then(|q| q.get("pages"))
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as u32);
    let chars_extracted = quality
        .and_then(|q| q.get("charsExtracted"))
        .and_then(serde_json::Value::as_u64);
    let degrades = metadata
        .and_then(|m| m.get("degrades"))
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    DocumentQuality {
        pages,
        chars_extracted,
        degrades,
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DocumentDetailResponse {
    /// The document's row.
    pub document: DocumentDetail,
    /// The document's chunks, in reading order.
    pub chunks: Vec<DocumentChunkView>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DocumentDetail {
    /// The document's id.
    pub id: String,
    /// The document's title.
    pub title: String,
    /// The categories the document is filed under.
    pub categories: Vec<String>,
    /// The subcategory, when it has one.
    pub subcategory: Option<String>,
    /// The file's type, as the store recorded it.
    #[serde(rename = "fileType")]
    pub file_type: Option<String>,
    /// The file's size in bytes.
    #[serde(rename = "fileSize")]
    pub file_size: Option<i32>,
    /// The file's MIME type.
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    /// `processing`, `waiting_for_ocr`, `needs_ocr`, `ready` or `failed`.
    pub status: String,
    /// When the document was created.
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// When the document was last changed.
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    /// The knowledge bases the document belongs to.
    #[serde(rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Vec<String>,
    /// How many chunks the document has.
    #[serde(rename = "chunkCount")]
    pub chunk_count: u32,
    /// The document's figures.
    pub figures: Vec<FigureView>,
    /// How many pages the document has (`#29`).
    #[serde(rename = "pages")]
    pub pages: Option<u32>,
    /// How much text the document really produced, in characters (`#29`).
    #[serde(rename = "charsExtracted")]
    pub chars_extracted: Option<u64>,
    /// What kept the document from being fully searchable, one line each
    /// (`#29`).
    #[serde(rename = "degrades")]
    pub degrades: Vec<String>,
    /// The SHA-256 of the uploaded bytes, when the document was stored with
    /// one. Null for a document uploaded before the hash was recorded.
    #[serde(rename = "sha256")]
    pub sha256: Option<String>,
    /// Version metadata (`RET-16`). Absent on documents created before the
    /// version columns existed.
    #[serde(rename = "docKey", skip_serializing_if = "Option::is_none")]
    pub doc_key: Option<String>,
    #[serde(rename = "version", skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(
        rename = "supersedesDocumentId",
        skip_serializing_if = "Option::is_none"
    )]
    pub supersedes_document_id: Option<String>,
    #[serde(rename = "effectiveFrom", skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
    #[serde(rename = "effectiveTo", skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<String>,
    /// True for the current version; unversioned documents are current too.
    #[serde(rename = "isLatest")]
    pub is_latest: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DocumentChunkView {
    /// The chunk's id, in the wire form `document_chunk:⟨{bare}⟩`.
    pub id: String,
    /// The chunk's 0-based position in the document.
    #[serde(rename = "chunkIndex")]
    pub chunk_index: u32,
    /// The chunk's text.
    pub content: String,
    /// The chunk's kind (`text`, `table_summary`, `table_row`, …).
    #[serde(rename = "chunkType")]
    pub chunk_type: Option<String>,
    /// The heading path the chunk sits under.
    pub section: Option<String>,
    /// The 0-based page the chunk is on.
    pub page: Option<u32>,
}

/// Read one document and its chunks.
///
/// `GET /v1/documents/{id}` — one document and its chunks. Needs the
/// `kb:read` scope. A document outside the caller's tenant, or outside a
/// limited key's knowledge bases, answers the same 404 as an unknown id.
#[utoipa::path(
    get,
    path = "/v1/documents/{id}",
    tag = "documents",
    params(("id" = String, Path, description = "The document's id.")),
    responses(
        (status = 200, description = "The document and its chunks", body = DocumentDetailResponse),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:read scope", body = ErrorBody),
        (status = 404, description = "No such document is visible to this key", body = ErrorBody),
        (status = 500, description = "A store could not be read", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn get_document(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Json<DocumentDetailResponse>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let meta = find_visible_document(&state, &auth, &id).await?;
    let quality = read_document_quality(meta.metadata.as_ref());
    let chunks = state
        .index
        .ordered_chunks(&auth.tenant_id, &meta.id)
        .await
        .map_err(ApiError::from)?;
    let chunk_views: Vec<DocumentChunkView> = chunks
        .iter()
        .map(|c| DocumentChunkView {
            // Bun's `String(row.id)`: `document_chunk:⟨{bare}⟩`. The same
            // boundary rule as a search hit's id (see `dto::chunk_id_for_client`).
            id: crate::dto::chunk_id_for_client(&c.unit_id.0),
            chunk_index: c.chunk_index,
            content: c.content.clone(),
            chunk_type: Some(c.chunk_type.as_storage().to_string()),
            section: c
                .metadata
                .get("section")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            page: c.page,
        })
        .collect();
    let chunk_count = chunk_views.len() as u32;
    Ok(Json(DocumentDetailResponse {
        document: DocumentDetail {
            id: meta.id.0,
            title: meta.title,
            categories: meta.categories,
            subcategory: meta.subcategory,
            file_type: meta.file_type,
            file_size: meta.file_size,
            mime_type: meta.mime_type,
            status: meta.status,
            created_at: meta.created_at,
            updated_at: meta.updated_at,
            knowledge_base_ids: meta.knowledge_base_ids,
            chunk_count,
            figures: read_document_figures(meta.metadata.as_ref()),
            pages: quality.pages,
            chars_extracted: quality.chars_extracted,
            degrades: quality.degrades,
            sha256: meta.sha256,
            doc_key: meta.doc_key,
            version: meta.version,
            supersedes_document_id: meta.supersedes_document_id,
            effective_from: meta.effective_from,
            effective_to: meta.effective_to,
            is_latest: meta.is_latest,
        },
        chunks: chunk_views,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchDocumentBody {
    /// The document's new title.
    pub title: Option<String>,
    /// The document's new categories.
    pub categories: Option<Vec<String>>,
    /// `Some(None)` clears the column; absent leaves it alone.
    pub subcategory: Option<Option<String>>,
    /// The knowledge bases the document belongs to.
    #[serde(rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Option<Vec<String>>,
    /// Version metadata (`RET-16`). Absent leaves the column alone; a bad date
    /// is a 400. Dates accept `YYYY-MM-DD` or a full ISO timestamp.
    #[serde(rename = "docKey")]
    pub doc_key: Option<String>,
    #[serde(rename = "version")]
    pub version: Option<String>,
    #[serde(rename = "effectiveFrom")]
    pub effective_from: Option<String>,
    #[serde(rename = "effectiveTo")]
    pub effective_to: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PatchedDocumentResponse {
    /// The document's row, after the patch.
    pub document: PatchedDocument,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PatchedDocument {
    /// The document's id.
    pub id: String,
    /// The document's title.
    pub title: String,
    /// The categories the document is filed under.
    pub categories: Vec<String>,
    /// The subcategory, when it has one.
    pub subcategory: Option<String>,
    /// The file's type, as the store recorded it.
    #[serde(rename = "fileType")]
    pub file_type: Option<String>,
    /// The file's size in bytes.
    #[serde(rename = "fileSize")]
    pub file_size: Option<i32>,
    /// The file's MIME type.
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    /// `processing`, `ready` or `failed`.
    pub status: String,
    /// When the document was created.
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// When the document was last changed.
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    /// The knowledge bases the document belongs to.
    #[serde(rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Vec<String>,
    /// Version metadata after the patch (`RET-16`).
    #[serde(rename = "docKey", skip_serializing_if = "Option::is_none")]
    pub doc_key: Option<String>,
    #[serde(rename = "version", skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(
        rename = "supersedesDocumentId",
        skip_serializing_if = "Option::is_none"
    )]
    pub supersedes_document_id: Option<String>,
    #[serde(rename = "effectiveFrom", skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
    #[serde(rename = "effectiveTo", skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<String>,
    /// True for the current version; unversioned documents are current too.
    #[serde(rename = "isLatest")]
    pub is_latest: bool,
}

/// Change a document's metadata.
///
/// `PATCH /v1/documents/{id}` — change a document's metadata. Needs the
/// `kb:write` scope. Absent members are left alone; `subcategory` set to
/// `null` clears the column.
#[utoipa::path(
    patch,
    path = "/v1/documents/{id}",
    tag = "documents",
    params(("id" = String, Path, description = "The document's id.")),
    request_body = PatchDocumentBody,
    responses(
        (status = 200, description = "The document, after the patch", body = PatchedDocumentResponse),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = ErrorBody),
        (status = 404, description = "No such document is visible to this key", body = ErrorBody),
        (status = 500, description = "The store could not be written", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn patch_document(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
    body: Option<Json<PatchDocumentBody>>,
) -> Result<Json<PatchedDocumentResponse>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    let doc_id = DocumentId::new_unchecked(id.clone());
    // Existence check inside the tenant before anything is written, so another
    // tenant's id 404s with nothing changed.
    find_visible_document(&state, &auth, &id).await?;
    let Json(body) = body.unwrap_or(Json(PatchDocumentBody {
        title: None,
        categories: None,
        subcategory: None,
        knowledge_base_ids: None,
        doc_key: None,
        version: None,
        effective_from: None,
        effective_to: None,
    }));

    let knowledge_base_ids = match body.knowledge_base_ids {
        None => None,
        Some(requested) => {
            // Narrow to what the key may bind AND what this tenant owns: a
            // caller-supplied id for another tenant's knowledge base is dropped
            // rather than creating a cross-tenant link.
            let restricted = restrict_kbs(&auth, Some(&requested)).unwrap_or_default();
            let owned = state
                .documents
                .filter_owned_kb_ids(
                    &auth.tenant_id,
                    &restricted
                        .iter()
                        .map(|k| KnowledgeBaseId::new_unchecked(k.clone()))
                        .collect::<Vec<_>>(),
                )
                .await
                .map_err(ApiError::from)?;
            Some(owned)
        }
    };

    // RET-16: validate before writing, so a bad date cannot leave the row in a
    // half-patched state. Absent fields keep whatever the row already had.
    let effective_from = parse_version_date("effectiveFrom", body.effective_from.as_deref())?;
    let effective_to = parse_version_date("effectiveTo", body.effective_to.as_deref())?;
    let patch = kb_ports::PatchDocument {
        title: body.title,
        categories: body.categories,
        subcategory: body.subcategory,
        knowledge_base_ids,
        doc_key: body.doc_key,
        version: body.version,
        effective_from,
        effective_to,
    };
    let updated = state
        .documents
        .patch_document(&auth.tenant_id, &doc_id, &patch)
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;

    Ok(Json(PatchedDocumentResponse {
        document: PatchedDocument {
            id: updated.id.0,
            title: updated.title,
            categories: updated.categories,
            subcategory: updated.subcategory,
            file_type: updated.file_type,
            file_size: updated.file_size,
            mime_type: updated.mime_type,
            status: updated.status,
            created_at: updated.created_at,
            updated_at: updated.updated_at,
            knowledge_base_ids: updated.knowledge_base_ids,
            doc_key: updated.doc_key,
            version: updated.version,
            supersedes_document_id: updated.supersedes_document_id,
            effective_from: updated.effective_from,
            effective_to: updated.effective_to,
            is_latest: updated.is_latest,
        },
    }))
}

/// Delete a document, softly or for good.
///
/// `DELETE /v1/documents/{id}` — mirrors TS deleteDocument: needs `kb:write`;
/// soft delete sets `deletedAt` and leaves chunks and the blob alone; hard
/// delete (`?hard=true`) removes the chunks, then the stored object, then the
/// row.
#[utoipa::path(
    delete,
    path = "/v1/documents/{id}",
    tag = "documents",
    params(
        ("id" = String, Path, description = "The document's id."),
        DeleteQuery
    ),
    responses(
        (status = 200, description = "The document was deleted", body = OkResponse),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:write scope", body = ErrorBody),
        (status = 404, description = "No such document is visible to this key", body = ErrorBody),
        (status = 500, description = "The store could not be written", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn delete_document(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:write") {
        return Err(ApiError::missing_scope("kb:write"));
    }
    let doc_id = DocumentId::new_unchecked(id.clone());
    let hard = q.hard.as_deref() == Some("true");

    // Resolve the document inside the caller's tenant before touching anything.
    // The Bun route 404s on the tenant-filtered lookup, so a hard delete must
    // not reach the chunk store first and wipe another tenant's chunks on its
    // way to the same 404. Soft-deleted documents are in scope here: Bun's
    // delete route does not filter on `deletedAt`, so a hard delete can follow
    // a soft delete.
    let doc = find_document(&state, &auth, &id, true).await?;

    if !hard {
        let deleted = state
            .documents
            .soft_delete(&auth.tenant_id, &doc_id)
            .await
            .map_err(ApiError::from)?;
        if !deleted {
            return Err(ApiError::NotFound);
        }
        return Ok(Json(serde_json::json!({ "ok": true })));
    }

    // Chunks first, then the graph, then the stored object, then the row. A
    // chunk-delete, graph-delete or blob-delete failure is logged and does not
    // stop the row delete: the row is what makes the document invisible, and
    // leaving it behind would keep a document the user deleted in their list.
    //
    // The graph goes before the row for a specific reason. A mention or relation
    // left behind would keep a path through a document nobody can see any more,
    // and a path is exactly what the scope rule is about — so the cascade is
    // best-effort but it is not optional.
    if let Err(err) = state.index.delete_document(&auth.tenant_id, &doc.id).await {
        tracing::warn!(error = %err, "chunk delete failed during hard delete");
    }
    if let Some(arm) = state.engine.inputs.page_arm.as_ref()
        && let Err(err) = arm.index.delete_document(&auth.tenant_id, &doc.id).await
    {
        tracing::warn!(error = %err, "page-image delete failed during hard delete");
    }
    // The page JPEGs the vision rerank reads. Their count is what ingest
    // recorded on the document; without it there is nothing to address, so the
    // delete is skipped rather than guessed. Best-effort: a bucket that refuses
    // one page must not keep the row the user deleted.
    let page_images = doc
        .metadata
        .as_ref()
        .and_then(|m| m.get("pageImages"))
        .and_then(|p| p.get("indexed"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    for page in 1..=page_images {
        let page = u32::try_from(page).unwrap_or(u32::MAX);
        let key = kb_ports::page_image_path(Some(&auth.tenant_id.0), &doc.id.0, page);
        if let Err(err) = state.blob.delete(&key).await {
            tracing::warn!(error = %err, %key, "page-image blob delete failed during hard delete");
        }
    }
    // Figure and picture assets ingest stored (`metadata.figures`). Only keys
    // under this document's own asset prefix are deleted, so a metadata value
    // can never name someone else's object.
    let asset_prefix = kb_ports::asset_path(Some(&auth.tenant_id.0), &doc.id.0, "");
    let asset_keys: Vec<String> = doc
        .metadata
        .as_ref()
        .and_then(|m| m.get("figures"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f.get("assetKey").and_then(serde_json::Value::as_str))
        .filter(|k| k.starts_with(&asset_prefix) && !k.contains(".."))
        .map(str::to_string)
        .collect();
    for key in asset_keys {
        if let Err(err) = state.blob.delete(&key).await {
            tracing::warn!(error = %err, %key, "asset blob delete failed during hard delete");
        }
    }
    if let Some(graph) = state.graph.as_ref()
        && let Err(err) = graph.delete_document(&auth.tenant_id, &doc_id).await
    {
        tracing::warn!(error = %err, "graph delete failed during hard delete");
    }
    // The document's tables (`TBL-1`): every version's rows and their Parquet
    // grids. Best-effort like the other cascades, but it must be attempted:
    // a table left behind would stay queryable after the document is gone.
    if let Some(tables) = state.tables.as_ref()
        && let Err(err) = tables
            .delete_document_tables(&auth.tenant_id, &doc_id)
            .await
    {
        tracing::warn!(error = %err, "table delete failed during hard delete");
    }
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
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// The query `DELETE /v1/documents/{id}` accepts.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteQuery {
    /// `"true"` removes the chunks, the stored original and the row. Anything
    /// else (or nothing) is the soft delete.
    #[serde(default)]
    pub hard: Option<String>,
}

/// Resolve a document id inside the caller's tenant, or 404.
///
/// Every document-addressed route goes through this one helper: the id is
/// caller-supplied and the chunk store has no tenant of its own, so nothing that keys
/// on an id may run before the tenant check.
pub(crate) async fn find_visible_document(
    state: &AppState,
    auth: &AuthContext,
    id: &str,
) -> Result<kb_ports::DocumentMeta, ApiError> {
    find_document(state, auth, id, false).await
}

/// The delete path is the one lookup that also sees soft-deleted documents, so
/// `?hard=true` can follow a soft delete.
///
/// The tenant check is not enough on its own: a key can be limited to some
/// knowledge bases, and a document outside them must answer exactly the same
/// 404 as a document in another tenant, so the limit never leaks an id's
/// existence. A document in no knowledge base is outside every limited key,
/// matching how search and the document list already treat it.
pub(crate) async fn find_document(
    state: &AppState,
    auth: &AuthContext,
    id: &str,
    include_deleted: bool,
) -> Result<kb_ports::DocumentMeta, ApiError> {
    let doc = state
        .documents
        .find_meta_by_id(
            &auth.tenant_id,
            &DocumentId::new_unchecked(id),
            include_deleted,
        )
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    if !key_can_see_document(auth, &doc) {
        return Err(ApiError::NotFound);
    }
    Ok(doc)
}

/// Whether a key's knowledge-base limit admits a document.
///
/// An empty limit means the whole tenant. A non-empty limit admits a document
/// only when at least one of its knowledge bases is in the limit; a document in
/// no knowledge base is therefore not visible to a limited key.
#[must_use]
pub(crate) fn key_can_see_document(auth: &AuthContext, doc: &kb_ports::DocumentMeta) -> bool {
    auth.knowledge_base_ids.is_empty()
        || doc
            .knowledge_base_ids
            .iter()
            .any(|kb| auth.knowledge_base_ids.iter().any(|k| k.0 == *kb))
}

/// 404 when a limited key asks about a document outside its knowledge bases.
///
/// Unlike [`find_document`], an unlimited key is not looked up at all, so a job
/// that references a document row which no longer exists keeps answering
/// exactly as it did before the limit existed.
pub(crate) async fn ensure_document_visible(
    state: &AppState,
    auth: &AuthContext,
    id: &str,
) -> Result<(), ApiError> {
    if auth.knowledge_base_ids.is_empty() {
        return Ok(());
    }
    find_document(state, auth, id, false).await.map(|_| ())
}

// ─── Knowledge bases ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct KbListResponse {
    /// The knowledge bases the caller may see.
    #[serde(rename = "knowledgeBases")]
    pub knowledge_bases: Vec<KbSummary>,
}

/// One knowledge base, as `GET /v1/knowledge-bases` returns it.
#[derive(Debug, Serialize, ToSchema)]
pub struct KbSummary {
    /// The base's id.
    pub id: String,
    /// The base's name.
    pub name: String,
    /// The base's description, when it has one.
    pub description: Option<String>,
    /// The base's display colour, when it has one.
    pub color: Option<String>,
    /// How many live documents the base holds.
    #[serde(rename = "documentCount")]
    pub document_count: u32,
}

/// List the caller's knowledge bases.
///
/// `GET /v1/knowledge-bases` — the caller's knowledge bases. Needs the
/// `kb:read` scope. A key bound to some bases sees only those.
#[utoipa::path(
    get,
    path = "/v1/knowledge-bases",
    tag = "taxonomy",
    responses(
        (status = 200, description = "The knowledge bases", body = KbListResponse),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:read scope", body = ErrorBody),
        (status = 500, description = "The store could not be read", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn list_knowledge_bases(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
) -> Result<Json<KbListResponse>, ApiError> {
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
    let knowledge_bases = rows
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
    Ok(Json(KbListResponse { knowledge_bases }))
}

/// `parseList` from `src/service/api.ts`: a JSON array when the value parses as
/// one, otherwise comma-separated with surrounding whitespace trimmed.
#[must_use]
pub fn parse_list(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(value) {
        return items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect();
    }
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_list, read_document_figures};
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

    #[test]
    fn parse_list_reads_json_arrays_and_csv() {
        assert!(parse_list("").is_empty());
        assert_eq!(parse_list(r#"["a","b"]"#), vec!["a", "b"]);
        // Non-strings are dropped, as the TypeScript filter does.
        assert_eq!(parse_list(r#"["a",1,null]"#), vec!["a"]);
        // A JSON value that is not an array falls through to CSV.
        assert_eq!(parse_list(r#"{"a":1}"#), vec![r#"{"a":1}"#]);
        assert_eq!(parse_list("a, b ,,c"), vec!["a", "b", "c"]);
        assert_eq!(parse_list("a"), vec!["a"]);
    }
}
