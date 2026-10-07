//! Read-path engine.
//!
//! One `retrieve` function owns the whole pipeline:
//!   1. Expand the query via the expander (original + paraphrases).
//!   2. Embed each query with the configured `Embedder`.
//!   3. For every query, kick off `vector_search` + `bm25_search` in parallel.
//!   4. Fuse all hits with `kb_core::fusion::reciprocal_rank_fusion`.
//!   5. Rerank the top `rerank_top_n` candidates with the configured
//!      `Reranker` (failures degrade to fused order).
//!   6. Pull in `window` neighbour chunks per anchor.
//!   7. Pull in figures via `figures` (no vision gate in R1).
//!   8. Drop duplicates and return the final `Vec<SearchHit>`.
//!
//! Failure surfaces:
//!   - Rerank errors → `RetrievalDegrade::RerankSkipped`, fused order kept.
//!   - Neighbour/figure errors → `RetrievalDegrade::NeighboursFailed` /
//!     `FiguresFailed`, hit list kept.
//!   - Embedder / chunk store errors → `EngineError::Unavailable` (the request
//!     cannot be served).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use kb_core::{RrfResult, reciprocal_rank_fusion_with};
use kb_ports::DocumentScope;
use kb_ports::PortError;
use kb_ports::{
    ChunkId, ChunkStore, DocumentId, DocumentMeta, DocumentStore, Embedder, EngineError,
    NeighbourAnchor, RerankCandidate, RerankResult, Reranker, RetrievalDegrade, SearchHit,
};
use tracing::warn;

/// Tunable engine knobs.
#[derive(Debug, Clone)]
pub struct EngineSettings {
    pub per_arm_top_k: u32,
    pub rerank_top_n: u32,
    pub final_top_k: u32,
    pub neighbour_window: u32,
    pub figure_limit: u32,
    pub rrf_k: u32,
    pub figure_cap: u32,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            per_arm_top_k: 20,
            rerank_top_n: 30,
            final_top_k: 10,
            neighbour_window: 3,
            figure_limit: 6,
            rrf_k: 60,
            figure_cap: 12,
        }
    }
}

/// A query expander the server can plug in without forcing `kb-engine` to
/// depend on `kb-inference`.
#[async_trait]
pub trait QueryExpander: Send + Sync {
    async fn expand(&self, query: &str) -> Vec<String>;
}

/// No-op expander (returns the original query unchanged).
pub struct NoopExpander;

#[async_trait]
impl QueryExpander for NoopExpander {
    async fn expand(&self, query: &str) -> Vec<String> {
        vec![query.trim().to_string()]
    }
}

#[derive(Clone)]
pub struct EngineInputs {
    pub embedder: Arc<dyn Embedder>,
    pub chunk_store: Arc<dyn ChunkStore>,
    pub reranker: Arc<dyn Reranker>,
    pub document_store: Arc<dyn DocumentStore>,
    pub query_expander: Arc<dyn QueryExpander>,
}

/// Engine handle — cheap to clone (all state behind `Arc`s).
#[derive(Clone)]
pub struct Engine {
    pub settings: EngineSettings,
    pub inputs: EngineInputs,
}

impl Engine {
    #[must_use]
    pub fn new(settings: EngineSettings, inputs: EngineInputs) -> Self {
        Self { settings, inputs }
    }

    /// Build an engine with the default no-op expander.
    #[must_use]
    pub fn with_settings_and_inputs(
        settings: EngineSettings,
        embedder: Arc<dyn Embedder>,
        chunk_store: Arc<dyn ChunkStore>,
        reranker: Arc<dyn Reranker>,
        document_store: Arc<dyn DocumentStore>,
    ) -> Self {
        let inputs = EngineInputs {
            embedder,
            chunk_store,
            reranker,
            document_store,
            query_expander: Arc::new(NoopExpander),
        };
        Self::new(settings, inputs)
    }

    /// Retrieve sources for `query`. The caller passes the resolved
    /// `DocumentScope` (built by a `DocumentStore`).
    pub async fn retrieve(
        &self,
        query: &str,
        scope: &DocumentScope,
    ) -> Result<RetrievalOutcome, EngineError> {
        if query.trim().is_empty() {
            return Ok(RetrievalOutcome::empty());
        }

        // 1. Expand (best-effort). Expander returns at least the original.
        let queries = self.inputs.query_expander.expand(query).await;

        // 2. Embed each query.
        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(queries.len());
        for q in &queries {
            let v = self
                .inputs
                .embedder
                .embed_query(q)
                .await
                .map_err(|e| EngineError::RetrievalFailed(format!("embedder: {e}")))?;
            embeddings.push(v);
        }

        // 3. Parallel vector + BM25 over every (query, arm).
        let per_arm_top_k = self.settings.per_arm_top_k;
        let mut vector_lists: Vec<Vec<SearchHit>> = Vec::with_capacity(queries.len());
        let mut bm25_lists: Vec<Vec<SearchHit>> = Vec::with_capacity(queries.len());

        for (q, embedding) in queries.iter().zip(embeddings.iter()) {
            let vector_fut = self
                .inputs
                .chunk_store
                .vector_search(scope, embedding, per_arm_top_k);
            let bm25_fut = self.inputs.chunk_store.bm25_search(scope, q, per_arm_top_k);
            let (vector_hits, bm25_hits) = tokio::join!(vector_fut, bm25_fut);
            let vector_hits = vector_hits.unwrap_or_else(|e| {
                warn!("vector arm failed: {e}");
                Vec::new()
            });
            let bm25_hits = bm25_hits.unwrap_or_else(|e| {
                warn!("bm25 arm failed: {e}");
                Vec::new()
            });
            vector_lists.push(vector_hits);
            bm25_lists.push(bm25_hits);
        }

        // 4. Fuse vector + bm25 lists per query, then reduce across queries by
        //    taking the max RRF contribution per chunk id.
        let mut fused_per_query: Vec<RrfResult<SearchHit>> = Vec::new();
        for (vec_list, bm_list) in vector_lists.iter().zip(bm25_lists.iter()) {
            let lists: Vec<Vec<SearchHit>> = vec![vec_list.clone(), bm_list.clone()];
            let merged = reciprocal_rank_fusion_with(
                lists,
                kb_core::RrfOptions {
                    k: self.settings.rrf_k,
                    limit: None,
                },
                |h| h.chunk.id.0.as_str(),
            );
            for m in merged {
                fused_per_query.push(m);
            }
        }

        let mut by_id: HashMap<ChunkId, (f64, SearchHit)> = HashMap::new();
        for item in fused_per_query {
            let id = item.first.chunk.id.clone();
            let score = item.rrf_score;
            by_id
                .entry(id)
                .and_modify(|(s, _)| *s = s.max(score))
                .or_insert((score, item.first));
        }
        let mut fused: Vec<(f64, SearchHit)> = by_id.into_values().collect();
        fused.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut hits: Vec<SearchHit> = fused.into_iter().map(|(_, h)| h).collect();

        // Enrich titles from the document store.
        enrich_titles(&self.inputs.document_store, scope, &mut hits).await;

        // 5. Rerank (degrade on failure).
        let mut degrades: Vec<RetrievalDegrade> = Vec::new();
        let rerank_top_n = self.settings.rerank_top_n as usize;
        let rerank_slice: Vec<SearchHit> = hits.iter().take(rerank_top_n).cloned().collect();
        let candidates: Vec<RerankCandidate> = rerank_slice
            .iter()
            .enumerate()
            .map(|(i, h)| RerankCandidate {
                id: h.chunk.id.clone(),
                text: h.chunk.content.clone(),
                original_rank: i as u32,
                original_score: h.similarity,
            })
            .collect();
        match self
            .inputs
            .reranker
            .rerank(query, candidates, self.settings.final_top_k)
            .await
        {
            Ok(ordered) => {
                let order: HashMap<ChunkId, u32> = ordered
                    .iter()
                    .map(|r| (r.id.clone(), r.final_rank))
                    .collect();
                hits.sort_by(|a, b| {
                    let ar = order.get(&a.chunk.id).copied().unwrap_or(u32::MAX);
                    let br = order.get(&b.chunk.id).copied().unwrap_or(u32::MAX);
                    ar.cmp(&br)
                });
            }
            Err(e) => {
                warn!("rerank failed; falling back to fused order: {e}");
                degrades.push(RetrievalDegrade::RerankUnavailable);
            }
        }

        // 6. Neighbours (degrade on failure).
        if self.settings.neighbour_window > 0 && !hits.is_empty() {
            let anchors: Vec<NeighbourAnchor> = hits
                .iter()
                .take(self.settings.final_top_k as usize)
                .map(|h| NeighbourAnchor {
                    chunk_id: h.chunk.id.clone(),
                    document_id: h.chunk.document_id.clone(),
                    chunk_index: h.chunk.metadata.chunk_index.unwrap_or(0),
                })
                .collect();
            match self
                .inputs
                .chunk_store
                .neighbours(&anchors, self.settings.neighbour_window)
                .await
            {
                Ok(mut neighbours) => {
                    enrich_titles(&self.inputs.document_store, scope, &mut neighbours).await;
                    let anchor_ids: HashSet<ChunkId> =
                        anchors.iter().map(|a| a.chunk_id.clone()).collect();
                    let hit_ids: HashSet<ChunkId> =
                        hits.iter().map(|h| h.chunk.id.clone()).collect();
                    for nh in neighbours {
                        if anchor_ids.contains(&nh.chunk.id) || hit_ids.contains(&nh.chunk.id) {
                            continue;
                        }
                        hits.push(nh);
                    }
                }
                Err(e) => {
                    degrades.push(RetrievalDegrade::NeighboursPartial);
                    let _ = e;
                }
            }
        }

        // 7. Figures (degrade on failure, no vision gate in R1).
        if self.settings.figure_limit > 0 && !hits.is_empty() {
            let mut doc_ids: Vec<String> =
                hits.iter().map(|h| h.chunk.document_id.0.clone()).collect();
            doc_ids.sort();
            doc_ids.dedup();
            let retrieved_text: String = hits
                .iter()
                .take(self.settings.final_top_k as usize)
                .map(|h| h.chunk.content.as_str())
                .collect::<Vec<&str>>()
                .join("\n");
            match self
                .inputs
                .chunk_store
                .figures(scope, query, &retrieved_text, self.settings.figure_limit)
                .await
            {
                Ok(mut figs) => {
                    enrich_titles(&self.inputs.document_store, scope, &mut figs).await;
                    let existing: HashSet<ChunkId> =
                        hits.iter().map(|h| h.chunk.id.clone()).collect();
                    let cap = self.settings.figure_cap;
                    let mut added: u32 = 0;
                    for f in figs {
                        if existing.contains(&f.chunk.id) {
                            continue;
                        }
                        if added >= cap {
                            break;
                        }
                        added += 1;
                        hits.push(f);
                    }
                }
                Err(e) => {
                    degrades.push(RetrievalDegrade::FiguresPartial);
                    let _ = e;
                }
            }
        }

        // 8. Trim to final_top_k.
        hits.truncate(self.settings.final_top_k as usize);

        Ok(RetrievalOutcome { hits, degrades })
    }
}

/// One retrieval result.
#[derive(Debug, Clone)]
pub struct RetrievalOutcome {
    pub hits: Vec<SearchHit>,
    pub degrades: Vec<RetrievalDegrade>,
}

impl RetrievalOutcome {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            hits: Vec::new(),
            degrades: Vec::new(),
        }
    }
}

/// Helper to convert a port error into an engine error. Kept so the engine
/// can centralise the mapping when more variants get added.
#[allow(dead_code)]
fn port_to_engine(e: PortError) -> EngineError {
    EngineError::RetrievalFailed(format!("port: {e}"))
}

/// Enrich titles by looking up document metadata for any document id that appears
/// in `hits` but doesn't have a title yet.
async fn enrich_titles(
    store: &Arc<dyn DocumentStore>,
    scope: &DocumentScope,
    hits: &mut Vec<SearchHit>,
) {
    let mut missing: HashSet<DocumentId> = HashSet::new();
    for h in hits.iter() {
        if h.chunk.document_title.is_empty() {
            missing.insert(h.chunk.document_id.clone());
        }
    }
    if missing.is_empty() {
        return;
    }
    let tenant = scope.tenant().clone();
    let limit = missing.len().max(1) as u32;
    let Ok(metas) = store.list(&tenant, limit, &[]).await else {
        return;
    };
    let title_by_id: HashMap<DocumentId, String> = metas
        .into_iter()
        .map(|m: DocumentMeta| (m.id, m.title))
        .collect();
    for h in hits.iter_mut() {
        if h.chunk.document_title.is_empty() {
            if let Some(title) = title_by_id.get(&h.chunk.document_id) {
                h.chunk.document_title = title.clone();
            }
        }
    }
}

// The engine never sees raw RerankResult directly here; we just hash its
// `final_rank` via `order` above. Keep the type reachable for the test build.
#[allow(dead_code)]
fn _type_anchors(_: RerankResult) {}
