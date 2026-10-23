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

mod jobs;
mod media;
mod taxonomy;
mod upload;

use std::collections::{HashMap, HashSet};
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
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use kb_core::policy::MimeRegistry;
use kb_engine::timing::{Stopwatch, SystemClock};
use kb_ports::{
    AuthContext, DocumentFilter, DocumentId, DocumentMeta, KnowledgeBaseId, ScopeRequest, SearchHit,
};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::dto::{
    ChunkDto, SearchRequestDto, SearchResponseDto, SearchStatsDto, StageDetailDto, StageTimingsDto,
    splice_json_member,
};
use crate::error::ApiError;
use crate::state::AppState;

/// Largest accepted upload, in bytes: `KB_MAX_FILE_BYTES`, read once per
/// process, defaulting to Bun's 50 MB (`src/lib/files/mime-types.ts`) so an
/// unset variable keeps the two runtimes identical. Large manuals and
/// textbooks (ViDoRe's 943-page computer science book is 50.9 MB) need more.
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

/// Upper bound on a multipart upload body. The file itself is capped at
/// [`max_file_bytes`] by the upload route; the slack covers the part headers,
/// the form fields and the multipart framing around a file exactly at the limit.
pub fn upload_body_limit() -> usize {
    usize::try_from(max_file_bytes() + 1024 * 1024).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod max_file_bytes_tests {
    use super::parse_max_file_bytes;

    #[test]
    fn unset_or_invalid_keeps_buns_50_mb() {
        let bun = 50 * 1024 * 1024;
        assert_eq!(parse_max_file_bytes(None), bun);
        assert_eq!(parse_max_file_bytes(Some("lots")), bun);
        assert_eq!(parse_max_file_bytes(Some("0")), bun);
        assert_eq!(parse_max_file_bytes(Some(" 104857600 ")), 104_857_600);
    }
}

/// Top-level router. Authenticated routes go here.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/search", post(search))
        .route(
            "/v1/documents",
            get(list_documents).post(upload::ingest_document),
        )
        .route(
            "/v1/documents/:id",
            get(get_document)
                .patch(patch_document)
                .delete(delete_document),
        )
        .route("/v1/documents/:id/raw", get(media::get_document_raw))
        .route("/v1/documents/:id/assets", get(media::get_document_asset))
        .route(
            "/v1/documents/:id/intelligence",
            get(media::get_document_intelligence),
        )
        .route("/v1/jobs/:id", get(jobs::get_job))
        .route("/v1/jobs/:id/retry", post(jobs::retry_job))
        .route(
            "/v1/knowledge-bases",
            get(list_knowledge_bases).post(taxonomy::create_knowledge_base),
        )
        .route(
            "/v1/knowledge-bases/:id",
            patch(taxonomy::patch_knowledge_base).delete(taxonomy::delete_knowledge_base),
        )
        .route(
            "/v1/categories",
            get(taxonomy::list_categories).post(taxonomy::create_category),
        )
        .route(
            "/v1/categories/:id",
            patch(taxonomy::patch_category).delete(taxonomy::delete_category),
        )
        .route("/v1/events", get(crate::events::events_stream))
        .layer(DefaultBodyLimit::max(upload_body_limit()))
}

/// Unauthenticated router (health + formats).
pub fn health_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/v1/health", get(health))
        .route("/v1/formats", get(formats))
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

async fn health(State(state): State<Arc<AppState>>) -> Response {
    // Each dependency is probed independently and reported as a boolean: a
    // liveness endpoint that fails the whole request tells an operator nothing
    // about which side is down.
    let postgres = state.documents.health_check().await.is_ok();
    let vectors = state.index.health_check().await.is_ok();
    let ok = postgres && vectors;
    let body = HealthResponse {
        ok,
        checks: HealthChecks { postgres, vectors },
        version: state.version.clone(),
    };
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body)).into_response()
}

#[derive(Debug, Serialize)]
pub struct FormatsResponse {
    extensions: &'static [&'static str],
    #[serde(rename = "maxFileBytes")]
    max_file_bytes: u64,
}

async fn formats() -> Json<FormatsResponse> {
    Json(FormatsResponse {
        extensions: MimeRegistry::kb_accepted_extensions(),
        max_file_bytes: max_file_bytes(),
    })
}

// ─── Search ──────────────────────────────────────────────────────────────────

async fn search(
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
        let alive_map: HashMap<DocumentId, DocumentMeta> =
            alive.iter().map(|m| (m.id.clone(), m.clone())).collect();
        let full_scope = state
            .documents
            .build_scope(&ScopeRequest {
                tenant: auth.tenant_id.clone(),
                filter: DocumentFilter::default(),
            })
            .await
            .map_err(ApiError::from)?;
        let scope = full_scope.narrowed_to(&requested);
        let scope_build_ms = scope_sw.elapsed_ms();
        let embed_sw = Stopwatch::start(&SystemClock);
        let embedded = state
            .embedder
            .embed_query_detailed(&query)
            .await
            .map_err(|e| ApiError::Internal(format!("embed: {e}")))?;
        let query_vec = embedded.vector;
        let query_embed_ms = embed_sw.elapsed_ms();
        let vector_sw = Stopwatch::start(&SystemClock);
        let query_hybrid = kb_ports::HybridQuery {
            dense: Some(query_vec),
            sparse: None,
            per_arm_k: max_chunks as usize,
            k: max_chunks as usize,
            fusion: kb_ports::Fusion::None,
        };
        let cands = state
            .index
            .search(&query_hybrid, &scope)
            .await
            .map_err(ApiError::from)?;
        let mut hits: Vec<SearchHit> = cands
            .into_iter()
            .map(kb_engine::retrieval::candidate_to_hit)
            .collect();
        let vector_arm_ms = vector_sw.elapsed_ms();
        hits.retain(|h| h.similarity >= 0.30);
        hits.truncate(max_chunks as usize);
        for h in hits.iter_mut() {
            if let Some(meta) = alive_map.get(&h.chunk.document_id) {
                h.chunk.document_title = meta.title.clone();
                h.chunk.categories = meta.categories.clone();
                h.chunk.subcategory = meta.subcategory.clone();
            }
        }
        let vector_results = hits.len();
        let chunk_dtos: Vec<ChunkDto> = hits.into_iter().map(ChunkDto::from).collect();
        let context = if body.format.as_deref() == Some("context") {
            Some(
                chunk_dtos
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("[{}] {}", i + 1, c.content))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            )
        } else {
            None
        };
        let total_results = chunk_dtos.len();
        let stats = StageTimingsDto {
            scope_build: scope_build_ms,
            arms: query_embed_ms + vector_arm_ms,
            detail: StageDetailDto {
                engine_total: query_embed_ms + vector_arm_ms,
                query_embed: query_embed_ms,
                query_embed_cached: embedded.cached,
                vector_arm: vector_arm_ms,
                ..StageDetailDto::default()
            },
            ..StageTimingsDto::default()
        };
        let response = SearchResponseDto {
            chunks: Some(chunk_dtos),
            sources: None,
            context,
            results: None,
            stats: Some(search_stats(stats, vector_results, total_results)),
            note: None,
            degrades: Vec::new(),
        };
        return Ok(finish_search(response, route_sw));
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
                stats: Some(search_stats(StageTimingsDto::default(), 0, 0)),
                note: None,
                degrades: Vec::new(),
            },
            route_sw,
        ));
    }

    // Branch 3: standard retrieval
    let filter = DocumentFilter {
        knowledge_base_ids: requested_kbs
            .unwrap_or_default()
            .into_iter()
            .map(KnowledgeBaseId::new_unchecked)
            .collect(),
        category: body.category.clone(),
    };
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

    let outcome = state
        .engine
        .retrieve(&query, &scope, Some(max_chunks))
        .await
        .map_err(ApiError::from)?;
    let mut response = SearchResponseDto::from(outcome);
    if let Some(stats) = response.stats.as_mut() {
        // The route knows two stages the engine does not: building the scope,
        // and everything above it in the handler.
        stats.stage_timings_ms.scope_build = scope_build_ms;
    }
    Ok(finish_search(response, route_sw))
}

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
        )
    });
    let mut stages = stats.stage_timings_ms;
    stages.serialise = serialise_ms;
    stages.total = route_sw.elapsed_ms();
    let stats = search_stats(stages, stats.vector_results, stats.total_results);
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

#[derive(Debug, Deserialize)]
pub struct ListDocumentsQuery {
    /// Kept as a string so an unparsable value degrades to the default instead
    /// of rejecting the request with a body the API never produces.
    pub limit: Option<String>,
    /// Bun/TS name (camelCase), used by existing clients.
    #[serde(alias = "knowledge_base_ids", rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DocumentListResponse {
    pub documents: Vec<DocumentSummary>,
}

/// `GET /v1/documents` row, field for field as `listDocuments` selects it.
#[derive(Debug, Serialize)]
pub struct DocumentSummary {
    pub id: String,
    pub title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    #[serde(rename = "fileType")]
    pub file_type: Option<String>,
    #[serde(rename = "fileSize")]
    pub file_size: Option<i32>,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Vec<String>,
}

async fn list_documents(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Query(q): Query<ListDocumentsQuery>,
) -> Result<Json<DocumentListResponse>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let limit = q
        .limit
        .as_deref()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
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
                }));
            }
            v.into_iter().map(KnowledgeBaseId::new_unchecked).collect()
        }
        None => Vec::new(),
    };
    let docs = state
        .documents
        .list(&auth.tenant_id, limit, &kb_filter)
        .await
        .map_err(ApiError::from)?;
    let documents = docs
        .into_iter()
        .map(|d| DocumentSummary {
            id: d.id.0,
            title: d.title,
            categories: d.categories,
            subcategory: d.subcategory,
            file_type: d.file_type,
            file_size: d.file_size,
            status: d.status,
            created_at: d.created_at,
            knowledge_base_ids: d.knowledge_base_ids,
        })
        .collect();
    Ok(Json(DocumentListResponse { documents }))
}

#[derive(Debug, Serialize)]
pub struct FigureView {
    #[serde(rename = "assetKey")]
    pub asset_key: String,
    pub page: u32,
    pub caption: Option<String>,
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

#[derive(Debug, Serialize)]
pub struct DocumentDetailResponse {
    pub document: DocumentDetail,
    pub chunks: Vec<DocumentChunkView>,
}

#[derive(Debug, Serialize)]
pub struct DocumentDetail {
    pub id: String,
    pub title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    #[serde(rename = "fileType")]
    pub file_type: Option<String>,
    #[serde(rename = "fileSize")]
    pub file_size: Option<i32>,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Vec<String>,
    #[serde(rename = "chunkCount")]
    pub chunk_count: u32,
    pub figures: Vec<FigureView>,
}

#[derive(Debug, Serialize)]
pub struct DocumentChunkView {
    pub id: String,
    #[serde(rename = "chunkIndex")]
    pub chunk_index: u32,
    pub content: String,
    #[serde(rename = "chunkType")]
    pub chunk_type: Option<String>,
    pub section: Option<String>,
    pub page: Option<u32>,
}

async fn get_document(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Json<DocumentDetailResponse>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    let meta = find_visible_document(&state, &auth, &id).await?;
    let chunks = state
        .index
        .ordered_chunks(&auth.tenant_id, &meta.id)
        .await
        .map_err(ApiError::from)?;
    let chunk_views: Vec<DocumentChunkView> = chunks
        .iter()
        .map(|c| DocumentChunkView {
            id: c.unit_id.0.clone(),
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
        },
        chunks: chunk_views,
    }))
}

#[derive(Debug, Deserialize)]
pub struct PatchDocumentBody {
    pub title: Option<String>,
    pub categories: Option<Vec<String>>,
    /// `Some(None)` clears the column; absent leaves it alone.
    pub subcategory: Option<Option<String>>,
    #[serde(rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct PatchedDocumentResponse {
    pub document: PatchedDocument,
}

#[derive(Debug, Serialize)]
pub struct PatchedDocument {
    pub id: String,
    pub title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    #[serde(rename = "fileType")]
    pub file_type: Option<String>,
    #[serde(rename = "fileSize")]
    pub file_size: Option<i32>,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "knowledgeBaseIds")]
    pub knowledge_base_ids: Vec<String>,
}

async fn patch_document(
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

    let patch = kb_ports::PatchDocument {
        title: body.title,
        categories: body.categories,
        subcategory: body.subcategory,
        knowledge_base_ids,
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
        },
    }))
}

/// `DELETE /v1/documents/:id`. Mirrors TS deleteDocument: needs `kb:write`;
/// soft delete sets `deletedAt` and leaves chunks and the blob alone; hard
/// delete (`?hard=true`) removes the chunks, then the stored object, then the row.
async fn delete_document(
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
    if let Some(graph) = state.graph.as_ref()
        && let Err(err) = graph.delete_document(&auth.tenant_id, &doc_id).await
    {
        tracing::warn!(error = %err, "graph delete failed during hard delete");
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

#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    pub hard: Option<String>,
}

/// Resolve a document id inside the caller's tenant, or 404.
///
/// Every document-addressed route goes through this one helper: the id is
/// caller-supplied and SurrealDB has no tenant of its own, so nothing that keys
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
pub(crate) async fn find_document(
    state: &AppState,
    auth: &AuthContext,
    id: &str,
    include_deleted: bool,
) -> Result<kb_ports::DocumentMeta, ApiError> {
    state
        .documents
        .find_meta_by_id(
            &auth.tenant_id,
            &DocumentId::new_unchecked(id),
            include_deleted,
        )
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)
}

// ─── Knowledge bases ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct KbListResponse {
    #[serde(rename = "knowledgeBases")]
    pub knowledge_bases: Vec<KbSummary>,
}

#[derive(Debug, Serialize)]
pub struct KbSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    #[serde(rename = "documentCount")]
    pub document_count: u32,
}

async fn list_knowledge_bases(
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
