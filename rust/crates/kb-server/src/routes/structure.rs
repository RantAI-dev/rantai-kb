//! `GET /v1/documents/:id/structure` — pages, figures and the chunk → page map
//! of one document, read from what ingest already stored.
//!
//! The read is additive: every field comes from a store record that existed
//! before this route. Pages and their text come from the page-image index's
//! page units (`ChunkType::Page`), because that is the only place the page's
//! own text is stored whole. Figures come from the text index's figure units,
//! which are exactly the figure chunks a search can return, merged with the
//! document row's `metadata.figures` for the bbox and caption that only the
//! document record carries. The chunk map comes from the text index's units.
//!
//! Fields the old ingest did not record are `null`, never inferred:
//! `hasTextLayer`/`ocrText` are recorded on page units from this release on
//! ([`kb_engine::PageTextSemantics`]), and `imageAssetKey` only when the page
//! unit says the JPEG write succeeded. A document ingested before that answers
//! `null` for all three; its pages and figures are still listed.
//!
//! # Caps
//!
//! A 2,000-page document must not become one unbounded store read. The route
//! asks the index for at most [`STRUCTURE_MAX_CHUNKS`] text units and at most
//! [`STRUCTURE_MAX_PAGES`] page units, and sets `truncated` when either cap is
//! reached. The adapter scrolls in windows (`DOCUMENT_SCROLL_WINDOW`); these
//! two counts are the caller's memory bound.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use kb_core::outline::{OutlineChunk, build_outline, estimated_chars, has_spans};
use kb_ports::{
    ChunkType, DocumentFilter, DocumentMeta, DocumentScope, ScopeRequest, page_image_path,
};
use utoipa::ToSchema;

use crate::auth::Authed;
use crate::dto::chunk_id_for_client;
use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

/// Text units read for the chunk → page map and the figure list.
pub const STRUCTURE_MAX_CHUNKS: usize = 20_000;
/// Page units read for the page list. The page arm's own `max_pages` defaults
/// to 500 and can be raised; this is the route's ceiling.
pub const STRUCTURE_MAX_PAGES: usize = 2_000;

/// The response body. camelCase on the wire, like every other `/v1` shape.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentStructureResponse {
    /// The document's id.
    pub document_id: String,
    /// The caller's own reference for the document, when it has one.
    pub external_ref: Option<String>,
    /// The document's title.
    pub title: String,
    /// The document's pages, by page number.
    pub pages: Vec<PageStructure>,
    /// The document's figures.
    pub figures: Vec<FigureStructure>,
    /// The document's chunks and the pages they sit on.
    pub chunks: Vec<ChunkPageRef>,
    /// True when either read reached its cap: more units may exist than are
    /// listed. False does not prove there are none (an index can always answer
    /// short), it only means the cap was not reached.
    pub truncated: bool,
    /// Outline v2. Additive: the older fields above are unchanged.
    pub outline_version: u32,
    /// A navigable table of contents with chunk and page ranges. Empty for an
    /// empty document.
    pub sections: Vec<SectionDto>,
    /// The document's estimated character count, from chunk spans when all
    /// chunks have them and from the chunks' own text otherwise.
    pub estimated_chars: u32,
    /// True only when every listed chunk carries a complete span.
    pub has_spans: bool,
}

/// One section of the outline v2, mapping to a contiguous chunk range.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SectionDto {
    /// Stable section id within this document (`s1`, `s2`, …), used by the
    /// content route's `?section=` parameter.
    pub id: String,
    /// The heading as printed, or the detected legal marker.
    pub title: String,
    /// 1 for a chapter/top-level heading, deeper for nested markers.
    pub level: u32,
    /// Ancestor titles from the top-level BAB down to this section.
    pub path: Vec<String>,
    /// First page of the section, or null when its chunks have no page.
    pub page_start: Option<u32>,
    /// Last page of the section, or null when its chunks have no page.
    pub page_end: Option<u32>,
    /// First chunk index (inclusive).
    pub chunk_start: u32,
    /// Last chunk index (inclusive).
    pub chunk_end: u32,
    /// Estimated non-overlap characters in the section.
    pub estimated_chars: u32,
    /// What produced the boundary: `bab`, `bagian`, `pasal`, `paragraf`,
    /// `heading` or `page`.
    pub marker: String,
}

impl From<kb_core::outline::Section> for SectionDto {
    fn from(section: kb_core::outline::Section) -> Self {
        Self {
            id: section.id,
            title: section.title,
            level: section.level,
            path: section.path,
            page_start: section.page_start,
            page_end: section.page_end,
            chunk_start: section.chunk_start,
            chunk_end: section.chunk_end,
            estimated_chars: section.estimated_chars,
            marker: section.marker.as_str().to_string(),
        }
    }
}

/// One page. `number` is 0-based, the same numbering as `ChunkMetadata.page`
/// and a search chunk's `page`; the page-image key is built from `number + 1`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PageStructure {
    /// The 0-based page number.
    pub number: u32,
    /// Whether the PDF page had its own text layer. `None` for a document
    /// ingested before page units recorded it, or a page with no page unit.
    pub has_text_layer: Option<bool>,
    /// The page's text when ingest recorded it as OCR output. `None` otherwise,
    /// including for old documents where the origin was not stored.
    pub ocr_text: Option<String>,
    /// The stored page JPEG's blob key, when ingest recorded that it exists.
    pub image_asset_key: Option<String>,
}

/// One figure. `key` is the searchable figure chunk's id when the figure has a
/// chunk, or the asset key for a figure recorded on the document row alone.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FigureStructure {
    /// The figure's chunk id, or its asset key.
    pub key: String,
    /// The 0-based page the figure is on.
    pub page: Option<u32>,
    /// `[x0, y0, x1, y1]` in page points, from `Document.metadata.figures`.
    #[schema(value_type = Option<Vec<f32>>)]
    pub bbox: Option<[f32; 4]>,
    /// The figure's caption, when it has one.
    pub caption: Option<String>,
    /// Text legible in the picture, as the image describer read it (`ING-5`);
    /// null when the picture was not read or shows no text.
    pub ocr_text: Option<String>,
    /// The blob key of the figure's image.
    pub asset_key: Option<String>,
    /// What the picture shows, as the image describer read it (`ING-5`).
    /// Additive; absent for figures from before it existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The picture's kind (`photo`, `chart`, `diagram`, …), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// One text chunk's place in the document.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChunkPageRef {
    /// The chunk's id.
    pub chunk_id: String,
    /// The chunk's 0-based position in the document.
    pub chunk_index: u32,
    /// 0-based page, or null when the chunk has none (a format without pages).
    pub page: Option<u32>,
}

/// A figure row as `Document.metadata.figures` stores it.
#[derive(Debug)]
struct StoredFigure {
    asset_key: String,
    page: Option<u32>,
    bbox: Option<[f32; 4]>,
    caption: Option<String>,
    text: Option<String>,
    description: Option<String>,
    kind: Option<String>,
}

/// The pages, figures and chunk-to-page map of a document.
///
/// `GET /v1/documents/{id}/structure`. Visibility is
/// [`find_visible_document`](crate::routes::find_visible_document): tenant
/// **and** the key's knowledge-base limit, answering the same 404 as an
/// unknown id so a limited key cannot probe for documents outside its bases.
#[utoipa::path(
    get,
    path = "/v1/documents/{id}/structure",
    tag = "structure",
    params(("id" = String, Path, description = "The document's id.")),
    responses(
        (status = 200, description = "The document's pages, figures and chunk map", body = DocumentStructureResponse),
        (status = 401, description = "No key, or a key that does not resolve", body = ErrorBody),
        (status = 403, description = "The key lacks the kb:read scope", body = ErrorBody),
        (status = 404, description = "No such document is visible to this key", body = ErrorBody),
        (status = 500, description = "The store could not be read", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn get_document_structure(
    State(state): State<Arc<AppState>>,
    Authed(auth): Authed,
    Path(id): Path<String>,
) -> Result<Json<DocumentStructureResponse>, ApiError> {
    if !crate::auth::has_scope(&auth, "kb:read") {
        return Err(ApiError::missing_scope("kb:read"));
    }
    // Visibility first: the same tenant + KB-limited-key check as the rest of
    // the route. No chunk is listed before this passes.
    let doc = crate::routes::find_visible_document(&state, &auth, &id).await?;
    let scope = state
        .documents
        .build_scope(&ScopeRequest {
            tenant: auth.tenant_id.clone(),
            filter: DocumentFilter {
                knowledge_base_ids: auth.knowledge_base_ids.clone(),
                ..DocumentFilter::default()
            },
            force_ids: false,
        })
        .await
        .map_err(ApiError::from)?
        .narrowed_to(std::slice::from_ref(&doc.id));
    structure_for(&state, &auth.tenant_id.0, &doc, &scope).await
}

async fn structure_for(
    state: &AppState,
    tenant: &str,
    doc: &DocumentMeta,
    scope: &DocumentScope,
) -> Result<Json<DocumentStructureResponse>, ApiError> {
    let tenant_id = kb_ports::TenantId(tenant.to_string());
    let text_units = state
        .index
        .document_units(&tenant_id, &doc.id, STRUCTURE_MAX_CHUNKS)
        .await
        .map_err(ApiError::from)?;
    // The page image arm owns its own index; the text index never holds page
    // units. Without the arm there are no page units, and pages are still
    // listed from the chunks and figures that a text search can return.
    let page_units = match state.engine.inputs.page_arm.as_ref() {
        Some(arm) => arm
            .index
            .document_units(&tenant_id, &doc.id, STRUCTURE_MAX_PAGES)
            .await
            .map_err(ApiError::from)?,
        None => Vec::new(),
    };
    let truncated =
        text_units.len() >= STRUCTURE_MAX_CHUNKS || page_units.len() >= STRUCTURE_MAX_PAGES;

    let stored_figures = read_stored_figures(doc.metadata.as_ref());
    let figures = collect_figures(&text_units, &stored_figures);
    let chunks: Vec<ChunkPageRef> = text_units
        .iter()
        .map(|unit| ChunkPageRef {
            chunk_id: chunk_id_for_client(&unit.unit_id.0),
            chunk_index: unit.chunk_index,
            page: unit.page,
        })
        .collect();

    let mut pages: BTreeMap<u32, PageStructure> = BTreeMap::new();
    for unit in &page_units {
        let Some(number) = unit.page else {
            continue;
        };
        let has_text_layer = unit
            .metadata
            .get("hasTextLayer")
            .and_then(serde_json::Value::as_bool);
        let is_ocr = unit
            .metadata
            .get("ocr")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let image_exists = unit
            .metadata
            .get("image")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        pages.insert(
            number,
            PageStructure {
                number,
                has_text_layer,
                ocr_text: if is_ocr {
                    page_ocr_text(&unit.content)
                } else {
                    None
                },
                image_asset_key: image_exists
                    .then(|| page_image_path(Some(tenant), &doc.id.0, number.saturating_add(1))),
            },
        );
    }
    // A page a text hit or a figure can point at is part of the document even
    // when the page arm did not index it. It carries only its number: nothing
    // else was stored for it.
    for number in text_units.iter().filter_map(|u| u.page) {
        pages.entry(number).or_insert(PageStructure {
            number,
            has_text_layer: None,
            ocr_text: None,
            image_asset_key: None,
        });
    }
    for figure in &figures {
        if let Some(number) = figure.page {
            pages.entry(number).or_insert(PageStructure {
                number,
                has_text_layer: None,
                ocr_text: None,
                image_asset_key: None,
            });
        }
    }

    // Outline v2 is built from the same scoped chunk read; a document outside
    // the scope can never contribute sections here. The stored outline from
    // the tree arm (RET-6) is the first source, when it can be mapped to chunks.
    let chunk_meta = state
        .index
        .list_chunk_meta(scope, &doc.id)
        .await
        .map_err(ApiError::from)?;
    let outline_chunks: Vec<OutlineChunk> = chunk_meta
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
    let stored_outline = doc
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("outline"))
        .and_then(|value| serde_json::from_value::<kb_core::Outline>(value.clone()).ok());
    let sections = build_outline(&outline_chunks, stored_outline.as_ref())
        .into_iter()
        .map(SectionDto::from)
        .collect();

    Ok(Json(DocumentStructureResponse {
        document_id: doc.id.0.clone(),
        external_ref: doc.external_ref.clone(),
        title: doc.title.clone(),
        pages: pages.into_values().collect(),
        figures,
        chunks,
        truncated,
        outline_version: 2,
        sections,
        estimated_chars: estimated_chars(&outline_chunks),
        has_spans: has_spans(&outline_chunks),
    }))
}

/// The page's stored text without the visual description the caption stage
/// appends to the same string. The page unit does not keep the two parts
/// separately (that would double the payload), and the description is not OCR
/// output, so it must not be returned as one.
fn page_ocr_text(content: &str) -> Option<String> {
    let text = content
        .split("\n\n[Visual] ")
        .next()
        .unwrap_or(content)
        .trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Every figure the text index can return for this document, merged with the
/// document row's figure rows for bbox and caption.
fn collect_figures(
    text_units: &[kb_ports::Candidate],
    stored: &[StoredFigure],
) -> Vec<FigureStructure> {
    let mut figures: Vec<FigureStructure> = Vec::new();
    let mut used_assets: Vec<String> = Vec::new();
    for unit in text_units
        .iter()
        .filter(|u| u.chunk_type == ChunkType::Figure)
    {
        let asset_key = unit
            .metadata
            .get("assetKey")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let merged = asset_key
            .as_deref()
            .and_then(|key| stored.iter().find(|s| s.asset_key == key));
        let caption = unit
            .metadata
            .get("section")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| merged.and_then(|s| s.caption.clone()));
        figures.push(FigureStructure {
            key: chunk_id_for_client(&unit.unit_id.0),
            page: unit.page.or_else(|| merged.and_then(|s| s.page)),
            bbox: merged.and_then(|s| s.bbox),
            caption,
            ocr_text: merged.and_then(|s| s.text.clone()),
            asset_key: asset_key.clone(),
            description: merged.and_then(|s| s.description.clone()),
            kind: merged.and_then(|s| s.kind.clone()),
        });
        if let Some(key) = asset_key {
            used_assets.push(key);
        }
    }
    // A figure recorded on the document row but with no chunk is still a figure
    // of the document (an ingest from before figure chunks, or a chunk-store
    // write that was rolled back). It is listed with its asset key as `key`.
    for stored in stored {
        if used_assets.contains(&stored.asset_key) {
            continue;
        }
        figures.push(FigureStructure {
            key: stored.asset_key.clone(),
            page: stored.page,
            bbox: stored.bbox,
            caption: stored.caption.clone(),
            ocr_text: stored.text.clone(),
            asset_key: Some(stored.asset_key.clone()),
            description: stored.description.clone(),
            kind: stored.kind.clone(),
        });
    }
    figures.sort_by(|a, b| a.page.cmp(&b.page).then_with(|| a.key.cmp(&b.key)));
    figures
}

fn non_empty_str(v: Option<&serde_json::Value>) -> Option<String> {
    v.and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parse `Document.metadata.figures`, keeping bbox and page, which the other
/// figure view (`read_document_figures`) drops. Entries without a non-empty
/// `assetKey` are skipped, exactly as that view skips them.
fn read_stored_figures(metadata: Option<&serde_json::Value>) -> Vec<StoredFigure> {
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
                .filter(|k| !k.is_empty())?
                .to_string();
            let page = f
                .get("page")
                .and_then(serde_json::Value::as_u64)
                .and_then(|p| u32::try_from(p).ok());
            let bbox = f.get("bbox").and_then(read_bbox);
            Some(StoredFigure {
                asset_key,
                page,
                bbox,
                caption: f
                    .get("caption")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                text: non_empty_str(f.get("text")),
                description: non_empty_str(f.get("description")),
                // `type` is the layout kind for old rows and the describer's
                // kind for read pictures; only a read picture has `origin`.
                kind: f.get("origin").and_then(|_| non_empty_str(f.get("type"))),
            })
        })
        .collect()
}

fn read_bbox(value: &serde_json::Value) -> Option<[f32; 4]> {
    let values = value.as_array()?;
    if values.len() != 4 {
        return None;
    }
    let mut bbox = [0.0f32; 4];
    for (i, value) in values.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let number = value.as_f64()? as f32;
        bbox[i] = number;
    }
    Some(bbox)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{collect_figures, page_ocr_text, read_bbox, read_stored_figures};

    #[test]
    fn a_page_without_ocr_text_is_null_not_empty() {
        assert_eq!(page_ocr_text("   "), None);
        assert_eq!(page_ocr_text("\n\n[Visual] a chart"), None);
    }

    #[test]
    fn the_visual_description_is_stripped_from_ocr_text() {
        assert_eq!(
            page_ocr_text("lorem ipsum\n\n[Visual] a chart of sales"),
            Some("lorem ipsum".to_string())
        );
        // OCR text that legitimately contains another bracketed line survives.
        assert_eq!(
            page_ocr_text("[Figure] caption\n\nbody"),
            Some("[Figure] caption\n\nbody".to_string())
        );
    }

    #[test]
    fn bbox_is_four_numbers_or_null() {
        assert_eq!(
            read_bbox(&serde_json::json!([1, 2, 3.5, 4])),
            Some([1.0, 2.0, 3.5, 4.0])
        );
        assert_eq!(read_bbox(&serde_json::json!([1, 2, 3])), None);
        assert_eq!(read_bbox(&serde_json::json!("nope")), None);
    }

    #[test]
    fn stored_figures_keep_bbox_and_skip_entries_without_an_asset() {
        let metadata = serde_json::json!({"figures": [
            {"assetKey": "a.png", "page": 3, "caption": "C", "bbox": [1,2,3,4]},
            {"assetKey": "", "page": 1},
            {"page": 2},
            "not-an-object"
        ]});
        let figures = read_stored_figures(Some(&metadata));
        assert_eq!(figures.len(), 1);
        assert_eq!(figures[0].asset_key, "a.png");
        assert_eq!(figures[0].page, Some(3));
        assert_eq!(figures[0].bbox, Some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(figures[0].caption.as_deref(), Some("C"));
    }

    #[test]
    fn a_read_picture_lists_its_description_kind_and_text_and_an_old_figure_does_not() {
        let metadata = serde_json::json!({"figures": [
            {"assetKey": "p.jpg", "page": 0, "type": "photo", "origin": "upload",
             "description": "A red bicycle.", "text": "STOP"},
            {"assetKey": "old.png", "page": 1, "type": "image", "caption": "Gambar 1"}
        ]});
        let figures = collect_figures(&[], &read_stored_figures(Some(&metadata)));
        assert_eq!(figures.len(), 2);
        assert_eq!(figures[0].description.as_deref(), Some("A red bicycle."));
        assert_eq!(figures[0].kind.as_deref(), Some("photo"));
        assert_eq!(figures[0].ocr_text.as_deref(), Some("STOP"));
        assert_eq!(figures[1].description, None);
        assert_eq!(figures[1].kind, None, "a layout kind is not a reading");
        let json = serde_json::to_value(&figures[1]).unwrap();
        assert!(
            json.get("description").is_none(),
            "absent, not null: {json}"
        );
    }
}
