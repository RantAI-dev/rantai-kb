//! Retrieval pipeline implementing hybrid search, fusion, reranking,
//! neighbour weave, figure co-retrieval, and context building.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use kb_core::{
    ContextItem, RrfOptions, SourceEntry, build_context_and_sources, reciprocal_rank_fusion_with,
    weave_neighbours_with,
};
use kb_ports::{
    Candidate, Chunk, ChunkId, ChunkMetadata, DocumentId, DocumentMeta, DocumentScope,
    DocumentStore, Embedder, EngineError, FigureGate, FigureGateCandidate, Fusion, HybridQuery,
    KeywordQuery, PortError, ProvenanceArm, RerankCandidate, Reranker, RetrievalDegrade,
    RetrievalIndex, RetrievalProvenance, SearchHit, TenantId,
};
use tracing::Instrument;

use crate::QueryExpander;
use crate::graph_arm::{GraphArm, GraphRanking};
use crate::timing::{Clock, StageTimings, Stopwatch, stage_span};

/// Parameters for one retrieval execution.
#[derive(Clone)]
pub struct RetrievalOptions {
    pub max_chunks: u32,
    pub min_similarity: f32,
    pub candidate_pool_size: u32,
    pub rerank_candidate_limit: u32,
    pub rerank_char_budget: usize,
    pub rerank_timeout_ms: u64,
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
    /// The entity-graph arm, when `KB_GRAPH_ARM` is on. `None` leaves the
    /// pipeline byte-identical to the two-arm version it has always been.
    pub graph_arm: Option<std::sync::Arc<GraphArm>>,
}

impl fmt::Debug for RetrievalOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetrievalOptions")
            .field("max_chunks", &self.max_chunks)
            .field("min_similarity", &self.min_similarity)
            .field("candidate_pool_size", &self.candidate_pool_size)
            .field("rerank_candidate_limit", &self.rerank_candidate_limit)
            .field("rerank_char_budget", &self.rerank_char_budget)
            .field("rerank_timeout_ms", &self.rerank_timeout_ms)
            .field("rerank_final_k", &self.rerank_final_k)
            .field("neighbour_window", &self.neighbour_window)
            .field("figure_limit", &self.figure_limit)
            .field("expansion_enabled", &self.expansion_enabled)
            .field("expansion_paraphrases", &self.expansion_paraphrases)
            .field("hybrid_bm25_enabled", &self.hybrid_bm25_enabled)
            .field("graph_arm", &self.graph_arm.as_ref().map(|_| "<GraphArm>"))
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
            rerank_candidate_limit: 0,
            rerank_char_budget: 0,
            rerank_timeout_ms: 20_000,
            rerank_final_k: 5,
            neighbour_window: 0,
            figure_limit: 0,
            expansion_enabled: false,
            expansion_paraphrases: 3,
            hybrid_bm25_enabled: true,
            figure_gate: None,
            rerank_enabled: false,
            graph_arm: None,
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
    /// What each stage cost. Always present: a request nobody times is a
    /// request nobody can make faster, and the numbers are the only evidence
    /// the move off SurrealDB can be judged by.
    pub timings: StageTimings,
}

const FIG_CAPTION_STOPWORDS: &[&str] = &[
    "yang", "untuk", "pada", "dan", "dengan", "dari", "atau", "adalah", "dalam", "serta", "oleh",
    "hal", "ini", "itu", "para", "kepada", "gambar", "tabel", "raja", "dewa", "kitab", "hari",
    "suci",
];

const FIG_LABELS: &[&str] = &[
    "gambar",
    "tabel",
    "grafik",
    "diagram",
    "foto",
    "bagan",
    "ilustrasi",
    "peta",
];

pub fn fig_caption_meaningful(caption: &str) -> bool {
    let lower = caption.to_lowercase();
    let trimmed = lower.trim_start();
    FIG_LABELS.iter().any(|label| {
        if let Some(rest) = trimmed.strip_prefix(label) {
            rest.chars()
                .next()
                .map(|c| c.is_whitespace() || c == '.' || c == ':')
                .unwrap_or(true)
        } else {
            false
        }
    })
}

pub fn fig_caption_keywords(caption: &str) -> Vec<String> {
    let lower = caption.to_lowercase();
    let mut body = caption.trim_start();

    // Strip label and optional number/dot
    for label in FIG_LABELS {
        if let Some(pos) = lower.find(label) {
            let mut end = pos + label.len();
            let bytes = caption.as_bytes();
            while end < bytes.len() {
                let b = bytes[end];
                if b.is_ascii_whitespace() || b.is_ascii_digit() || b == b'.' || b == b':' {
                    end += 1;
                } else {
                    break;
                }
            }
            body = &caption[end..];
            break;
        }
    }

    body.split(|c: char| !c.is_alphabetic())
        .map(str::to_lowercase)
        .filter(|w| w.len() >= 4 && !FIG_CAPTION_STOPWORDS.contains(&w.as_str()))
        .collect()
}

pub fn candidate_to_hit(c: Candidate) -> SearchHit {
    let retrieval = RetrievalProvenance::of(&c);
    candidate_to_hit_with(c, retrieval)
}

/// [`candidate_to_hit`] for a phase the engine itself ran, which contributes an
/// arm of its own and has no fused score of its own.
///
/// Used for the neighbour weave and the figure co-retrieval: both read
/// candidates the arms never ranked, so `Candidate`'s scores are `0.0` defaults
/// and saying `fusedScore: 0` would read like a score of zero.
pub fn candidate_to_hit_from(c: Candidate, arm: ProvenanceArm) -> SearchHit {
    candidate_to_hit_with(c, Some(RetrievalProvenance::unscored(arm)))
}

fn candidate_to_hit_with(c: Candidate, retrieval: Option<RetrievalProvenance>) -> SearchHit {
    let section = c
        .metadata
        .get("section")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let asset_key = c
        .metadata
        .get("assetKey")
        .or_else(|| c.metadata.get("asset_key"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let anchor_chunk_index = c
        .metadata
        .get("anchorChunkIndex")
        .or_else(|| c.metadata.get("anchor_chunk_index"))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let chunk_index = Some(c.chunk_index);
    let page = c.page.or_else(|| {
        c.metadata
            .get("page")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
    });
    let is_bm25 = c.scores.dense.is_none() && c.scores.sparse.is_some();
    let similarity = if is_bm25 {
        0.0
    } else {
        c.scores.dense.unwrap_or(c.scores.fused)
    };
    SearchHit {
        chunk: Chunk {
            id: c.unit_id,
            document_id: c.document_id,
            document_title: String::new(),
            content: c.content,
            chunk_type: c.chunk_type,
            metadata: ChunkMetadata {
                page,
                section,
                chunk_index,
                asset_key,
                anchor_chunk_index,
                contextual_prefix: c.contextual_prefix,
            },
            categories: Vec::new(),
            subcategory: None,
        },
        similarity,
        rank: c.ranks.fused,
        is_bm25,
        retrieval,
    }
}

pub fn select_figures(
    candidates: Vec<Candidate>,
    query: &str,
    retrieved_text: &str,
    anchored_chunks: &[(DocumentId, u32)],
    limit: u32,
) -> Vec<Candidate> {
    let original_candidates = candidates.clone();
    if limit == 0 || candidates.is_empty() {
        return Vec::new();
    }
    let anchor_set: HashSet<(String, u32)> = anchored_chunks
        .iter()
        .map(|(d, idx)| (d.0.clone(), *idx))
        .collect();

    let q_lower = query.to_lowercase();
    let text_lower = retrieved_text.to_lowercase();

    let mut scored: Vec<(i32, Candidate)> = Vec::new();
    let mut seen_caption: HashSet<String> = HashSet::new();

    for c in candidates {
        let asset_key = c
            .metadata
            .get("assetKey")
            .or_else(|| c.metadata.get("asset_key"))
            .and_then(|v| v.as_str());
        let Some(asset_key) = asset_key else {
            continue;
        };
        if asset_key.trim().is_empty() {
            continue;
        }

        let caption = c
            .metadata
            .get("section")
            .and_then(|v| v.as_str())
            .unwrap_or(&c.content)
            .trim();
        let cap_key = caption.to_lowercase();

        let anchor_chunk_idx = c
            .metadata
            .get("anchorChunkIndex")
            .or_else(|| c.metadata.get("anchor_chunk_index"))
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let anchored = anchor_chunk_idx
            .map(|idx| anchor_set.contains(&(c.document_id.0.clone(), idx)))
            .unwrap_or(false);

        if !seen_caption.insert(cap_key) {
            continue;
        }

        let mut score = 0;
        if anchored {
            score = 3;
        } else {
            if !fig_caption_meaningful(caption) {
                continue;
            }
            let keywords = fig_caption_keywords(caption);
            let any_in_query = keywords.iter().any(|k| q_lower.contains(k.as_str()));
            let any_in_text = keywords.iter().any(|k| text_lower.contains(k.as_str()));
            if any_in_query {
                score = 2;
            } else if any_in_text {
                score = 1;
            }
        }

        if score > 0 {
            scored.push((score, c));
        }
    }

    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.document_id.0.cmp(&b.1.document_id.0))
            .then_with(|| a.1.chunk_index.cmp(&b.1.chunk_index))
    });

    if scored.is_empty() {
        let mut out = original_candidates;
        out.truncate(limit as usize);
        return out;
    }

    scored.truncate(limit as usize);
    scored.into_iter().map(|(_, c)| c).collect()
}

/// Enrich document titles, categories, and subcategory on hits via `DocumentStore::find_alive_meta_by_ids`.
pub async fn enrich_titles(
    doc_store: &Arc<dyn DocumentStore>,
    tenant: &TenantId,
    hits: &mut [SearchHit],
) {
    let mut cache = HashMap::new();
    enrich_titles_cached(doc_store, tenant, hits, &mut cache).await;
}

pub async fn enrich_titles_cached(
    doc_store: &Arc<dyn DocumentStore>,
    tenant: &TenantId,
    hits: &mut [SearchHit],
    cache: &mut HashMap<DocumentId, DocumentMeta>,
) {
    let mut missing_ids: Vec<DocumentId> = Vec::new();
    let mut seen: HashSet<DocumentId> = HashSet::new();
    for h in hits.iter() {
        if !cache.contains_key(&h.chunk.document_id) && seen.insert(h.chunk.document_id.clone()) {
            missing_ids.push(h.chunk.document_id.clone());
        }
    }
    if !missing_ids.is_empty() {
        if let Ok(metas) = doc_store.find_alive_meta_by_ids(tenant, &missing_ids).await {
            for m in metas {
                cache.insert(m.id.clone(), m);
            }
        }
    }
    for h in hits.iter_mut() {
        if let Some(meta) = cache.get(&h.chunk.document_id) {
            h.chunk.document_title = meta.title.clone();
            h.chunk.categories = meta.categories.clone();
            h.chunk.subcategory = meta.subcategory.clone();
        }
    }
}

/// Fetch the graph arm's ranked chunks and re-fuse all three arms together.
///
/// **The fusion is the same RRF the other arms use**, at the same `k`. That is
/// the point of reusing it: a new scoring function would need its own calibration
/// and its own benchmark, and there is no evidence either exists. The three input
/// lists are the dense arm's candidates, the sparse arm's candidates, and the
/// graph arm's — split back out of the adapter's own fused list by their
/// per-arm ranks, which is the only record of which arm produced what.
///
/// The `k` here is the engine-level constant (60, the RRF paper's), **not** the
/// store-level one (`kb_core::RRF_K = 2`, Qdrant's). The two agreed
/// until the store's fusion was measured (`docs/kf-vplane-eval.md`); they no
/// longer do, and that is deliberate and unfinished: the store's `k = 2` was
/// measured on `miracl-id-200` and this arm's `k = 60` was not, so the graph
/// arm keeps what it had until it is measured on its own. The graph arm is off
/// by default (`KB_GRAPH_ARM`), so the default path does not depend on it.
///
/// Returns the fused candidates and, alongside them, each graph-ranked chunk's
/// position in the graph arm's own list. The position is provenance; the fused
/// score is not comparable to it.
///
/// `index.fetch` is scoped, so a chunk in another tenant's document, or one the
/// caller cannot see, comes back as nothing rather than as a hit. That is the
/// scope rule holding at the only place the arm touches storage.
async fn fuse_graph(
    index: &Arc<dyn RetrievalIndex>,
    scope: &DocumentScope,
    graph_chunks: &[ChunkId],
    candidates: &[Candidate],
    pool_size: u32,
) -> Result<(Vec<Candidate>, HashMap<ChunkId, u32>), PortError> {
    let fetched = index.fetch(scope, graph_chunks).await?;

    // The graph list is the arm's own ranking; the fetched candidates come back
    // in whatever order the adapter chose, so they are re-sorted back onto it.
    let position: HashMap<&ChunkId, u32> = graph_chunks
        .iter()
        .enumerate()
        .map(|(rank, id)| (id, rank as u32))
        .collect();
    let mut graph_list: Vec<Candidate> = fetched
        .into_iter()
        .filter(|c| position.contains_key(&c.unit_id))
        .collect();
    graph_list.sort_by_key(|c| position[&c.unit_id]);
    let ranks: HashMap<ChunkId, u32> = graph_list
        .iter()
        .map(|c| (c.unit_id.clone(), position[&c.unit_id]))
        .collect();

    let dense_list: Vec<Candidate> = candidates
        .iter()
        .filter(|c| c.ranks.dense.is_some())
        .cloned()
        .collect();
    let sparse_list: Vec<Candidate> = candidates
        .iter()
        .filter(|c| c.ranks.sparse.is_some())
        .cloned()
        .collect();
    let unranked: Vec<Candidate> = candidates
        .iter()
        .filter(|c| c.ranks.dense.is_none() && c.ranks.sparse.is_none())
        .cloned()
        .collect();

    let fused = reciprocal_rank_fusion_with(
        [dense_list, sparse_list, graph_list],
        RrfOptions {
            k: 60,
            limit: Some(pool_size),
        },
        |c: &Candidate| c.unit_id.0.as_str(),
    );

    let mut out: Vec<Candidate> = Vec::with_capacity(fused.len() + unranked.len());
    for (position, item) in fused.into_iter().enumerate() {
        let mut candidate = item.first;
        candidate.scores.fused = item.rrf_score as f32;
        candidate.ranks.fused = position as u32;
        out.push(candidate);
    }
    // Candidates no arm ranked (an adapter that reported nothing) would otherwise
    // be dropped by the fusion, which is a silent loss of a chunk the arms did
    // return. They go last, in their original order.
    for mut candidate in unranked {
        candidate.ranks.fused = out.len() as u32;
        out.push(candidate);
    }
    Ok((out, ranks))
}

/// Execute the full retrieval pipeline.
///
/// Every collaborator is passed explicitly rather than behind a context
/// object: the engine receives its stores as arguments, which is the whole
/// point of the port boundary.
#[allow(clippy::too_many_arguments)]
pub async fn execute_pipeline(
    doc_store: &Arc<dyn DocumentStore>,
    index: &Arc<dyn RetrievalIndex>,
    embedder: &Arc<dyn Embedder>,
    reranker: &Arc<dyn Reranker>,
    expander: &Arc<dyn QueryExpander>,
    scope: &DocumentScope,
    query: &str,
    opts: &RetrievalOptions,
    clock: &dyn Clock,
) -> Result<RetrievalOutcome, EngineError> {
    let total_sw = Stopwatch::start(clock);
    let mut timings = StageTimings::default();
    let mut degrades: Vec<RetrievalDegrade> = Vec::new();
    let pool_size = opts.candidate_pool_size.clamp(1, 100);

    // 1. Query expansion.
    let expand_sw = Stopwatch::start(clock);
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
    timings.expand_ms = expand_sw.elapsed_ms();

    // 2. Query embedding & sparse encoding
    let embed_sw = Stopwatch::start(clock);
    let embeds = variants.iter().map(|q| embedder.embed_query_detailed(q));
    let vectors = futures::future::join_all(embeds).await;
    timings.query_embed_ms = embed_sw.elapsed_ms();

    let mut query_vectors: Vec<Vec<f32>> = Vec::with_capacity(vectors.len());
    let mut cached: Option<bool> = None;
    let mut embed_failed = false;
    for res in vectors {
        match res {
            Ok(qe) => {
                cached = Some(cached.map_or(qe.cached, |seen| seen && qe.cached));
                query_vectors.push(qe.vector);
            }
            Err(e) => {
                tracing::warn!(error = %e, "query embed failed");
                embed_failed = true;
            }
        }
    }
    timings.query_embed_cached = cached.unwrap_or(false);
    if embed_failed || query_vectors.is_empty() {
        degrades.push(RetrievalDegrade::VectorArmFailed);
    }

    let sparse_query = if opts.hybrid_bm25_enabled {
        let sq = KeywordQuery::from_text(query);
        if sq.is_empty() { None } else { Some(sq) }
    } else {
        None
    };

    if query_vectors.is_empty() && sparse_query.is_none() {
        return Err(EngineError::RetrievalFailed(
            "both search arms failed".to_string(),
        ));
    }

    let search_sw = Stopwatch::start(clock);
    let primary_dense = query_vectors.first().cloned();

    let candidates_res = if let (Some(dense), Some(sparse)) = (&primary_dense, &sparse_query) {
        let hybrid = HybridQuery {
            dense: Some(dense.clone()),
            sparse: Some(sparse.clone()),
            per_arm_k: pool_size as usize,
            k: pool_size as usize,
            fusion: Fusion::Rrf,
        };
        match index.search(&hybrid, scope).await {
            Ok(c) => Ok(c),
            Err(err) => {
                let dense_only = HybridQuery {
                    dense: Some(dense.clone()),
                    sparse: None,
                    per_arm_k: pool_size as usize,
                    k: pool_size as usize,
                    fusion: Fusion::None,
                };
                let sparse_only = HybridQuery {
                    dense: None,
                    sparse: Some(sparse.clone()),
                    per_arm_k: pool_size as usize,
                    k: pool_size as usize,
                    fusion: Fusion::None,
                };
                match (
                    index.search(&dense_only, scope).await,
                    index.search(&sparse_only, scope).await,
                ) {
                    (Ok(d_hits), Err(s_err)) => {
                        tracing::warn!(error = %s_err, "keyword arm failed in index");
                        degrades.push(RetrievalDegrade::Bm25ArmFailed);
                        Ok(d_hits)
                    }
                    (Err(d_err), Ok(s_hits)) => {
                        tracing::warn!(error = %d_err, "vector arm failed in index");
                        degrades.push(RetrievalDegrade::VectorArmFailed);
                        Ok(s_hits)
                    }
                    (Ok(_d_hits), Ok(_)) => Err(err),
                    (Err(_), Err(_)) => Err(err),
                }
            }
        }
    } else if let Some(dense) = &primary_dense {
        let dense_only = HybridQuery {
            dense: Some(dense.clone()),
            sparse: None,
            per_arm_k: pool_size as usize,
            k: pool_size as usize,
            fusion: Fusion::None,
        };
        match index.search(&dense_only, scope).await {
            Ok(c) => Ok(c),
            Err(e) => {
                degrades.push(RetrievalDegrade::VectorArmFailed);
                Err(e)
            }
        }
    } else if let Some(sparse) = &sparse_query {
        let sparse_only = HybridQuery {
            dense: None,
            sparse: Some(sparse.clone()),
            per_arm_k: pool_size as usize,
            k: pool_size as usize,
            fusion: Fusion::None,
        };
        match index.search(&sparse_only, scope).await {
            Ok(c) => Ok(c),
            Err(e) => {
                degrades.push(RetrievalDegrade::Bm25ArmFailed);
                Err(e)
            }
        }
    } else {
        Ok(Vec::new())
    };

    let store_time = search_sw.elapsed_ms();
    timings.arms_ms = store_time;
    timings.vector_arm_ms = store_time;
    timings.keyword_arm_ms = store_time;

    let mut candidates = match candidates_res {
        Ok(c) => c,
        Err(e) => {
            return Err(EngineError::RetrievalFailed(format!(
                "retrieval index search failed: {e}"
            )));
        }
    };

    let scope_sw = Stopwatch::start(clock);
    let alive_check_ids: Vec<DocumentId> =
        candidates.iter().map(|c| c.document_id.clone()).collect();
    let mut meta_cache: HashMap<DocumentId, DocumentMeta> = HashMap::new();
    if !alive_check_ids.is_empty() {
        if let Ok(metas) = doc_store
            .find_alive_meta_by_ids(scope.tenant(), &alive_check_ids)
            .await
        {
            for m in metas {
                meta_cache.insert(m.id.clone(), m);
            }
        }
    }
    timings.scope_check_ms = scope_sw.elapsed_ms();

    let fusion_sw = Stopwatch::start(clock);
    timings.fusion_ms = fusion_sw.elapsed_ms();

    // If expansion had additional variants, run dense search for them and union
    if query_vectors.len() > 1 {
        let mut cand_map: HashMap<ChunkId, Candidate> = candidates
            .into_iter()
            .map(|c| (c.unit_id.clone(), c))
            .collect();
        for dense_vec in query_vectors.into_iter().skip(1) {
            let var_query = HybridQuery {
                dense: Some(dense_vec),
                sparse: None,
                per_arm_k: pool_size as usize,
                k: pool_size as usize,
                fusion: Fusion::None,
            };
            if let Ok(var_hits) = index.search(&var_query, scope).await {
                for hit in var_hits {
                    cand_map
                        .entry(hit.unit_id.clone())
                        .and_modify(|prev| {
                            if hit.scores.fused > prev.scores.fused {
                                *prev = hit.clone();
                            }
                        })
                        .or_insert(hit);
                }
            }
        }
        candidates = cand_map.into_values().collect();
        candidates.sort_by(|a, b| {
            b.scores
                .fused
                .partial_cmp(&a.scores.fused)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.unit_id.0.cmp(&b.unit_id.0))
        });
        candidates.truncate(pool_size as usize);
    }

    // Min similarity filtering applies to hits that matched only on the dense arm.
    // Pure BM25 hits or hits matching BM25 are preserved.
    candidates.retain(|c| {
        c.scores
            .dense
            .is_none_or(|sim| sim >= opts.min_similarity || c.scores.sparse.is_some())
    });

    // 4b. The graph arm, fused as one more ranked list.
    let graph_sw = Stopwatch::start(clock);
    let graph_ranks = match &opts.graph_arm {
        None => HashMap::new(),
        Some(arm) => {
            let ranking = match arm.rank(scope, query, clock).await {
                Ok(ranking) => ranking,
                Err(e) => {
                    // A failed arm is a degraded search, not a failed one. The
                    // other arms' candidates are already in `candidates` and go
                    // out unchanged; what must not happen is silence, because a
                    // graph arm that quietly stopped running looks exactly like a
                    // graph arm that has stopped helping (AGENTS rule 6).
                    tracing::warn!(error = %e, "graph arm failed (non-fatal)");
                    degrades.push(RetrievalDegrade::GraphArmUnavailable);
                    GraphRanking::default()
                }
            };
            if ranking.chunks.is_empty() {
                // No entity in this scope matches the query. That is the graph
                // having nothing to say, not a failure, so no degrade.
                tracing::debug!(
                    seeds = ranking.seeds,
                    unmatched_terms = ranking.unmatched_terms,
                    "graph arm had nothing to say",
                );
                HashMap::new()
            } else {
                match fuse_graph(index, scope, &ranking.chunks, &candidates, pool_size).await {
                    Ok((fused, ranks)) => {
                        candidates = fused;
                        ranks
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "graph arm fetch failed (non-fatal)");
                        degrades.push(RetrievalDegrade::GraphArmUnavailable);
                        HashMap::new()
                    }
                }
            }
        }
    };
    timings.graph_arm_ms = graph_sw.elapsed_ms();

    let mut hits: Vec<SearchHit> = candidates.into_iter().map(candidate_to_hit).collect();

    // The graph arm's rank is provenance, not a score: the fused score a graph
    // chunk carries is an RRF sum like everyone else's, and comparing it to
    // anything would be meaningless.
    for hit in &mut hits {
        if let Some(rank) = graph_ranks.get(&hit.chunk.id) {
            hit.retrieval = Some(
                hit.retrieval
                    .clone()
                    .unwrap_or_else(|| RetrievalProvenance::unscored(ProvenanceArm::Graph))
                    .with_arm(ProvenanceArm::Graph)
                    .with_graph_rank(*rank),
            );
        }
    }

    // Enrich titles & categories
    let enrich_sw = Stopwatch::start(clock);
    enrich_titles_cached(doc_store, scope.tenant(), &mut hits, &mut meta_cache).await;
    timings.enrich_ms = enrich_sw.elapsed_ms();

    // 5. Optional rerank (only if rerank_enabled AND candidates exceed max_chunks)
    let max_chunks = opts.max_chunks.clamp(1, 50) as usize;
    let rerank_sw = Stopwatch::start(clock);
    let rerank_future = async {
        if opts.rerank_enabled && hits.len() > max_chunks {
            let final_k = opts.rerank_final_k.clamp(1, 100);
            // 0 means no limit: every candidate goes to the reranker, as before L.4.
            let candidate_limit = match opts.rerank_candidate_limit {
                0 => hits.len(),
                n => (n as usize).clamp(1, hits.len()),
            };
            let (to_rerank, tail) = if hits.len() > candidate_limit {
                let tail = hits.split_off(candidate_limit);
                (hits, tail)
            } else {
                (hits, Vec::new())
            };
            let candidates: Vec<RerankCandidate> = to_rerank
                .iter()
                .map(|h| {
                    let text = if opts.rerank_char_budget > 0
                        && h.chunk.content.len() > opts.rerank_char_budget
                    {
                        match h
                            .chunk
                            .content
                            .char_indices()
                            .take_while(|&(idx, _)| idx <= opts.rerank_char_budget)
                            .last()
                        {
                            Some((idx, ch)) => &h.chunk.content[..idx + ch.len_utf8()],
                            None => &h.chunk.content,
                        }
                    } else {
                        &h.chunk.content
                    };
                    RerankCandidate {
                        id: h.chunk.id.clone(),
                        text: text.to_string(),
                        original_rank: h.rank,
                        original_score: h.similarity,
                    }
                })
                .collect();

            let rerank_timeout = std::time::Duration::from_millis(opts.rerank_timeout_ms.max(100));
            match tokio::time::timeout(rerank_timeout, reranker.rerank(query, candidates, final_k))
                .await
            {
                Ok(Ok(ordered)) => {
                    // Remember the fused order so hits the reranker did not return are
                    // appended deterministically, not in HashMap iteration order.
                    let fused_order: Vec<String> =
                        to_rerank.iter().map(|h| h.chunk.id.0.clone()).collect();
                    let mut hit_map: HashMap<String, Vec<SearchHit>> = HashMap::new();
                    for h in to_rerank {
                        hit_map.entry(h.chunk.id.0.clone()).or_default().push(h);
                    }
                    let mut reordered: Vec<SearchHit> = Vec::new();
                    for (rank, item) in ordered.into_iter().enumerate() {
                        if let Some(hits_for_id) = hit_map.get_mut(&item.id.0) {
                            if !hits_for_id.is_empty() {
                                let mut hit = hits_for_id.remove(0);
                                hit.rank = rank as u32;
                                reordered.push(hit);
                            }
                        }
                    }
                    for id in fused_order {
                        if let Some(remaining_hits) = hit_map.get_mut(&id) {
                            for mut hit in remaining_hits.drain(..) {
                                hit.rank = reordered.len() as u32;
                                reordered.push(hit);
                            }
                        }
                    }
                    for mut hit in tail {
                        hit.rank = reordered.len() as u32;
                        reordered.push(hit);
                    }
                    reordered
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "rerank error; falling back to fused order");
                    degrades.push(RetrievalDegrade::RerankUnavailable);
                    let mut hits = to_rerank;
                    hits.extend(tail);
                    hits
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_ms = opts.rerank_timeout_ms,
                        "rerank timed out; falling back to fused order"
                    );
                    degrades.push(RetrievalDegrade::RerankUnavailable);
                    let mut hits = to_rerank;
                    hits.extend(tail);
                    hits
                }
            }
        } else {
            hits
        }
    };

    let mut reranked_hits: Vec<SearchHit> = rerank_future
        .instrument(stage_span("rerank", "RERANKER"))
        .await;
    timings.rerank_ms = rerank_sw.elapsed_ms();

    // 6. Cut to max_chunks (1..=50)
    reranked_hits.truncate(max_chunks);

    // 7. Neighbours (when window > 0)
    let neighbours_sw = Stopwatch::start(clock);
    if opts.neighbour_window > 0 && !reranked_hits.is_empty() {
        let mut by_doc: HashMap<DocumentId, Vec<u32>> = HashMap::new();
        for h in &reranked_hits {
            if let Some(idx) = h.chunk.metadata.chunk_index {
                by_doc
                    .entry(h.chunk.document_id.clone())
                    .or_default()
                    .push(idx);
            }
        }
        let mut all_neighbours: Vec<SearchHit> = Vec::new();
        let mut neighbour_failed = false;

        let neighbour_futures = by_doc
            .iter()
            .map(|(doc_id, indices)| index.neighbours(scope, doc_id, indices));
        let neighbour_results = futures::future::join_all(neighbour_futures).await;
        for res in neighbour_results {
            match res {
                Ok(candidates) => {
                    for c in candidates {
                        all_neighbours.push(candidate_to_hit_from(c, ProvenanceArm::Neighbour));
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "neighbours fetch failed");
                    neighbour_failed = true;
                }
            }
        }
        if neighbour_failed {
            degrades.push(RetrievalDegrade::NeighboursPartial);
        }
        if !all_neighbours.is_empty() {
            enrich_titles_cached(
                doc_store,
                scope.tenant(),
                &mut all_neighbours,
                &mut meta_cache,
            )
            .await;
            reranked_hits = weave_neighbours_with(
                &reranked_hits,
                &all_neighbours,
                opts.neighbour_window,
                |h| h.chunk.id.0.clone(),
                |h| h.chunk.document_id.0.clone(),
                |h| h.chunk.metadata.chunk_index.unwrap_or(0),
            );
        }
    }
    timings.neighbours_ms = neighbours_sw.elapsed_ms();

    // 8. Figures (when figure_limit > 0)
    let figures_sw = Stopwatch::start(clock);
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

        match index.figures(scope, &retrieved_doc_ids).await {
            Ok(fig_candidates) => {
                let selected = select_figures(
                    fig_candidates,
                    query,
                    &retrieved_text,
                    &anchored_chunks,
                    opts.figure_limit,
                );
                let mut figures: Vec<SearchHit> = selected
                    .into_iter()
                    .map(|c| candidate_to_hit_from(c, ProvenanceArm::Figure))
                    .collect();
                if let Some(gate) = &opts.figure_gate {
                    if !figures.is_empty() {
                        let gate_candidates: Vec<FigureGateCandidate> = figures
                            .iter()
                            .map(|f| FigureGateCandidate {
                                id: f.chunk.id.0.clone(),
                                asset_key: f.chunk.metadata.asset_key.clone().unwrap_or_default(),
                                caption: f.chunk.content.clone(),
                            })
                            .collect();
                        match gate.gate_figures(query, &gate_candidates).await {
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
                enrich_titles_cached(doc_store, scope.tenant(), &mut figures, &mut meta_cache)
                    .await;
                reranked_hits.extend(figures);
            }
            Err(_) => {
                degrades.push(RetrievalDegrade::FiguresPartial);
            }
        }
    }
    timings.figures_ms = figures_sw.elapsed_ms();

    // 9. Build context string and sources with kb-core
    let context_sw = Stopwatch::start(clock);
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
    timings.context_ms = context_sw.elapsed_ms();
    timings.total_ms = total_sw.elapsed_ms();
    timings.engine_total_ms = timings.total_ms;

    Ok(RetrievalOutcome {
        chunks: reranked_hits,
        sources,
        context,
        degrades,
        timings,
    })
}
