//! The page-image arm: each page of a PDF is embedded as an image, and a query
//! is matched against those page vectors as well as against the text.
//!
//! - **Ingest** ([`PageIngest`]): render the pages, embed them, and store one
//!   dense-only unit per page in a [`RetrievalIndex`] of its own, with the ids
//!   of the text chunks on that page in its metadata.
//! - **Search** ([`PageArm`]): embed the query, search the page index within the
//!   caller's [`DocumentScope`], and fuse the page ranking with the text ranking
//!   at page level ([`kb_core::page_fusion`]). A page only the image arm found
//!   brings its first text chunk into the results, fetched from the text index
//!   under the same scope.
//!
//! Scope: the page index is searched with the same `DocumentScope` as every
//! other arm, and the chunks a page brings in are read back through
//! `RetrievalIndex::fetch`, which is scoped too. Nothing here can widen it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use kb_ports::{
    Candidate, ChunkId, ChunkType, DocumentId, DocumentScope, Fusion, HybridQuery, IndexUnit,
    PageEmbedder, PageRenderer, PortError, ProvenanceArm, RetrievalIndex, RetrievalProvenance,
    SearchHit, TenantId,
};

use crate::retrieval::candidate_to_hit_from;

/// Search-time settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageArmConfig {
    /// Weight of the image ranking in the fusion (text gets `1 - w`).
    pub image_weight: f32,
    /// RRF constant for the text/image fusion.
    pub rrf_k: u32,
    /// Pages fetched from the page index per query.
    pub candidates: usize,
    /// Budget for query embedding + page search. Past it the search degrades.
    pub timeout: Duration,
}

impl Default for PageArmConfig {
    fn default() -> Self {
        Self {
            image_weight: 0.5,
            rrf_k: 5,
            candidates: 100,
            timeout: Duration::from_secs(10),
        }
    }
}

/// One page the image arm ranked.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedPage {
    pub document_id: DocumentId,
    pub page: u32,
    pub score: f32,
    /// Text chunks on this page, in chunk order.
    pub chunk_ids: Vec<ChunkId>,
}

/// The search side of the arm.
pub struct PageArm {
    pub embedder: Arc<dyn PageEmbedder>,
    pub index: Arc<dyn RetrievalIndex>,
    pub config: PageArmConfig,
}

impl PageArm {
    /// Pages for `query` within `scope`, best first.
    ///
    /// # Errors
    ///
    /// When the query cannot be embedded or the page index cannot be searched.
    pub async fn rank(
        &self,
        scope: &DocumentScope,
        query: &str,
    ) -> Result<Vec<RankedPage>, PortError> {
        if scope.document_ids().is_empty() {
            return Ok(Vec::new());
        }
        let vector = self.embedder.embed_query(query).await?;
        let n = self.config.candidates.max(1);
        let hits = self
            .index
            .search(
                &HybridQuery {
                    dense: Some(vector),
                    sparse: None,
                    per_arm_k: n,
                    k: n,
                    fusion: Fusion::None,
                },
                scope,
            )
            .await?;
        Ok(hits.into_iter().filter_map(ranked_page).collect())
    }
}

fn ranked_page(c: Candidate) -> Option<RankedPage> {
    let page = c.page?;
    let chunk_ids = c
        .metadata
        .get("chunkIds")
        .and_then(|v| v.as_array())
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(|s| ChunkId(s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(RankedPage {
        document_id: c.document_id,
        page,
        score: c.scores.dense.unwrap_or(c.scores.fused),
        chunk_ids,
    })
}

/// Where a hit sits for fusion: its page, or the chunk itself when it has none.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Slot {
    Page(DocumentId, u32),
    Chunk(ChunkId),
}

fn slot_of(hit: &SearchHit) -> Slot {
    match hit.chunk.metadata.page {
        Some(p) => Slot::Page(hit.chunk.document_id.clone(), p),
        None => Slot::Chunk(hit.chunk.id.clone()),
    }
}

/// Fuse the ranked text hits with the image arm's pages.
///
/// Text hits keep their relative order inside a page; a page only the image arm
/// found contributes its first chunk, fetched from `text_index` under `scope`,
/// for pages within the first `limit` fused slots. Returns the merged hits and
/// whether fetching those chunks failed (a partial result, to be reported).
pub async fn merge_pages(
    text_index: &Arc<dyn RetrievalIndex>,
    scope: &DocumentScope,
    text_hits: Vec<SearchHit>,
    pages: &[RankedPage],
    config: &PageArmConfig,
    limit: usize,
) -> (Vec<SearchHit>, bool) {
    if pages.is_empty() {
        return (text_hits, false);
    }
    let mut text_slots: Vec<Slot> = Vec::new();
    let mut by_slot: HashMap<Slot, Vec<SearchHit>> = HashMap::new();
    for hit in text_hits {
        let slot = slot_of(&hit);
        if !by_slot.contains_key(&slot) {
            text_slots.push(slot.clone());
        }
        by_slot.entry(slot).or_default().push(hit);
    }
    let image_slots: Vec<Slot> = pages
        .iter()
        .map(|p| Slot::Page(p.document_id.clone(), p.page))
        .collect();
    let page_of: HashMap<Slot, &RankedPage> = image_slots.iter().cloned().zip(pages).collect();
    let fused = kb_core::page_fusion::fuse_page_rankings(
        &text_slots,
        &image_slots,
        config.image_weight,
        config.rrf_k,
    );

    // First chunk of each image-only page that can still make the cut.
    let wanted: Vec<ChunkId> = fused
        .iter()
        .take(limit.max(1))
        .filter(|s| !by_slot.contains_key(*s))
        .filter_map(|s| page_of.get(s).and_then(|p| p.chunk_ids.first().cloned()))
        .collect();
    let mut fetched: HashMap<ChunkId, SearchHit> = HashMap::new();
    let mut partial = false;
    if !wanted.is_empty() {
        match text_index.fetch(scope, &wanted).await {
            Ok(cands) => {
                for c in cands {
                    let hit = candidate_to_hit_from(c, ProvenanceArm::Page);
                    fetched.insert(hit.chunk.id.clone(), hit);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "page arm: fetching page chunks failed");
                partial = true;
            }
        }
    }

    let mut out: Vec<SearchHit> = Vec::new();
    for slot in fused {
        let image = page_of.get(&slot);
        if let Some(hits) = by_slot.remove(&slot) {
            for mut hit in hits {
                if image.is_some() {
                    hit.retrieval = Some(
                        hit.retrieval
                            .take()
                            .unwrap_or_else(|| RetrievalProvenance::unscored(ProvenanceArm::Page))
                            .with_arm(ProvenanceArm::Page),
                    );
                }
                out.push(hit);
            }
        } else if let Some(page) = image {
            if let Some(mut hit) = page.chunk_ids.first().and_then(|id| fetched.remove(id)) {
                hit.similarity = page.score;
                out.push(hit);
            }
        }
    }
    for (i, hit) in out.iter_mut().enumerate() {
        hit.rank = u32::try_from(i).unwrap_or(u32::MAX);
    }
    (out, partial)
}

/// The ingest side of the arm.
pub struct PageIngest {
    pub renderer: Arc<dyn PageRenderer>,
    pub embedder: Arc<dyn PageEmbedder>,
    pub index: Arc<dyn RetrievalIndex>,
    /// Pages rendered per document, at most.
    pub max_pages: u32,
    /// Characters of each page's text embedded with its image; 0 = image only.
    pub text_chars: usize,
}

/// Page ids are derived, so a re-ingest overwrites rather than duplicates.
#[must_use]
pub fn page_unit_id(document_id: &DocumentId, page: u32) -> ChunkId {
    ChunkId(format!("{}_page_{page}", document_id.0))
}

impl PageIngest {
    /// Render, embed and store every page of one document, replacing what was
    /// stored for it before. `chunks_by_page` maps a 0-based page to the text
    /// chunk ids on it. Returns the number of pages stored (0 for a format
    /// without pages).
    ///
    /// # Errors
    ///
    /// Rendering, embedding or storing failed.
    #[allow(clippy::too_many_arguments)]
    pub async fn index_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        category: Option<String>,
        mime_type: &str,
        bytes: &[u8],
        chunks_by_page: &HashMap<u32, Vec<ChunkId>>,
        page_texts: &HashMap<u32, String>,
    ) -> Result<usize, PortError> {
        self.index.delete_document(tenant, document_id).await?;
        let mut pages = self
            .renderer
            .render(mime_type, bytes, self.max_pages)
            .await?;
        if pages.is_empty() {
            return Ok(0);
        }
        if self.text_chars > 0 {
            for p in &mut pages {
                if let Some(t) = page_texts.get(&p.page) {
                    p.text = t.trim().chars().take(self.text_chars).collect();
                }
            }
        }
        let vectors = self.embedder.embed_pages(&pages).await?;
        if vectors.len() != pages.len() {
            return Err(PortError::Malformed(format!(
                "page embedder returned {} vectors for {} pages",
                vectors.len(),
                pages.len()
            )));
        }
        let units: Vec<IndexUnit> = pages
            .iter()
            .zip(vectors)
            .map(|(p, dense)| {
                let chunk_ids: Vec<&str> = chunks_by_page
                    .get(&p.page)
                    .map(|ids| ids.iter().map(|c| c.0.as_str()).collect())
                    .unwrap_or_default();
                IndexUnit {
                    id: page_unit_id(document_id, p.page),
                    document_id: document_id.clone(),
                    tenant: tenant.clone(),
                    knowledge_base_id: None,
                    category: category.clone(),
                    chunk_index: p.page,
                    page: Some(p.page),
                    chunk_type: ChunkType::Page,
                    content: String::new(),
                    contextual_prefix: None,
                    metadata: serde_json::json!({ "chunkIds": chunk_ids, "page": p.page }),
                    dense,
                    sparse: kb_core::sparse::SparseVector::default(),
                }
            })
            .collect();
        let n = units.len();
        self.index.upsert(units).await?;
        Ok(n)
    }
}
