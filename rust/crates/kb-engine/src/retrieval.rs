//! Retrieval pipeline implementing hybrid search, fusion, reranking,
//! neighbour weave, figure co-retrieval, and context building.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use kb_core::{
    ArmPlan, BoostConfig, ContextItem, QueryClass, RecencyField, RrfOptions, SkippedArm,
    SourceEntry, authority_factor, authority_value, base_from_rank, base_from_rerank_in,
    build_context_and_sources, classify_query, date_value, normalise_authority, parse_date,
    recency_factor, reciprocal_rank_fusion_with, rerank_scores_are_probabilities,
    weave_neighbours_with,
};
use kb_ports::{
    Candidate, Chunk, ChunkId, ChunkMetadata, DocumentId, DocumentMeta, DocumentScope,
    DocumentStore, Embedder, EngineError, FigureGate, FigureGateCandidate, Fusion, HybridQuery,
    KeywordQuery, PortError, ProvenanceArm, RerankCandidate, Reranker, RetrievalDegrade,
    RetrievalIndex, RetrievalProvenance, SearchHit, TenantId, sort_candidates,
};
use tracing::Instrument;

use crate::QueryExpander;
use crate::global_arm::{GlobalArm, fuse_global};
use crate::graph_arm::{GraphArm, GraphRanking};
use crate::page_arm::PageArm;
use crate::timing::{Clock, StageTimings, Stopwatch, stage_span};
use crate::tree_arm::{TreeArm, TreeStats, fuse_tree, tree_chunk_ids};

/// Parameters for one retrieval execution.
#[derive(Clone)]
pub struct RetrievalOptions {
    pub max_chunks: u32,
    pub min_similarity: f32,
    /// Optional relevance floor (RET-2). `None` leaves the pipeline exactly as
    /// it was; `Some(t)` keeps only hits whose comparable score is `>= t`.
    ///
    /// The per-request `minScore` overrides the server default, which is
    /// resolved by the caller ([`crate::Engine::retrieve_with_floor`]).
    pub min_score: Option<f32>,
    pub candidate_pool_size: u32,
    /// RRF constant for the two-arm fusion (`EngineSettings::rrf_k`).
    pub rrf_k: u32,
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
    /// The global (graph community) arm, when `KB_GLOBAL_ARM` is on. `None`
    /// leaves the pipeline byte-identical to the one before `DEC-5`: the query
    /// is not classified for this arm and no community store is read.
    pub global_arm: Option<std::sync::Arc<GlobalArm>>,
    /// The page-image arm, when configured. `None` leaves the pipeline as it was.
    pub page_arm: Option<std::sync::Arc<PageArm>>,
    /// The tree arm (`DEC-4`), when `KB_TREE_ARM` is on. `None` leaves the
    /// pipeline byte-identical to the previous release; `Some` only runs it for
    /// queries the classifier labels `structural`.
    pub tree_arm: Option<std::sync::Arc<TreeArm>>,
    /// Optional recency/authority boost (`RET-4`). `None` — or a config with
    /// neither boost — leaves the ranking exactly as it was.
    pub boost: Option<BoostConfig>,
    /// Evaluation instant for the recency boost. Tests pin it; production
    /// leaves it `None` and the pipeline reads the clock once, at the boost
    /// step.
    pub boost_now: Option<DateTime<Utc>>,
    /// Adaptive arm choice (RET-5). Off by default: the pipeline runs exactly
    /// the arms every earlier release ran. On, the query is classified and the
    /// arms its class cannot use are not launched.
    pub adaptive_arms: bool,
}

impl fmt::Debug for RetrievalOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetrievalOptions")
            .field("max_chunks", &self.max_chunks)
            .field("min_similarity", &self.min_similarity)
            .field("min_score", &self.min_score)
            .field("candidate_pool_size", &self.candidate_pool_size)
            .field("rrf_k", &self.rrf_k)
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
            .field("global_arm", &self.global_arm.as_ref().map(|a| a.config()))
            .field("page_arm", &self.page_arm.as_ref().map(|a| a.config))
            .field("tree_arm", &self.tree_arm.as_ref().map(|a| a.config))
            .field("boost", &self.boost)
            .field("adaptive_arms", &self.adaptive_arms)
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
            min_score: None,
            candidate_pool_size: 20,
            rrf_k: 60,
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
            global_arm: None,
            page_arm: None,
            tree_arm: None,
            boost: None,
            boost_now: None,
            adaptive_arms: false,
        }
    }
}

/// What the global arm did, for `stats.globalArm`.
///
/// Present only when the arm ran: `KB_GLOBAL_ARM` on *and* the query classified
/// aggregative. `communities: 0` is a legitimate answer — the tenant has no
/// community set, so the arm had nothing to say. It is not a degrade.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlobalArmStats {
    /// Communities selected by BM25 over the summary text.
    pub communities: usize,
    /// Member chunks the arm contributed to the fused list.
    pub chunks: usize,
}

/// Which comparable score the relevance floor compared against.
///
/// The fused RRF score is deliberately absent: it is a sum of `1/(k + rank)`
/// terms, so it is rank-only and its absolute value says nothing about how
/// relevant a hit is, let alone how relevant it is across queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorScoreKind {
    /// The cross-encoder reranker's score. Comparable across queries only
    /// after calibration, which is why the default is unset.
    Rerank,
    /// The dense arm's cosine similarity.
    Dense,
}

impl FloorScoreKind {
    /// The wire spelling, matching `stats.floorScoreKind`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rerank => "rerank",
            Self::Dense => "dense",
        }
    }
}

/// What the floor did, for the response's `stats` object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloorStats {
    pub score_kind: FloorScoreKind,
    /// Hits removed by the floor (before neighbours and figures are woven in).
    pub dropped: usize,
}

/// What a recency/authority boost did (`RET-4`), for `stats.boost`.
///
/// Each count is the number of returned hits that carried a usable field for
/// that boost — a parseable date, or a numeric authority. A hit without the
/// field is not an error and is counted nowhere: it kept its base relevance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoostStats {
    pub recency: usize,
    pub authority: usize,
}

/// What adaptive arm choice did, for the response's `stats` object.
///
/// Present only when adaptive mode ran. `skipped_arms` is empty after a
/// fallback, because a fallback ran the full plan: naming an arm as skipped
/// while it actually ran would be a lie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptiveStats {
    /// The question's class (`stats.queryClass`).
    pub class: QueryClass,
    /// The arms the plan turned off, each with the class's reason.
    pub skipped_arms: Vec<SkippedArm>,
    /// True when the plan's skip left nothing and the full plan was run once.
    pub arm_plan_fallback: bool,
}

/// The outcome of the retrieval pipeline.
#[derive(Debug, Clone)]
pub struct RetrievalOutcome {
    pub chunks: Vec<SearchHit>,
    pub sources: Vec<SourceEntry>,
    pub context: String,
    pub degrades: Vec<RetrievalDegrade>,
    /// A human-readable explanation for a result that is empty on purpose,
    /// e.g. "the floor removed every hit". `None` for an ordinary answer.
    pub note: Option<String>,
    /// `Some` only when a floor was configured and actually applied.
    pub floor: Option<FloorStats>,
    /// `Some` only when the request asked for a boost; absent (not zero) when
    /// it did not, so a response that never boosted is byte-identical to one
    /// from before `RET-4`.
    pub boost: Option<BoostStats>,
    /// `Some` only when adaptive arm choice ran (`stats.queryClass` and
    /// `stats.skippedArms`). `None` on a default search, so an old client sees
    /// exactly the earlier shape.
    pub adaptive: Option<AdaptiveStats>,
    /// `Some` only when the tree arm ran and had nodes to offer (`stats.treeArm`).
    /// `None` on every ordinary search, so an old client sees exactly the
    /// earlier shape.
    pub tree: Option<TreeStats>,
    /// `Some` only when the global community arm ran (`KB_GLOBAL_ARM` on and an
    /// aggregative query). `None` on every other search, so a server with the
    /// flag off answers exactly as it did before `DEC-5`.
    pub global_arm: Option<GlobalArmStats>,
    /// What each stage cost. Always present: a request nobody times is a
    /// request nobody can make faster, and the numbers are the only evidence
    /// a retrieval change can be judged by.
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
    let dense_score = c.scores.dense;
    let is_bm25 = dense_score.is_none() && c.scores.sparse.is_some();
    let similarity = if is_bm25 {
        0.0
    } else {
        dense_score.unwrap_or(c.scores.fused)
    };
    SearchHit {
        chunk: Chunk {
            id: c.unit_id,
            document_id: c.document_id,
            document_title: String::new(),
            external_ref: None,
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
        dense_score,
        // Set later, only when a reranker actually scored this hit.
        rerank_score: None,
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
            h.chunk.external_ref = meta.external_ref.clone();
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

/// Did the keyword arm produce this hit?
///
/// A hit can carry both arms; the exemption the floor grants is for the
/// keyword arm's exact match, so a hit the sparse arm never ranked is not
/// exempt even when its text happens to contain the token.
fn keyword_matched(hit: &SearchHit) -> bool {
    hit.is_bm25
        || hit
            .retrieval
            .as_ref()
            .is_some_and(|p| p.has(ProvenanceArm::Sparse))
}

/// The comparable score the floor compares against, if this hit has one.
///
/// Missing is *not* the same as zero: a hit with no dense score (BM25-only, or
/// fetched without scoring) has no evidence either way, and a floor that
/// treated "unknown" as "passes" would let every keyword hit through.
fn floor_score(hit: &SearchHit, kind: FloorScoreKind) -> Option<f32> {
    match kind {
        FloorScoreKind::Rerank => hit.rerank_score,
        FloorScoreKind::Dense => hit.dense_score,
    }
}

/// Keep hits at or above `threshold`, except an exact-match hit the keyword
/// arm matched. Returns the kept hits (ranks renumbered) and how many dropped.
fn apply_relevance_floor(
    hits: Vec<SearchHit>,
    kind: FloorScoreKind,
    threshold: f32,
    signature: &kb_core::QuerySignature,
) -> (Vec<SearchHit>, usize) {
    let mut kept: Vec<SearchHit> = Vec::with_capacity(hits.len());
    let mut dropped = 0usize;
    for hit in hits {
        // The exemption is deliberately narrow: the query must be an
        // identifier/quoted phrase, the keyword arm must have matched this
        // hit, and the hit's text must literally contain the token. A question
        // that merely mentions a model number does not get a free pass for
        // unrelated passages.
        let exempt = signature.is_exact()
            && keyword_matched(&hit)
            && signature.matches_content(&hit.chunk.content);
        let passes = floor_score(&hit, kind).is_some_and(|score| score >= threshold);
        if exempt || passes {
            kept.push(hit);
        } else {
            dropped += 1;
        }
    }
    for (rank, hit) in kept.iter_mut().enumerate() {
        hit.rank = rank as u32;
    }
    (kept, dropped)
}

/// The document's date in the field the request named.
///
/// `createdAt`/`updatedAt` are the Postgres timestamps the store already
/// returned with the document row; `metadata.<key>` reads the top-level JSONB
/// key. An absent or unparseable value is `None` — no boost, never an error.
fn recency_date(meta: &DocumentMeta, field: &RecencyField) -> Option<DateTime<Utc>> {
    match field {
        RecencyField::CreatedAt => parse_date(&meta.created_at),
        RecencyField::UpdatedAt => parse_date(&meta.updated_at),
        RecencyField::Metadata(key) => meta
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get(key))
            .and_then(date_value),
    }
}

/// The document's authority value, when the JSONB metadata carries a number.
fn authority_number(meta: &DocumentMeta, key: &str) -> Option<f64> {
    meta.metadata
        .as_ref()
        .and_then(|metadata| metadata.get(key))
        .and_then(authority_value)
}

/// Apply the request's recency/authority boosts and re-order the hits.
///
/// Runs after fusion, the rerank and the relevance floor, and before the final
/// truncation to `max_chunks`. It deliberately touches only the *order* and
/// `rank`: `rerank_score`, `dense_score` and `similarity` are the scores the
/// floor compares against, and the floor must keep meaning what RET-2 defined.
///
/// The new order is `final` descending, then the same deterministic keys
/// `kb_ports::sort_candidates` uses (content, page, chunk index) — never a
/// document or chunk uuid, so a tie does not depend on an ingest's random id.
///
/// Returns what was actually applied. When the request is off, or no hit
/// carries a usable field, the hits are left exactly as they arrived: that is
/// the "rank-identical when off" guarantee, and it also covers a request that
/// asks for a boost the corpus cannot satisfy.
pub fn apply_boosts(
    hits: &mut Vec<SearchHit>,
    meta: &HashMap<DocumentId, DocumentMeta>,
    config: &BoostConfig,
    now: DateTime<Utc>,
) -> BoostStats {
    let mut stats = BoostStats::default();
    if config.is_off() || hits.is_empty() {
        return stats;
    }

    let metas: Vec<Option<&DocumentMeta>> = hits
        .iter()
        .map(|hit| meta.get(&hit.chunk.document_id))
        .collect();
    let dates: Vec<Option<DateTime<Utc>>> = match &config.recency {
        Some(recency) => metas
            .iter()
            .map(|meta| meta.and_then(|meta| recency_date(meta, &recency.field)))
            .collect(),
        None => vec![None; hits.len()],
    };
    let authority_values: Vec<Option<f64>> = match &config.authority {
        Some(authority) => metas
            .iter()
            .map(|meta| meta.and_then(|meta| authority_number(meta, &authority.field)))
            .collect(),
        None => vec![None; hits.len()],
    };
    let authority_normalised = normalise_authority(&authority_values);

    stats.recency = dates.iter().filter(|date| date.is_some()).count();
    stats.authority = authority_values
        .iter()
        .filter(|value| value.is_some())
        .count();
    if stats.recency == 0 && stats.authority == 0 {
        return stats;
    }

    let recency = config.recency.as_ref();
    let authority = config.authority.as_ref();
    let probabilities = rerank_scores_are_probabilities(hits.iter().filter_map(|h| h.rerank_score));
    let mut scored: Vec<(SearchHit, f64)> = std::mem::take(hits)
        .into_iter()
        .enumerate()
        .map(|(index, hit)| {
            let base = hit.rerank_score.map_or_else(
                || base_from_rank(hit.rank),
                |score| base_from_rerank_in(score, probabilities),
            );
            let mut factor = 1.0_f64;
            if let (Some(recency), Some(date)) = (recency, dates[index]) {
                factor *= recency_factor(date, now, recency.half_life_days, recency.weight);
            }
            if let (Some(authority), Some(normalised)) = (authority, authority_normalised[index]) {
                factor *= authority_factor(normalised, authority.weight);
            }
            (hit, base * factor)
        })
        .collect();

    scored.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.chunk.content.cmp(&b.0.chunk.content))
            .then_with(|| a.0.chunk.metadata.page.cmp(&b.0.chunk.metadata.page))
            .then_with(|| {
                a.0.chunk
                    .metadata
                    .chunk_index
                    .cmp(&b.0.chunk.metadata.chunk_index)
            })
    });
    for (rank, (mut hit, _)) in scored.into_iter().enumerate() {
        hit.rank = rank as u32;
        hits.push(hit);
    }
    stats
}

/// [`apply_boosts`] at the current wall clock.
///
/// Used by the by-id search branch, which does not go through the pipeline but
/// still has the document rows it read for titles.
pub fn apply_boosts_now(
    hits: &mut Vec<SearchHit>,
    meta: &HashMap<DocumentId, DocumentMeta>,
    config: &BoostConfig,
) -> BoostStats {
    apply_boosts(hits, meta, config, Utc::now())
}

/// Execute the full retrieval pipeline.
///
/// Every collaborator is passed explicitly rather than behind a context
/// object: the engine receives its stores as arguments, which is the whole
/// point of the port boundary.
///
/// With `RetrievalOptions::adaptive_arms` off this is exactly the pre-RET-5
/// pipeline. With it on, the query is classified once and the arms its class
/// cannot use are not launched; every skipped arm is named on
/// [`RetrievalOutcome::adaptive`]. If the skip leaves nothing and a skipped arm
/// could have contributed candidates, the full plan runs once more and the
/// outcome says `arm_plan_fallback: true` — a plan that guessed wrong must not
/// turn a searchable question into an empty answer.
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
    if !opts.adaptive_arms {
        return execute_pipeline_with_plan(
            doc_store,
            index,
            embedder,
            reranker,
            expander,
            scope,
            query,
            opts,
            clock,
            &ArmPlan::all(),
        )
        .await;
    }

    let signature = classify_query(query);
    let plan = signature.arm_plan();
    let skipped_candidate_arm = !plan.dense
        || !plan.sparse
        || (!plan.page_image && opts.page_arm.is_some())
        || (!plan.graph && opts.graph_arm.is_some());
    let mut outcome = execute_pipeline_with_plan(
        doc_store, index, embedder, reranker, expander, scope, query, opts, clock, &plan,
    )
    .await?;

    if outcome.chunks.is_empty() && skipped_candidate_arm && outcome.note.is_none() {
        tracing::info!(
            class = signature.class.as_str(),
            "adaptive plan returned nothing; re-running the full plan once"
        );
        outcome = execute_pipeline_with_plan(
            doc_store,
            index,
            embedder,
            reranker,
            expander,
            scope,
            query,
            opts,
            clock,
            &ArmPlan::all(),
        )
        .await?;
        outcome.adaptive = Some(AdaptiveStats {
            class: signature.class,
            skipped_arms: Vec::new(),
            arm_plan_fallback: true,
        });
        return Ok(outcome);
    }

    outcome.adaptive = Some(AdaptiveStats {
        class: signature.class,
        skipped_arms: signature.skipped_arms(),
        arm_plan_fallback: false,
    });
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
async fn execute_pipeline_with_plan(
    doc_store: &Arc<dyn DocumentStore>,
    index: &Arc<dyn RetrievalIndex>,
    embedder: &Arc<dyn Embedder>,
    reranker: &Arc<dyn Reranker>,
    expander: &Arc<dyn QueryExpander>,
    scope: &DocumentScope,
    query: &str,
    opts: &RetrievalOptions,
    clock: &dyn Clock,
    plan: &ArmPlan,
) -> Result<RetrievalOutcome, EngineError> {
    let total_sw = Stopwatch::start(clock);
    let mut timings = StageTimings::default();
    let mut degrades: Vec<RetrievalDegrade> = Vec::new();
    let pool_size = opts.candidate_pool_size.clamp(1, 100);

    // 0. The page-image arm runs alongside everything below: its query embedding
    // is a second round trip to a different model, and it must not add to the
    // text arms' latency. Joined after the rerank, where it is fused.
    //
    // Adaptive mode may skip the arm entirely: no task is spawned, so its query
    // embed and page search (the expensive part) never happen.
    let page_task = if plan.page_image {
        opts.page_arm.clone()
    } else {
        None
    }
    .map(|arm| {
        let scope = scope.clone();
        let query = query.to_string();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let res = tokio::time::timeout(arm.config.timeout, arm.rank(&scope, &query)).await;
            (res, started.elapsed())
        })
    });

    // 1. Query expansion. Expansion feeds the dense arm only, so it is skipped
    // with that arm rather than spending a paid call for nothing.
    let expand_sw = Stopwatch::start(clock);
    let variants: Vec<String> = if opts.expansion_enabled && plan.dense {
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
    let mut cached: Option<bool> = None;
    let mut embed_failed = false;
    let mut query_vectors: Vec<Vec<f32>> = Vec::new();
    if plan.dense {
        let embeds = variants.iter().map(|q| embedder.embed_query_detailed(q));
        let vectors = futures::future::join_all(embeds).await;
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
    }
    timings.query_embed_ms = embed_sw.elapsed_ms();
    timings.query_embed_cached = cached.unwrap_or(false);
    if plan.dense && (embed_failed || query_vectors.is_empty()) {
        degrades.push(RetrievalDegrade::VectorArmFailed);
    }

    let sparse_query = if opts.hybrid_bm25_enabled && plan.sparse {
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
            fusion: Fusion::Rrf {
                k: opts.rrf_k.max(1),
            },
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
        // One ordering for every path: `unit_id` embeds the per-ingest document
        // UUID, so sorting on it would make an expanded query's ties depend on
        // the ingest. `sort_candidates` breaks them on the corpus instead.
        sort_candidates(&mut candidates);
        candidates.truncate(pool_size as usize);
    }

    // Min similarity filtering applies to hits that matched only on the dense arm.
    // Pure BM25 hits or hits matching BM25 are preserved.
    candidates.retain(|c| {
        c.scores
            .dense
            .is_none_or(|sim| sim >= opts.min_similarity || c.scores.sparse.is_some())
    });

    // 4b. The graph arm, fused as one more ranked list. Adaptive mode may skip
    // it: no seed lookup and no scoped-graph walk happens at all.
    let graph_sw = Stopwatch::start(clock);
    let graph_ranks = if !plan.graph {
        HashMap::new()
    } else {
        match &opts.graph_arm {
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
        }
    };
    timings.graph_arm_ms = graph_sw.elapsed_ms();

    // 4c. The tree arm (`DEC-4`): structural questions only, never the full
    // corpus. The classifier runs regardless of adaptive mode, because the
    // gate is a property of the question, not of whether other arms were
    // skipped. A failed outline read or page fetch is one more degraded arm;
    // the candidates already scored go out unchanged (AGENTS rule 6).
    let mut tree_stats: Option<TreeStats> = None;
    let mut tree_ids: HashSet<ChunkId> = HashSet::new();
    let tree_class = opts.tree_arm.as_ref().map(|_| classify_query(query).class);
    if let (Some(arm), Some(QueryClass::Structural)) = (opts.tree_arm.as_ref(), tree_class) {
        match arm.rank(doc_store, index, scope, query).await {
            Ok(ranking) => {
                if !ranking.chunks.is_empty() {
                    tree_ids = tree_chunk_ids(&ranking.chunks);
                    candidates = fuse_tree(candidates, ranking.chunks, &arm.config);
                }
                if ranking.stats.nodes > 0 {
                    tree_stats = Some(ranking.stats);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "tree arm failed (non-fatal)");
                degrades.push(RetrievalDegrade::TreeArmFailed);
            }
        }
    }
    // 4c. The global (graph community) arm, `DEC-5`. It runs only when the flag
    // is on *and* the query is aggregative: a summary of a whole community
    // cannot answer "what does chapter 2 say", and running it for every query
    // would spend latency on the questions it was measured to hurt. With the
    // flag off (`opts.global_arm == None`) nothing here executes, so the
    // pipeline is byte-identical to the one before this arm existed.
    // No clock read when the flag is off: a request that never looked at this
    // arm must not pay even two `Instant::now` calls for it.
    let global_sw = opts.global_arm.as_ref().map(|_| Stopwatch::start(clock));
    let mut global_stats: Option<GlobalArmStats> = None;
    let mut global_ranks: HashMap<ChunkId, u32> = HashMap::new();
    if let Some(arm) = opts.global_arm.as_ref() {
        let aggregative = classify_query(query).class == QueryClass::Aggregative;
        if aggregative {
            match arm.rank(scope, query).await {
                Ok(ranking) => {
                    // No communities is the arm having nothing to say, not a
                    // failure: stats says so and the other arms go out unchanged.
                    global_stats = Some(GlobalArmStats {
                        communities: ranking.communities,
                        chunks: ranking.chunks.len(),
                    });
                    if !ranking.chunks.is_empty() {
                        match fuse_global(
                            index,
                            scope,
                            &ranking.chunks,
                            &candidates,
                            arm.config().weight,
                            opts.rrf_k,
                            pool_size,
                        )
                        .await
                        {
                            Ok((fused, ranks)) => {
                                candidates = fused;
                                global_ranks = ranks;
                            }
                            Err(e) => {
                                // A failed arm is a degraded search, not a failed
                                // one: the candidates already fused go out as they
                                // are, and the response names the failure.
                                tracing::warn!(error = %e, "global arm fetch failed (non-fatal)");
                                degrades.push(RetrievalDegrade::GlobalArmFailed);
                                // Nothing was fused, so the count is 0 even
                                // though the communities were selected.
                                global_stats = Some(GlobalArmStats {
                                    communities: ranking.communities,
                                    chunks: 0,
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "global arm failed (non-fatal)");
                    degrades.push(RetrievalDegrade::GlobalArmFailed);
                }
            }
        }
    }
    if let Some(stopwatch) = global_sw {
        timings.global_arm_ms = stopwatch.elapsed_ms();
    }

    let mut hits: Vec<SearchHit> = candidates.into_iter().map(candidate_to_hit).collect();

    // A tree hit is one the outline brought in; the text arms never scored it.
    for hit in &mut hits {
        if tree_ids.contains(&hit.chunk.id) {
            hit.retrieval = Some(
                hit.retrieval
                    .clone()
                    .unwrap_or_else(|| RetrievalProvenance::unscored(ProvenanceArm::Tree))
                    .with_arm(ProvenanceArm::Tree),
            );
        }
    }

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
        // The global arm's position is provenance too, and it is the only record
        // that a chunk reached the answer through a community summary rather
        // than through its own text.
        if global_ranks.contains_key(&hit.chunk.id) {
            hit.retrieval = Some(
                hit.retrieval
                    .clone()
                    .unwrap_or_else(|| RetrievalProvenance::unscored(ProvenanceArm::Global))
                    .with_arm(ProvenanceArm::Global),
            );
        }
    }

    // Enrich titles & categories
    let enrich_sw = Stopwatch::start(clock);
    enrich_titles_cached(doc_store, scope.tenant(), &mut hits, &mut meta_cache).await;
    timings.enrich_ms = enrich_sw.elapsed_ms();

    // 5. Optional rerank, whenever there are at least two candidates to order.
    //
    // Bun (and this engine before 2026-10-23) reranked only when the pool held
    // more candidates than the caller asked for, so a caller asking for 20+
    // chunks silently got the fused order: the reranker is worth +0.037 nDCG@10
    // on miracl-id-200 and +0.108 on hotpot (docs/eval-ab/README.md), and the
    // number of chunks requested says nothing about whether their order
    // matters. Deliberate difference from Bun: docs/rust-cutover.md §8.
    let max_chunks = opts.max_chunks.clamp(1, 50) as usize;
    let rerank_sw = Stopwatch::start(clock);
    let rerank_future = async {
        if plan.rerank && opts.rerank_enabled && hits.len() > 1 {
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

            // With a relevance floor (RET-2) every candidate needs a score, or the
            // floor would drop every hit below `final_k` for having none and cut
            // the answer to `final_k` hits. So the reranker scores them all (a
            // rerank API bills per document sent, not per result returned), while
            // the order below stays exactly the floorless one: the first
            // `final_k` by rerank score, the rest in fused order.
            let request_k = if opts.min_score.is_some() {
                u32::try_from(candidates.len())
                    .unwrap_or(u32::MAX)
                    .max(final_k)
            } else {
                final_k
            };
            let rerank_timeout = std::time::Duration::from_millis(opts.rerank_timeout_ms.max(100));
            match tokio::time::timeout(
                rerank_timeout,
                reranker.rerank(query, candidates, request_k),
            )
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
                    // Scores of candidates past `final_k`, attached in fused order.
                    let mut tail_scores: HashMap<String, f32> = HashMap::new();
                    for (rank, item) in ordered.into_iter().enumerate() {
                        if rank >= final_k as usize {
                            tail_scores.entry(item.id.0.clone()).or_insert(item.score);
                            continue;
                        }
                        if let Some(hits_for_id) = hit_map.get_mut(&item.id.0) {
                            if !hits_for_id.is_empty() {
                                let mut hit = hits_for_id.remove(0);
                                hit.rank = rank as u32;
                                // Keep the score the floor will compare against.
                                // It was previously thrown away after ordering.
                                hit.rerank_score = Some(item.score);
                                reordered.push(hit);
                            }
                        }
                    }
                    for id in fused_order {
                        if let Some(remaining_hits) = hit_map.get_mut(&id) {
                            for mut hit in remaining_hits.drain(..) {
                                hit.rank = reordered.len() as u32;
                                if let Some(score) = tail_scores.get(&id) {
                                    hit.rerank_score = Some(*score);
                                }
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

    // 5a. Optional relevance floor (RET-2). This is the only place the
    // pipeline answers "nothing here is relevant" instead of "here is the
    // nearest of nothing".
    //
    // It runs after the rerank because the reranker's score is the one that
    // means the same across queries when a reranker ran. It runs before the
    // page arm, neighbours and figures because those phases attach context to
    // a ranked hit: with no hit left there is nothing to attach, and letting
    // the page arm repopulate the answer would defeat the floor.
    let mut floor_stats: Option<FloorStats> = None;
    let mut floor_note: Option<String> = None;
    if let Some(threshold) = opts.min_score {
        let rerank_ran =
            opts.rerank_enabled && reranked_hits.iter().any(|h| h.rerank_score.is_some());
        let rerank_failed = degrades.contains(&RetrievalDegrade::RerankUnavailable);
        let vector_failed = degrades.contains(&RetrievalDegrade::VectorArmFailed);
        // A floor whose score arm failed must not run: an empty answer from a
        // failed arm is indistinguishable from "nothing is relevant", which is
        // exactly the silent failure AGENTS rule 6 forbids.
        let skip = (opts.rerank_enabled && rerank_failed) || (!rerank_ran && vector_failed);
        if skip {
            tracing::warn!(
                min_score = threshold,
                "relevance floor skipped: the arm that supplies its score degraded"
            );
            degrades.push(RetrievalDegrade::FloorSkipped);
        } else {
            let kind = if rerank_ran {
                FloorScoreKind::Rerank
            } else {
                FloorScoreKind::Dense
            };
            let signature = kb_core::classify_query(query);
            let (kept, dropped) = apply_relevance_floor(reranked_hits, kind, threshold, &signature);
            reranked_hits = kept;
            floor_stats = Some(FloorStats {
                score_kind: kind,
                dropped,
            });
            if dropped > 0 && reranked_hits.is_empty() {
                floor_note = Some(format!(
                    "No result passed the relevance floor (minScore={threshold}, score={}): \
                     {} candidate(s) were dropped",
                    kind.as_str(),
                    dropped
                ));
                // Nothing is left to rank, so do not let the page arm turn
                // this into a non-empty visual answer.
                if let Some(task) = page_task.as_ref() {
                    task.abort();
                }
                timings.total_ms = total_sw.elapsed_ms();
                timings.engine_total_ms = timings.total_ms;
                return Ok(RetrievalOutcome {
                    chunks: Vec::new(),
                    sources: Vec::new(),
                    context: String::new(),
                    degrades,
                    note: floor_note,
                    floor: floor_stats,
                    boost: opts.boost.as_ref().map(|_| BoostStats::default()),
                    adaptive: None,
                    tree: tree_stats,
                    // The global arm runs after the floor, so a floored-to-empty
                    // answer has not reached it: absent is the honest value.
                    global_arm: None,
                    timings,
                });
            }
        }
    }

    // 5b. Fuse the page-image arm's pages with the ranked text hits, at page
    // level. A failed or slow arm degrades the search; it never empties it.
    let pages_sw = Stopwatch::start(clock);
    if let (Some(task), Some(arm)) = (page_task, opts.page_arm.as_ref()) {
        match task.await {
            Ok((Ok(Ok(pages)), elapsed)) => {
                timings.page_arm_ms = elapsed.as_secs_f64() * 1000.0;
                // Page-level reranking uses the same reranker as the chunks, and
                // only when reranking is on for this search.
                let rerank_ctx = (opts.rerank_enabled && arm.config.rerank.is_some()).then(|| {
                    crate::page_arm::PageRerankContext {
                        page_index: &arm.index,
                        reranker,
                        query,
                    }
                });
                let outcome = crate::page_arm::merge_pages(
                    index,
                    scope,
                    reranked_hits,
                    &pages,
                    &arm.config,
                    max_chunks,
                    rerank_ctx,
                )
                .await;
                reranked_hits = outcome.hits;
                if outcome.rerank_failed {
                    degrades.push(RetrievalDegrade::RerankUnavailable);
                }
                if outcome.fetch_partial {
                    degrades.push(RetrievalDegrade::PageArmUnavailable);
                }
                {
                    let mut page_only: Vec<SearchHit> = reranked_hits
                        .iter()
                        .filter(|h| h.chunk.document_title.is_empty())
                        .cloned()
                        .collect();
                    if !page_only.is_empty() {
                        enrich_titles_cached(
                            doc_store,
                            scope.tenant(),
                            &mut page_only,
                            &mut meta_cache,
                        )
                        .await;
                        let titled: HashMap<ChunkId, SearchHit> = page_only
                            .into_iter()
                            .map(|h| (h.chunk.id.clone(), h))
                            .collect();
                        for h in &mut reranked_hits {
                            if let Some(t) = titled.get(&h.chunk.id) {
                                h.chunk.document_title.clone_from(&t.chunk.document_title);
                                h.chunk.categories.clone_from(&t.chunk.categories);
                                h.chunk.external_ref.clone_from(&t.chunk.external_ref);
                            }
                        }
                    }
                }
                // 5c. Opt-in live vision rerank of the top page images, after
                // the page rerank. Its model order is blended with the current
                // one by weighted RRF; any failure keeps the order and is
                // reported, never swallowed (AGENTS rule 6). Every page it sees
                // comes from a hit that already passed scope.
                if plan.rerank
                    && opts.rerank_enabled
                    && let Some(vision) = arm.vision.as_ref()
                {
                    let vision_sw = Stopwatch::start(clock);
                    let ctx = crate::page_arm::PageVisionContext {
                        scorer: &vision.scorer,
                        blobs: &vision.blobs,
                        config: &vision.config,
                        query,
                        tenant: scope.tenant(),
                    };
                    match crate::page_arm::vision_rerank(&mut reranked_hits, &ctx).await {
                        Ok(()) => {}
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "vision rerank failed (non-fatal); keeping page order"
                            );
                            degrades.push(RetrievalDegrade::VisionRerankUnavailable);
                        }
                    }
                    timings.vision_rerank_ms = vision_sw.elapsed_ms();
                }
            }
            Ok((Ok(Err(e)), _)) => {
                tracing::warn!(error = %e, "page arm failed (non-fatal)");
                degrades.push(RetrievalDegrade::PageArmUnavailable);
            }
            Ok((Err(_), _)) => {
                tracing::warn!("page arm timed out (non-fatal)");
                degrades.push(RetrievalDegrade::PageArmUnavailable);
            }
            Err(e) => {
                tracing::warn!(error = %e, "page arm task failed (non-fatal)");
                degrades.push(RetrievalDegrade::PageArmUnavailable);
            }
        }
    }
    timings.pages_ms = pages_sw.elapsed_ms();

    // 5d. Optional recency/authority boosts (`RET-4`). Deliberately after the
    // floor: the floor must keep comparing the raw rerank/dense score, and a
    // boost that moved a hit below the threshold would erase the only
    // cross-query meaning the floor has. It runs before the cut to
    // `max_chunks`, so a promoted hit is not lost to a pre-boost truncation.
    // The document rows are the ones already read for title enrichment — no
    // extra round trip — and a document missing from that map simply gets no
    // boost, which is the same as "the document has no field".
    let boost_stats = opts
        .boost
        .as_ref()
        .filter(|config| !config.is_off())
        .map(|config| {
            let now = opts.boost_now.unwrap_or_else(Utc::now);
            apply_boosts(&mut reranked_hits, &meta_cache, config, now)
        });

    // 6. Cut to max_chunks (1..=50)
    reranked_hits.truncate(max_chunks);
    // The ranking, before neighbours and figures are woven in for context.
    let ranked_ids: Vec<ChunkId> = reranked_hits.iter().map(|h| h.chunk.id.clone()).collect();

    // 7. Neighbours (when window > 0)
    let neighbours_sw = Stopwatch::start(clock);
    if plan.neighbour && opts.neighbour_window > 0 && !reranked_hits.is_empty() {
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
    if plan.figure && opts.figure_limit > 0 && !reranked_hits.is_empty() {
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
        chunks: ranked_first(reranked_hits, &ranked_ids),
        sources,
        context,
        degrades,
        note: floor_note,
        floor: floor_stats,
        boost: boost_stats,
        adaptive: None,
        tree: tree_stats,
        global_arm: global_stats,
        timings,
    })
}

/// The ranked hits first, in rank order; then everything added for context
/// (neighbours, figures), in the order it was woven.
///
/// The context text keeps the woven reading order, where a neighbour sits next
/// to its hit (and a preceding neighbour before it). The `chunks` list is what
/// a client ranks and cites, so a neighbour must not displace a hit: woven in,
/// neighbours cost 0.033 page nDCG@10 on vidore3-hr-en (0.435 against 0.468
/// with neighbours off), and could move the top hit to second place.
fn ranked_first(woven: Vec<SearchHit>, ranked_ids: &[ChunkId]) -> Vec<SearchHit> {
    let position: HashMap<&ChunkId, usize> = ranked_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();
    let (mut ranked, rest): (Vec<SearchHit>, Vec<SearchHit>) = woven
        .into_iter()
        .partition(|h| position.contains_key(&h.chunk.id));
    ranked.sort_by_key(|h| position.get(&h.chunk.id).copied().unwrap_or(usize::MAX));
    ranked.extend(rest);
    ranked
}
