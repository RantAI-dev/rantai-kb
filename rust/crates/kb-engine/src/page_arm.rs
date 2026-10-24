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
    PageEmbedder, PageRenderer, PortError, ProvenanceArm, RerankCandidate, Reranker,
    RetrievalIndex, RetrievalProvenance, SearchHit, TenantId,
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
    /// Rerank the fused pages on their text, or `None` to keep the fused order.
    pub rerank: Option<PageRerankConfig>,
}

impl Default for PageArmConfig {
    fn default() -> Self {
        Self {
            image_weight: 0.5,
            rrf_k: 5,
            candidates: 100,
            timeout: Duration::from_secs(10),
            rerank: None,
        }
    }
}

/// Page-level reranking: after fusion, the top `depth` pages are scored by the
/// reranker on each page's whole text (not on one chunk), and that order is
/// blended with the fused order by weighted RRF.
///
/// Why pages and not chunks: a page the image arm found has no ranked text
/// chunk at all, so a chunk reranker never sees it. Offline on ViDoRe v3 HR,
/// reranking the fused top 30 pages on their text lifted page nDCG@10 from
/// 0.655 to 0.705 held-out (+0.049 [+0.031, +0.069]) with Nemotron-rerank-8B;
/// 2-fold CV chose weight 0.7 and k = 1 in both folds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageRerankConfig {
    /// Fused pages reranked.
    pub depth: usize,
    /// Weight of the reranker's order in the blend (fused order gets `1 - w`).
    pub weight: f32,
    /// RRF constant of the blend.
    pub rrf_k: u32,
    /// Characters of page text the reranker reads.
    pub chars: usize,
    /// Budget for the rerank call. Past it the fused order stands, degraded.
    pub timeout: Duration,
}

impl Default for PageRerankConfig {
    fn default() -> Self {
        Self {
            depth: 30,
            weight: 0.7,
            rrf_k: 1,
            chars: 3000,
            timeout: Duration::from_secs(20),
        }
    }
}

/// What page-level reranking needs at search time.
pub struct PageRerankContext<'a> {
    /// The page index, read for each page's stored text.
    pub page_index: &'a Arc<dyn RetrievalIndex>,
    pub reranker: &'a Arc<dyn Reranker>,
    pub query: &'a str,
}

/// The result of [`merge_pages`].
#[derive(Debug)]
pub struct MergeOutcome {
    pub hits: Vec<SearchHit>,
    /// Fetching the chunks of image-only pages failed; some pages are missing.
    pub fetch_partial: bool,
    /// Page reranking was asked for and failed or timed out; the fused order stands.
    pub rerank_failed: bool,
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
    rerank: Option<PageRerankContext<'_>>,
) -> MergeOutcome {
    if pages.is_empty() {
        return MergeOutcome {
            hits: text_hits,
            fetch_partial: false,
            rerank_failed: false,
        };
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
    let mut fused = kb_core::page_fusion::fuse_page_rankings(
        &text_slots,
        &image_slots,
        config.image_weight,
        config.rrf_k,
    );
    let mut rerank_failed = false;
    if let (Some(cfg), Some(ctx)) = (config.rerank, rerank) {
        match rerank_slots(&fused, &by_slot, scope, &cfg, &ctx).await {
            Ok(order) => fused = order,
            Err(e) => {
                tracing::warn!(error = %e, "page rerank failed (non-fatal); keeping fused order");
                rerank_failed = true;
            }
        }
    }

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
    MergeOutcome {
        hits: out,
        fetch_partial: partial,
        rerank_failed,
    }
}

/// Rerank the top `cfg.depth` fused slots on their text and blend that order
/// with the fused one. A page's text is the text stored on its page unit; a
/// slot with no stored text falls back to its ranked chunks' text, and a slot
/// with neither is not sent (hosted rerankers reject empty documents, e.g.
/// voyage's 400 "Input cannot contain empty strings") and so ranks last in the
/// reranker's list.
async fn rerank_slots(
    fused: &[Slot],
    by_slot: &HashMap<Slot, Vec<SearchHit>>,
    scope: &DocumentScope,
    cfg: &PageRerankConfig,
    ctx: &PageRerankContext<'_>,
) -> Result<Vec<Slot>, PortError> {
    let depth = cfg.depth.min(fused.len());
    if depth < 2 {
        return Ok(fused.to_vec());
    }
    let head = &fused[..depth];
    let page_ids: Vec<ChunkId> = head
        .iter()
        .filter_map(|s| match s {
            Slot::Page(doc, page) => Some(page_unit_id(doc, *page)),
            Slot::Chunk(_) => None,
        })
        .collect();
    let stored: HashMap<ChunkId, String> = ctx
        .page_index
        .fetch(scope, &page_ids)
        .await?
        .into_iter()
        .map(|c| (c.unit_id, c.content))
        .collect();
    let candidates: Vec<RerankCandidate> = head
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let from_page = match slot {
                Slot::Page(doc, page) => stored
                    .get(&page_unit_id(doc, *page))
                    .filter(|t| !t.trim().is_empty())
                    .cloned(),
                Slot::Chunk(_) => None,
            };
            let text = from_page.unwrap_or_else(|| {
                by_slot
                    .get(slot)
                    .map(|hits| {
                        hits.iter()
                            .map(|h| h.chunk.content.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            });
            if text.trim().is_empty() {
                return None;
            }
            Some(RerankCandidate {
                id: ChunkId(i.to_string()),
                text: text.chars().take(cfg.chars).collect(),
                original_rank: u32::try_from(i).unwrap_or(u32::MAX),
                original_score: 0.0,
            })
        })
        .collect();
    if candidates.len() < 2 {
        return Ok(fused.to_vec());
    }
    let final_k = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
    let ranked = tokio::time::timeout(
        cfg.timeout,
        ctx.reranker.rerank(ctx.query, candidates, final_k),
    )
    .await
    .map_err(|_| PortError::Unavailable("page rerank timed out".into()))??;
    let reranked: Vec<Slot> = ranked
        .iter()
        .filter_map(|r| r.id.0.parse::<usize>().ok())
        .filter_map(|i| head.get(i).cloned())
        .collect();
    // Fused order first so ties break on it, as in `page_rerank.py`; the
    // reranker's order is the weighted "second" list.
    let mut order =
        kb_core::page_fusion::fuse_page_rankings(head, &reranked, cfg.weight, cfg.rrf_k);
    order.extend(fused[depth..].iter().cloned());
    Ok(order)
}

/// Page units per upsert request when indexing a document's pages.
const PAGE_UPSERT_BATCH: usize = 64;

/// Characters of page text stored on a page unit (what a page reranker reads).
pub const PAGE_TEXT_STORED_CHARS: usize = 8000;

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
                    // The page's text, for page-level reranking at search time.
                    content: page_texts
                        .get(&p.page)
                        .map(|t| t.trim().chars().take(PAGE_TEXT_STORED_CHARS).collect())
                        .unwrap_or_default(),
                    contextual_prefix: None,
                    metadata: serde_json::json!({ "chunkIds": chunk_ids, "page": p.page }),
                    dense,
                    sparse: kb_core::sparse::SparseVector::default(),
                }
            })
            .collect();
        let n = units.len();
        // In batches: one request with every page of a long PDF (943 pages ×
        // 1,024 floats) timed out against Qdrant.
        let mut units = units;
        while !units.is_empty() {
            let rest = units.split_off(units.len().min(PAGE_UPSERT_BATCH));
            self.index.upsert(units).await?;
            units = rest;
        }
        Ok(n)
    }
}
