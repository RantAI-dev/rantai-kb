//! `GET /v1/documents/:id/content` — read on after a search, without searching
//! again (`RET-14`).
//!
//! An agent that found a chunk reads the surrounding section or pages in order,
//! under a character budget. The text is stitched by `kb-core` from the
//! vector-free chunk metadata read, with overlap removed and hard-boundary
//! paragraph separators restored, so page boundaries are invisible in the
//! output. A cursor carries the document id, the next chunk index, the end of
//! the original selection and the document's content hash: it cannot name
//! another document, and it fails with 409 if the document changed.
//!
//! Visibility is the exact check used by `/structure`: `kb:read`, the caller's
//! tenant, the key's knowledge-base limit and the soft-delete filter. Nothing
//! is read from the chunk store before it passes.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde::Serialize;
use utoipa::{IntoParams, ToSchema};

use kb_core::content::{ContentCursor, StitchChunk, WindowStart, stitch_from};
use kb_core::outline::{OutlineChunk, build_outline};
use kb_ports::{ChunkMeta, ChunkType, DocumentFilter, ScopeRequest};

use crate::auth::Authed;
use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

/// The default character budget, clamped to [`MIN_CHARS`]..[`MAX_CHARS`].
pub const DEFAULT_MAX_CHARS: usize = 8_000;
pub const MIN_CHARS: usize = 500;
pub const MAX_CHARS: usize = 50_000;

/// Degrade emitted when an old chunk has no spans and had to be appended whole.
pub const MISSING_SPANS_DEGRADE: &str = "content: some chunks have no spans; overlap may repeat";

/// The query string. Exactly one selector may be set; `maxChars` may accompany
/// any of them.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ContentQuery {
    /// A section id from `/structure`'s outline v2, e.g. `s3`.
    pub section: Option<String>,
    /// First zero-based page, inclusive. Used with `pageEnd`.
    pub page_start: Option<u32>,
    /// Last zero-based page, inclusive. Used with `pageStart`.
    pub page_end: Option<u32>,
    /// First chunk index, inclusive. Used with `chunkEnd`.
    pub chunk_start: Option<u32>,
    /// Last chunk index, inclusive. Used with `chunkStart`.
    pub chunk_end: Option<u32>,
    /// Opaque continuation token from a previous response.
    pub cursor: Option<String>,
    /// Character budget, clamped to 500..50000 (default 8000).
    pub max_chars: Option<usize>,
}

/// One kind of document selection. `All` means the whole document from chunk 0.
#[derive(Debug)]
enum Selector {
    All,
    Section(String),
    Pages(u32, u32),
    Chunks(u32, u32),
    Cursor(String),
}

fn selector(query: &ContentQuery) -> Result<Selector, ApiError> {
    let mut found: Vec<Selector> = Vec::new();
    if let Some(section) = &query.section {
        found.push(Selector::Section(section.clone()));
    }
    if query.page_start.is_some() || query.page_end.is_some() {
        let (Some(start), Some(end)) = (query.page_start, query.page_end) else {
            return Err(ApiError::BadRequest(
                "pageStart and pageEnd must be given together".to_string(),
            ));
        };
        found.push(Selector::Pages(start, end));
    }
    if query.chunk_start.is_some() || query.chunk_end.is_some() {
        let (Some(start), Some(end)) = (query.chunk_start, query.chunk_end) else {
            return Err(ApiError::BadRequest(
                "chunkStart and chunkEnd must be given together".to_string(),
            ));
        };
        if start > end {
            return Err(ApiError::BadRequest(
                "chunkStart must not be greater than chunkEnd".to_string(),
            ));
        }
        found.push(Selector::Chunks(start, end));
    }
    if let Some(cursor) = &query.cursor {
        found.push(Selector::Cursor(cursor.clone()));
    }
    match found.len() {
        0 => Ok(Selector::All),
        1 => Ok(found.remove(0)),
        _ => Err(ApiError::BadRequest(
            "use exactly one of section, pageStart/pageEnd, chunkStart/chunkEnd or cursor"
                .to_string(),
        )),
    }
}

/// A reference to another record of the same document. Ids and pages only —
/// no payloads, so reading on cannot turn into a second search.
#[derive(Debug, Serialize, ToSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ContentRef {
    Table { table_id: String, page: u32 },
    Figure { asset_key: String, page: u32 },
}

/// The chunk and page range actually returned, both inclusive. Pages are null
/// when no chunk in the window carries one.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentRange {
    pub chunk_start: u32,
    pub chunk_end: u32,
    pub page_start: Option<u32>,
    pub page_end: Option<u32>,
}

/// The response body, camelCase on the wire like every other `/v1` shape.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentContentResponse {
    pub document_id: String,
    /// Stitched text, with overlap removed. Page boundaries are not marked:
    /// concatenating successive cursor pages reproduces one unbounded read.
    pub text: String,
    pub range: ContentRange,
    /// True when more chunks of the selection remain after this page.
    pub truncated: bool,
    /// Base64url cursor for the next page, or null on the last one.
    pub next_cursor: Option<String>,
    pub refs: Vec<ContentRef>,
    /// Visible degradation, never a silent empty answer.
    pub degrades: Vec<String>,
}

/// Read a bounded, byte-exact slice of one document.
///
/// `GET /v1/documents/{id}/content`. The visibility check is identical to
/// [`crate::routes::structure::get_document_structure`]: a document of another
/// tenant, outside the key's knowledge-base limit or soft-deleted answers the
/// same 404. A cursor for another document is a 400, a changed content hash a
/// 409, so neither can be used to read outside the caller's scope.
#[utoipa::path(
    get,
    path = "/v1/documents/{id}/content",
    tag = "structure",
    params(
        ("id" = String, Path, description = "The document's id."),
        ContentQuery
    ),
    responses(
        (status = 200, description = "Stitched text, its range, refs and a continuation cursor", body = DocumentContentResponse),
        (status = 400, description = "Two selectors, a malformed cursor or a cursor for another document", body = ErrorBody),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:read scope", body = ErrorBody),
        (status = 404, description = "No such document is visible to this key", body = ErrorBody),
        (status = 409, description = "The document changed since the cursor was issued", body = ErrorBody),
        (status = 500, description = "The store could not be read", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn get_document_content(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
    Query(query): Query<ContentQuery>,
) -> Result<Json<DocumentContentResponse>, ApiError> {
    Ok(Json(
        get_document_content_inner(&state, &auth, &id, query).await?,
    ))
}

/// The shared body of `GET /v1/documents/{id}/content`.
///
/// The MCP `kb_read` tool calls this function, so its visibility check, cursor
/// validation and stitching are byte-for-byte the same as the HTTP route.
pub(crate) async fn get_document_content_inner(
    state: &Arc<AppState>,
    auth: &kb_ports::AuthContext,
    id: &str,
    query: ContentQuery,
) -> Result<DocumentContentResponse, ApiError> {
    if !crate::auth::has_scope(auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    // Visibility first, as in /structure: no metadata is read before this.
    let doc = crate::routes::find_visible_document(state, auth, id).await?;
    let scope = state
        .documents
        .build_scope(&ScopeRequest {
            tenant: auth.tenant_id.clone(),
            filter: DocumentFilter {
                knowledge_base_ids: auth.knowledge_base_ids.clone(),
                ..DocumentFilter::default()
            },
        })
        .await
        .map_err(ApiError::from)?
        .narrowed_to(std::slice::from_ref(&doc.id));

    // `sha256` is the content identity from ING-10. A document stored without
    // one falls back to its updatedAt timestamp, which PATCH bumps.
    let hash = doc
        .sha256
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| doc.updated_at.clone());
    // One vector-free scroll backs every selector, including the outline a
    // section id resolves through. Nothing is read before visibility passed.
    let meta = state
        .index
        .list_chunk_meta(&scope, &doc.id)
        .await
        .map_err(ApiError::from)?;
    let selection = selector(&query)?;
    let (raw_start, raw_end, continuation) = match &selection {
        Selector::All => (0u32, u32::MAX, false),
        Selector::Chunks(start, end) => (*start, *end, false),
        Selector::Pages(page_start, page_end) => {
            if page_start > page_end {
                return Err(ApiError::BadRequest(
                    "pageStart must not be greater than pageEnd".to_string(),
                ));
            }
            (*page_start, *page_end, false)
        }
        Selector::Section(section) => {
            let (start, end) = section_range(&doc, &meta, section)?;
            (start, end, false)
        }
        Selector::Cursor(value) => {
            let cursor = ContentCursor::decode(value)
                .ok_or_else(|| ApiError::BadRequest("malformed cursor".to_string()))?;
            if cursor.d != id {
                return Err(ApiError::BadRequest(
                    "cursor belongs to another document".to_string(),
                ));
            }
            if cursor.h != hash {
                return Err(ApiError::Conflict(
                    "document changed since cursor was issued",
                ));
            }
            (cursor.n, cursor.e, true)
        }
    };

    let selected: Vec<&ChunkMeta> = meta
        .iter()
        .filter(|chunk| match &selection {
            Selector::Pages(page_start, page_end) => chunk
                .page
                .is_some_and(|page| page >= *page_start && page <= *page_end),
            _ => chunk.chunk_index >= raw_start && chunk.chunk_index <= raw_end,
        })
        .collect();
    // Resolve selections to concrete chunk indices, so a cursor always carries
    // the end of the *original* selection and `range` never reports a page
    // number in a chunk slot.
    let last_index = meta.last().map_or(0, |chunk| chunk.chunk_index);
    let (start, end) = if matches!(&selection, Selector::Pages(..)) {
        (
            selected.first().map_or(0, |chunk| chunk.chunk_index),
            selected.last().map_or(0, |chunk| chunk.chunk_index),
        )
    } else {
        (
            raw_start,
            if raw_end == u32::MAX {
                last_index
            } else {
                raw_end
            },
        )
    };
    let stitched = {
        let chunks: Vec<StitchChunk> = selected
            .iter()
            .map(|meta| StitchChunk {
                chunk_index: meta.chunk_index,
                content: meta.content.clone(),
                char_start: meta.char_start,
                char_end: meta.char_end,
                overlap_prefix_len: meta.overlap_prefix_len,
            })
            .collect();
        // A window that does not start at chunk 0 must drop the first chunk's
        // overlap: the chunk it repeats is outside the window. Only a cursor
        // continuation puts a hard-boundary separator at the very front.
        let window_start = if continuation {
            WindowStart::Continuation
        } else {
            WindowStart::Selection
        };
        stitch_from(
            &chunks,
            query
                .max_chars
                .unwrap_or(DEFAULT_MAX_CHARS)
                .clamp(MIN_CHARS, MAX_CHARS),
            window_start,
        )
    };

    let emitted: Vec<&ChunkMeta> = selected
        .iter()
        .copied()
        .filter(|chunk| chunk.chunk_index <= stitched.last_chunk_index)
        .collect();
    let range = ContentRange {
        chunk_start: emitted.first().map_or(start, |chunk| chunk.chunk_index),
        chunk_end: emitted.last().map_or(end, |chunk| chunk.chunk_index),
        page_start: emitted.iter().filter_map(|chunk| chunk.page).min(),
        page_end: emitted.iter().filter_map(|chunk| chunk.page).max(),
    };
    let refs = collect_refs(state, &auth.tenant_id.0, &doc.id, &emitted, &range).await?;
    let mut degrades = Vec::new();
    if stitched.missing_spans {
        degrades.push(MISSING_SPANS_DEGRADE.to_string());
    }
    let next_cursor = stitched.truncated.then(|| {
        ContentCursor {
            d: doc.id.0.clone(),
            n: stitched.last_chunk_index.saturating_add(1),
            e: end,
            h: hash,
        }
        .encode()
    });

    Ok(DocumentContentResponse {
        document_id: doc.id.0.clone(),
        text: stitched.text,
        range,
        truncated: stitched.truncated,
        next_cursor,
        refs,
        degrades,
    })
}

/// Resolve a section id through the same outline v2 input as `/structure`.
fn section_range(
    doc: &kb_ports::DocumentMeta,
    meta: &[ChunkMeta],
    section: &str,
) -> Result<(u32, u32), ApiError> {
    let chunks: Vec<OutlineChunk> = meta
        .iter()
        .map(|meta| OutlineChunk {
            chunk_index: meta.chunk_index,
            page: meta.page,
            section: meta.section.clone(),
            content: meta.content.clone(),
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
    build_outline(&chunks, stored.as_ref())
        .into_iter()
        .find(|candidate| candidate.id == section)
        .map(|candidate| (candidate.chunk_start, candidate.chunk_end))
        .ok_or_else(|| ApiError::BadRequest(format!("unknown section: {section}")))
}

/// Figures come from the chunks we just emitted; tables from the table store,
/// filtered to the pages actually returned. Neither carries a payload.
async fn collect_refs(
    state: &AppState,
    tenant: &str,
    document: &kb_ports::DocumentId,
    emitted: &[&ChunkMeta],
    range: &ContentRange,
) -> Result<Vec<ContentRef>, ApiError> {
    let mut refs = Vec::new();
    let mut seen = Vec::new();
    for chunk in emitted {
        if chunk.chunk_type != ChunkType::Figure && chunk.asset_key.is_none() {
            continue;
        }
        let Some(asset_key) = chunk.asset_key.as_deref() else {
            // A figure chunk with no crop asset has no `assetKey` to reference.
            continue;
        };
        let Some(page) = chunk.page else {
            continue;
        };
        let key = asset_key.to_string();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key.clone());
        refs.push(ContentRef::Figure {
            asset_key: key,
            page,
        });
    }
    if let Some(tables) = state.tables.as_ref() {
        let tenant_id = kb_ports::TenantId(tenant.to_string());
        let records = tables
            .list_tables(&tenant_id, Some(document))
            .await
            .map_err(ApiError::from)?;
        for record in records {
            let Some(page) = range
                .page_start
                .zip(range.page_end)
                .and_then(|(low, high)| {
                    record
                        .page_span
                        .filter(|(start, end)| *start <= high && *end >= low)
                        .map(|(start, _)| start)
                })
            else {
                continue;
            };
            refs.push(ContentRef::Table {
                table_id: record.id.as_str().to_string(),
                page,
            });
        }
    }
    Ok(refs)
}
