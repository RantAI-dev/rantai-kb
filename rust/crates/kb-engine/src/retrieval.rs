//! Retrieval pipeline implementing hybrid search, fusion, reranking,
//! neighbour weave, figure co-retrieval, and context building.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use kb_core::{ContextItem, SourceEntry, build_context_and_sources, weave_neighbours_with};
use kb_ports::{
    ChunkStore, DocumentId, DocumentMeta, DocumentScope, DocumentStore, Embedder, EngineError,
    FigureGate, FigureGateCandidate, NeighbourAnchor, RerankCandidate, Reranker, RetrievalDegrade,
    SearchHit, TenantId,
};

use crate::QueryExpander;

/// Parameters for one retrieval execution.
#[derive(Clone)]
pub struct RetrievalOptions {
    pub max_chunks: u32,
    pub min_similarity: f32,
    pub candidate_pool_size: u32,
    pub rerank_final_k: u32,
    pub neighbour_window: u32,
    pub figure_limit: u32,
    /// Whether to ask the expander for paraphrases.
    pub expansion_enabled: bool,
    /// Paraphrases kept besides the original query.
    pub expansion_paraphrases: u32,
    /// Run the BM25 arm.
    pub hybrid_bm25_enabled: bool,
    /// Optional figure gate veto.
    pub figure_gate: Option<Arc<dyn FigureGate>>,
    /// Whether reranker is enabled.
    pub rerank_enabled: bool,
}

impl fmt::Debug for RetrievalOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetrievalOptions")
            .field("max_chunks", &self.max_chunks)
            .field("min_similarity", &self.min_similarity)
            .field("candidate_pool_size", &self.candidate_pool_size)
            .field("rerank_final_k", &self.rerank_final_k)
            .field("neighbour_window", &self.neighbour_window)
            .field("figure_limit", &self.figure_limit)
            .field("expansion_enabled", &self.expansion_enabled)
            .field("expansion_paraphrases", &self.expansion_paraphrases)
            .field("hybrid_bm25_enabled", &self.hybrid_bm25_enabled)
            .field(
                "figure_gate",
                &self.figure_gate.as_ref().map(|_| "<FigureGate>"),
            )
            .finish()
    }
}

impl Default for RetrievalOptions {
    fn default() -> Self {
        Self {
            max_chunks: 8,
            min_similarity: 0.30,
            candidate_pool_size: 20,
            rerank_final_k: 5,
            neighbour_window: 0,
            figure_limit: 0,
            expansion_enabled: false,
            expansion_paraphrases: 3,
            hybrid_bm25_enabled: true,
            figure_gate: None,
            rerank_enabled: false,
        }
    }
}

/// The outcome of the retrieval pipeline.
#[derive(Debug, Clone)]
pub struct RetrievalOutcome {
    pub chunks: Vec<SearchHit>,
    pub sources: Vec<SourceEntry>,
    pub context: String,
    pub degrades: Vec<RetrievalDegrade>,
}

/// Enrich document titles, categories, and subcategory on hits via `DocumentStore::find_alive_meta_by_ids`.
pub async fn enrich_titles(
    doc_store: &Arc<dyn DocumentStore>,
    tenant: &TenantId,
    hits: &mut [SearchHit],
) {
    let mut missing_ids: Vec<DocumentId> = Vec::new();
    let mut seen: HashSet<DocumentId> = HashSet::new();
    for h in hits.iter() {
        if seen.insert(h.chunk.document_id.clone()) {
            missing_ids.push(h.chunk.document_id.clone());
        }
    }
    if missing_ids.is_empty() {
        return;
    }

    if let Ok(metas) = doc_store.find_alive_meta_by_ids(tenant, &missing_ids).await {
        let meta_map: HashMap<DocumentId, DocumentMeta> =
            metas.into_iter().map(|m| (m.id.clone(), m)).collect();
        for h in hits.iter_mut() {
            if let Some(meta) = meta_map.get(&h.chunk.document_id) {
                h.chunk.document_title = meta.title.clone();
                h.chunk.categories = meta.categories.clone();
                h.chunk.subcategory = meta.subcategory.clone();
            }
        }
    }
}

/// Execute the full retrieval pipeline.
///
/// Every collaborator is passed explicitly rather than behind a context
/// object: the engine receives its stores as arguments, which is the whole
/// point of the port boundary.
// The argument list mirrors the pipeline's stages, so it is kept even though
// the lint would rather it were bundled.
#[allow(clippy::too_many_arguments)]
pub async fn execute_pipeline(
    doc_store: &Arc<dyn DocumentStore>,
    chunk_store: &Arc<dyn ChunkStore>,
    embedder: &Arc<dyn Embedder>,
    reranker: &Arc<dyn Reranker>,
    expander: &Arc<dyn QueryExpander>,
    scope: &DocumentScope,
    query: &str,
    opts: &RetrievalOptions,
) -> Result<RetrievalOutcome, EngineError> {
    let mut degrades: Vec<RetrievalDegrade> = Vec::new();
    let pool_size = opts.candidate_pool_size.clamp(1, 100);

    // 1. Query expansion. The original query is always first. A failed
    //    expander records a degrade and the original query is used alone.
    let variants: Vec<String> = if opts.expansion_enabled {
        match expander.expand(query).await {
            Ok(mut v) => {
                if !v.iter().any(|q| q == query) {
                    v.insert(0, query.to_string());
                }
                v.truncate(1 + opts.expansion_paraphrases as usize);
                v
            }
            Err(_) => {
                degrades.push(RetrievalDegrade::ExpansionUnavailable);
                vec![query.to_string()]
            }
        }
    } else {
        vec![query.to_string()]
    };

    // 2. Vector arm (scoped to document IDs from KB/category if non-empty).
    let vector_future = async {
        if scope.document_ids().is_empty() {
            return Ok(Vec::new());
        }
        let embeds = variants.iter().map(|q| embedder.embed_query(q));
        let vectors = futures::future::join_all(embeds).await;
        // Own the vectors first: the search futures borrow them, so they must
        // outlive the futures.
        let mut query_vectors: Vec<Vec<f32>> = Vec::with_capacity(vectors.len());
        for res in vectors {
            let vec = res.map_err(|e| format!("vector embed: {e}"))?;
            query_vectors.push(vec);
        }
        let searches = query_vectors
            .iter()
            .map(|vec| chunk_store.vector_search(scope, vec, pool_size));
        let lists = futures::future::join_all(searches).await;

        let mut union: HashMap<String, SearchHit> = HashMap::new();
        for list in lists {
            let list = list.map_err(|e| format!("vector search: {e}"))?;
            for hit in list {
                let keep_new = union
                    .get(&hit.chunk.id.0)
                    .is_none_or(|prev| prev.similarity < hit.similarity);
                if keep_new {
                    union.insert(hit.chunk.id.0.clone(), hit);
                }
            }
        }
        let mut merged: Vec<SearchHit> = union.into_values().collect();
        merged.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.chunk.id.0.cmp(&b.chunk.id.0))
        });
        merged.retain(|h| h.similarity >= opts.min_similarity);
        merged.truncate(pool_size as usize);
        for (rank, hit) in merged.iter_mut().enumerate() {
            hit.rank = rank as u32;
            hit.is_bm25 = false;
        }
        Ok::<Vec<SearchHit>, String>(merged)
    };

    // 3. BM25 arm: global in SurrealDB, filtered against alive tenant documents in Postgres,
    //    and scoped to visible document IDs when narrowed.
    let bm25_future = async {
        if !opts.hybrid_bm25_enabled || scope.document_ids().is_empty() {
            return Ok(Vec::new());
        }
        let raw_hits = chunk_store
            .bm25_search_global(query, pool_size)
            .await
            .map_err(|e| format!("bm25 search: {e}"))?;
        if raw_hits.is_empty() {
            return Ok(Vec::new());
        }
        let scope_doc_ids: HashSet<&DocumentId> = scope.document_ids().iter().collect();
        let mut seen = HashSet::new();
        let doc_ids: Vec<DocumentId> = raw_hits
            .iter()
            .map(|h| h.chunk.document_id.clone())
            .filter(|id| scope_doc_ids.contains(id) && seen.insert(id.clone()))
            .collect();
        if doc_ids.is_empty() {
            return Ok(Vec::new());
        }
        let alive_metas = doc_store
            .find_alive_meta_by_ids(scope.tenant(), &doc_ids)
            .await
            .map_err(|e| format!("resolve doc metas: {e}"))?;
        let meta_map: HashMap<DocumentId, DocumentMeta> =
            alive_metas.into_iter().map(|m| (m.id.clone(), m)).collect();

        let mut filtered = Vec::new();
        for mut hit in raw_hits {
            if !scope_doc_ids.contains(&hit.chunk.document_id) {
                continue;
            }
            if let Some(doc) = meta_map.get(&hit.chunk.document_id) {
                hit.similarity = 0.0;
                hit.chunk.document_title = doc.title.clone();
                hit.chunk.categories = doc.categories.clone();
                hit.chunk.subcategory = doc.subcategory.clone();
                hit.chunk.metadata.section = None;
                hit.chunk.metadata.contextual_prefix = None;
                hit.chunk.metadata.asset_key = None;
                hit.chunk.metadata.page = None;
                hit.is_bm25 = true;
                filtered.push(hit);
            }
        }
        for (rank, hit) in filtered.iter_mut().enumerate() {
            hit.rank = rank as u32;
        }
        Ok::<Vec<SearchHit>, String>(filtered)
    };

    let (vector_res, bm25_res) = tokio::join!(vector_future, bm25_future);

    let (mut v_hits, b_hits) = match (vector_res, bm25_res) {
        (Err(v_err), Err(b_err)) => {
            return Err(EngineError::RetrievalFailed(format!(
                "both search arms failed. vector: {v_err}; bm25: {b_err}"
            )));
        }
        (Err(_), Ok(b_hits)) => {
            degrades.push(RetrievalDegrade::VectorArmFailed);
            (Vec::new(), b_hits)
        }
        (Ok(v_hits), Err(_)) => {
            degrades.push(RetrievalDegrade::Bm25ArmFailed);
            (v_hits, Vec::new())
        }
        (Ok(v_hits), Ok(b_hits)) => (v_hits, b_hits),
    };

    // Enrich titles & categories on vector hits
    enrich_titles(doc_store, scope.tenant(), &mut v_hits).await;

    // 4. Fuse without cross-arm deduplication (matching Bun's RecordId behavior).
    // Vector hits and BM25 hits are treated as distinct candidates in RRF.
    let fused_hits: Vec<SearchHit> = if b_hits.is_empty() {
        v_hits
    } else if v_hits.is_empty() {
        b_hits
    } else {
        struct Candidate {
            hit: SearchHit,
            score: f64,
            insert_index: usize,
        }
        let mut candidates: Vec<Candidate> = Vec::with_capacity(v_hits.len() + b_hits.len());
        for (i, hit) in v_hits.into_iter().enumerate() {
            let score = 1.0 / (60.0 + (i as f64) + 1.0);
            candidates.push(Candidate {
                hit,
                score,
                insert_index: 2 * i,
            });
        }
        for (j, hit) in b_hits.into_iter().enumerate() {
            let score = 1.0 / (60.0 + (j as f64) + 1.0);
            candidates.push(Candidate {
                hit,
                score,
                insert_index: 2 * j + 1,
            });
        }
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.insert_index.cmp(&b.insert_index))
        });
        candidates.into_iter().map(|c| c.hit).collect()
    };

    if fused_hits.is_empty() {
        return Ok(RetrievalOutcome {
            chunks: Vec::new(),
            sources: Vec::new(),
            context: String::new(),
            degrades,
        });
    }

    // 5. Optional rerank (only if rerank_enabled AND candidates exceed max_chunks)
    let max_chunks = opts.max_chunks.clamp(1, 50) as usize;
    let mut reranked_hits: Vec<SearchHit> = if opts.rerank_enabled && fused_hits.len() > max_chunks
    {
        let final_k = opts.rerank_final_k.clamp(1, 100);
        let candidates: Vec<RerankCandidate> = fused_hits
            .iter()
            .map(|h| RerankCandidate {
                id: h.chunk.id.clone(),
                text: h.chunk.content.clone(),
                original_rank: h.rank,
                original_score: h.similarity,
            })
            .collect();

        match reranker.rerank(query, candidates, final_k).await {
            Ok(ordered) => {
                let mut hit_map: HashMap<String, Vec<SearchHit>> = HashMap::new();
                for h in fused_hits {
                    hit_map.entry(h.chunk.id.0.clone()).or_default().push(h);
                }
                let mut reordered: Vec<SearchHit> = Vec::new();
                for (rank, item) in ordered.into_iter().enumerate() {
                    if let Some(hits) = hit_map.get_mut(&item.id.0) {
                        if !hits.is_empty() {
                            let mut hit = hits.remove(0);
                            hit.rank = rank as u32;
                            reordered.push(hit);
                        }
                    }
                }
                reordered
            }
            Err(_) => {
                degrades.push(RetrievalDegrade::RerankUnavailable);
                fused_hits
            }
        }
    } else {
        fused_hits
    };

    // 6. Cut to max_chunks (1..=50)
    reranked_hits.truncate(max_chunks);

    // 7. Neighbours (when window > 0)
    if opts.neighbour_window > 0 && !reranked_hits.is_empty() {
        let anchors: Vec<NeighbourAnchor> = reranked_hits
            .iter()
            .filter_map(|c| {
                c.chunk.metadata.chunk_index.map(|idx| NeighbourAnchor {
                    chunk_id: c.chunk.id.clone(),
                    document_id: c.chunk.document_id.clone(),
                    chunk_index: idx,
                })
            })
            .collect();

        if !anchors.is_empty() {
            match chunk_store
                .neighbours(scope, &anchors, opts.neighbour_window)
                .await
            {
                Ok(mut neighbours) => {
                    enrich_titles(doc_store, scope.tenant(), &mut neighbours).await;
                    reranked_hits = weave_neighbours_with(
                        &reranked_hits,
                        &neighbours,
                        opts.neighbour_window,
                        |h| h.chunk.id.0.clone(),
                        |h| h.chunk.document_id.0.clone(),
                        |h| h.chunk.metadata.chunk_index.unwrap_or(0),
                    );
                }
                Err(_) => {
                    degrades.push(RetrievalDegrade::NeighboursPartial);
                }
            }
        }
    }

    // 8. Figures (when figure_limit > 0)
    if opts.figure_limit > 0 && !reranked_hits.is_empty() {
        let mut retrieved_doc_ids: Vec<DocumentId> = Vec::new();
        let mut seen_docs: HashSet<DocumentId> = HashSet::new();
        for c in &reranked_hits {
            if seen_docs.insert(c.chunk.document_id.clone()) {
                retrieved_doc_ids.push(c.chunk.document_id.clone());
            }
        }

        let anchored_chunks: Vec<(DocumentId, u32)> = reranked_hits
            .iter()
            .filter_map(|c| {
                c.chunk
                    .metadata
                    .chunk_index
                    .map(|idx| (c.chunk.document_id.clone(), idx))
            })
            .collect();

        let retrieved_text: String = reranked_hits
            .iter()
            .map(|c| c.chunk.content.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        match chunk_store
            .figures(
                scope,
                query,
                &retrieved_text,
                &retrieved_doc_ids,
                &anchored_chunks,
                opts.figure_limit,
            )
            .await
        {
            Ok(mut figures) => {
                if let Some(gate) = &opts.figure_gate {
                    if !figures.is_empty() {
                        let candidates: Vec<FigureGateCandidate> = figures
                            .iter()
                            .map(|f| FigureGateCandidate {
                                id: f.chunk.id.0.clone(),
                                asset_key: f.chunk.metadata.asset_key.clone().unwrap_or_default(),
                                caption: f.chunk.content.clone(),
                            })
                            .collect();
                        match gate.gate_figures(query, &candidates).await {
                            Ok(kept_ids) => {
                                let kept_set: HashSet<String> = kept_ids.into_iter().collect();
                                figures.retain(|f| kept_set.contains(&f.chunk.id.0));
                            }
                            Err(e) => {
                                tracing::warn!("figure gate failed (non-fatal): {e}");
                            }
                        }
                    }
                }
                enrich_titles(doc_store, scope.tenant(), &mut figures).await;
                reranked_hits.extend(figures);
            }
            Err(_) => {
                degrades.push(RetrievalDegrade::FiguresPartial);
            }
        }
    }

    // 9. Build context string and sources with kb-core
    let context_items: Vec<ContextItem<'_>> = reranked_hits
        .iter()
        .map(|hit| ContextItem {
            document_id: Some(hit.chunk.document_id.0.as_str()),
            document_title: &hit.chunk.document_title,
            section: hit.chunk.metadata.section.as_deref(),
            categories: &hit.chunk.categories,
            asset_key: hit.chunk.metadata.asset_key.as_deref(),
            page: hit.chunk.metadata.page,
            chunk_type: if hit.is_bm25 {
                None
            } else {
                Some(hit.chunk.chunk_type.as_storage())
            },
            content: &hit.chunk.content,
            contextual_prefix: hit.chunk.metadata.contextual_prefix.as_deref(),
        })
        .collect();

    let (context, sources) = build_context_and_sources(&context_items);

    Ok(RetrievalOutcome {
        chunks: reranked_hits,
        sources,
        context,
        degrades,
    })
}
