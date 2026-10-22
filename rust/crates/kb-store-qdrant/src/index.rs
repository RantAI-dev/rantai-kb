//! `RetrievalIndex` implementation for Qdrant.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use kb_ports::Fusion as PortFusion;
use kb_ports::{
    ArmRanks, ArmScores, Candidate, ChunkId, DocumentId, DocumentScope, HybridQuery,
    IndexCapabilities, IndexUnit, KeywordSemantics, PortError, RetrievalIndex, TenantId,
    sort_candidates,
};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::Fusion as QdrantFusion;
use qdrant_client::qdrant::{
    CountPointsBuilder, DeletePointsBuilder, NamedVectors, PointId, PointStruct,
    PrefetchQueryBuilder, Query, QueryBatchPointsBuilder, QueryPointsBuilder, ScrollPointsBuilder,
    SearchParams, UpsertPointsBuilder, Vector, VectorInput,
};

use crate::ids::point_id;
use crate::payload::{candidate_from, unit_payload};
use crate::settings::{DENSE_VECTOR, QdrantSettings, SPARSE_VECTOR, map};

/// Points below which the dense arm scans exactly instead of walking the HNSW
/// graph, and how long that count is believed for.
///
/// Measured on this box (Qdrant v1.19.2, 1024-dim cosine vectors, k=20, 50
/// queries; reproduce with `cargo test -p kb-store-qdrant --features
/// integration --test exact_vs_hnsw -- --ignored --nocapture` against a live
/// Qdrant):
///
/// | points | exact p50 | HNSW p50 | HNSW recall@20 vs exact |
/// |---:|---:|---:|---:|
/// | 1,500 | 2.38 ms | 2.34 ms | 1.000 |
/// | 15,000 | 10.13 ms | 8.49 ms | 0.960 |
/// | 50,000 | 34.39 ms | 30.86 ms | 0.883 |
///
/// At the size this service actually runs — the miracl-id-50 corpus is 1,477
/// chunks — an exact scan costs **0.04 ms more** than HNSW and loses nothing, so
/// there is no latency argument for approximating here. Above ~15k the trade
/// flips: exact costs 19% more at 15k and 11% at 50k while HNSW drops 4% and 12%
/// of its top-20, and the cost of exact grows linearly (about 0.7 ms per 1k
/// points) while the latency target at 1M chunks is a p95 under 20 ms. Hence the
/// threshold, not a blanket `exact: true`.
pub const EXACT_SEARCH_THRESHOLD: u64 = 20_000;

/// How long a cached point count is believed. The count is one extra round trip,
/// so it is worth a minute of staleness: crossing the threshold changes how
/// *fast* an answer comes, never whether it is right.
const POINT_COUNT_TTL_MS: u64 = 60_000;

/// The Qdrant adapter for `RetrievalIndex`.
#[derive(Clone)]
pub struct QdrantIndex {
    client: Qdrant,
    collection: String,
    settings: QdrantSettings,
    /// Cached `points_count`, with the time it was read: decides
    /// [`EXACT_SEARCH_THRESHOLD`]. In a `tokio::sync::Mutex` because counting is
    /// an await and a `std::sync::Mutex` would be held across it.
    point_count: Arc<tokio::sync::Mutex<Option<(u64, Instant)>>>,
}

impl QdrantIndex {
    /// Create an index with the default collection name for the settings.
    pub fn new(settings: QdrantSettings) -> Result<Self, PortError> {
        let client = settings.client()?;
        let collection = settings.collection_name();
        Ok(Self {
            client,
            collection,
            settings,
            point_count: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    /// Create an index with a specific collection name (e.g. for isolated test runs).
    pub fn new_with_collection(
        settings: QdrantSettings,
        collection: String,
    ) -> Result<Self, PortError> {
        let client = settings.client()?;
        Ok(Self {
            client,
            collection,
            settings,
            point_count: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    /// The collection this index addresses.
    #[must_use]
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// Underlying client.
    #[must_use]
    pub fn client(&self) -> &Qdrant {
        &self.client
    }

    /// Underlying settings.
    #[must_use]
    pub fn settings(&self) -> &QdrantSettings {
        &self.settings
    }

    /// Whether the dense arm should scan exactly, per [`EXACT_SEARCH_THRESHOLD`].
    ///
    /// The count is cached for [`POINT_COUNT_TTL_MS`] and refreshed only when a
    /// search actually needs it. A failing count falls back to the approximate
    /// path (`true`): if the collection cannot be counted, this call is already
    /// in trouble, and an exact scan on top of a timeout would be worse.
    async fn dense_is_exact(&self) -> bool {
        if let Some((count, read_at)) = *self.point_count.lock().await
            && read_at.elapsed().as_millis() < u128::from(POINT_COUNT_TTL_MS)
        {
            return count <= EXACT_SEARCH_THRESHOLD;
        }
        let counted = self
            .client
            .count(CountPointsBuilder::new(&self.collection).exact(true))
            .await
            .map(|response| response.result.map_or(0, |r| r.count));
        match counted {
            Ok(count) => {
                *self.point_count.lock().await = Some((count, Instant::now()));
                count <= EXACT_SEARCH_THRESHOLD
            }
            Err(err) => {
                tracing::warn!(error = %err, "point count failed; searching approximately");
                true
            }
        }
    }

    /// `SearchParams` for one dense query, exact or not.
    async fn dense_params(&self) -> Option<SearchParams> {
        if self.dense_is_exact().await {
            Some(SearchParams {
                exact: Some(true),
                ..Default::default()
            })
        } else {
            None
        }
    }
}

/// One arm's ranked result, in a form whose rank can be assigned deterministically.
///
/// See [`ranked_arm`] for why the raw Qdrant order is not good enough.
#[derive(Clone)]
struct ScoredPoint {
    id: PointId,
    /// `unit_id` from the payload: the tie-break key. `None` only when the query
    /// did not ask for payloads, which none of the adapter's own queries do.
    unit_id: Option<String>,
    score: f32,
    payload: std::collections::HashMap<String, qdrant_client::qdrant::Value>,
}

/// One arm's results, ordered by score and then by a stable key.
///
/// This is the fix for the tiny corpus being non-deterministic across stacks:
/// with an 8-dimensional stub embedding, cosine similarity collapses into a
/// handful of *exactly equal* scores (a probe over the 19-chunk CI corpus found
/// 4-way and 5-way ties), and Qdrant returns tied points in whatever order its
/// segment happens to hold — which depends on ingest order, and ingest order
/// varies because six documents are ingested concurrently. The arm's rank is
/// that position, RRF is a function of ranks, so a tie reshuffling moved
/// nDCG@10 by up to 0.036 on identical data.
///
/// Breaking ties on `unit_id` (a deterministic function of tenant, document and
/// chunk index) makes the arm's ranking a function of the *scores* and the
/// *ids*, neither of which changes between two ingests of the same corpus.
fn ranked_arm(points: &[ScoredPoint]) -> Vec<&ScoredPoint> {
    let mut ordered: Vec<(usize, &ScoredPoint)> = points.iter().enumerate().collect();
    ordered.sort_by(|(left_index, left), (right_index, right)| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| match (&left.unit_id, &right.unit_id) {
                (Some(l), Some(r)) => l.cmp(r),
                // No payload means no id to break ties on, so fall back to the
                // order the store gave, which is what the old code did.
                _ => left_index.cmp(right_index),
            })
    });
    ordered.into_iter().map(|(_, point)| point).collect()
}

/// Score and rank per point, for the RRF path's two lookups.
fn rank_map(points: &[ScoredPoint]) -> HashMap<PointId, (f32, u32)> {
    ranked_arm(points)
        .into_iter()
        .enumerate()
        .map(|(rank, point)| (point.id.clone(), (point.score, rank as u32)))
        .collect()
}

/// The adapter's own query results as rankable points. A point without an id is
/// dropped rather than ranked: it could not be fetched back, fused or de-duplicated.
fn scored_points(results: &[qdrant_client::qdrant::ScoredPoint]) -> Vec<ScoredPoint> {
    results
        .iter()
        .filter_map(|point| {
            Some(ScoredPoint {
                id: point.id.clone()?,
                unit_id: crate::payload::string(&point.payload, crate::payload::KEY_UNIT_ID),
                score: point.score,
                payload: point.payload.clone(),
            })
        })
        .collect()
}

/// One of the generated builders plus `Option<SearchParams>`: the dense arm asks
/// for an exact scan below the threshold and nothing above it.
fn with_params<T>(builder: T, params: &Option<SearchParams>) -> T
where
    T: DenseParams,
{
    match params {
        Some(params) => builder.with_search_params(params.clone()),
        None => builder,
    }
}

/// The one method `QueryPointsBuilder` and `PrefetchQueryBuilder` share.
trait DenseParams: Sized {
    fn with_search_params(self, params: SearchParams) -> Self;
}

impl DenseParams for QueryPointsBuilder {
    fn with_search_params(self, params: SearchParams) -> Self {
        self.params(params)
    }
}

impl DenseParams for PrefetchQueryBuilder {
    fn with_search_params(self, params: SearchParams) -> Self {
        self.params(params)
    }
}

fn point_of(unit: &IndexUnit) -> PointStruct {
    let id = point_id(&unit.tenant, &unit.document_id, unit.chunk_index);
    let mut vectors = NamedVectors::default();
    if !unit.dense.is_empty() {
        vectors = vectors.add_vector(DENSE_VECTOR, unit.dense.clone());
    }
    if !unit.sparse.is_empty() {
        vectors = vectors.add_vector(
            SPARSE_VECTOR,
            Vector::new_sparse(unit.sparse.indices.clone(), unit.sparse.values.clone()),
        );
    }
    PointStruct::new(id.to_string(), vectors, unit_payload(unit))
}

#[async_trait]
impl RetrievalIndex for QdrantIndex {
    async fn upsert(&self, units: Vec<IndexUnit>) -> Result<(), PortError> {
        if units.is_empty() {
            return Ok(());
        }
        for batch in units.chunks(self.settings.upsert_batch) {
            let points: Vec<PointStruct> = batch.iter().map(point_of).collect();
            self.client
                .upsert_points(UpsertPointsBuilder::new(&self.collection, points).wait(true))
                .await
                .map_err(map)?;
        }
        // The cached count is now stale by exactly what was written; dropping it
        // makes the next search re-count, which also fixes an upsert that
        // replaced a document's chunks with a different number of them.
        *self.point_count.lock().await = None;
        Ok(())
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<u64, PortError> {
        let filter = crate::payload::one_document_filter(tenant, document_id);
        let count_resp = self
            .client
            .count(
                CountPointsBuilder::new(&self.collection)
                    .filter(filter.clone())
                    .exact(true),
            )
            .await
            .map_err(map)?;
        let count = count_resp.result.map_or(0, |r| r.count);
        if count == 0 {
            return Ok(0);
        }
        self.client
            .delete_points(
                DeletePointsBuilder::new(&self.collection)
                    .points(filter)
                    .wait(true),
            )
            .await
            .map_err(map)?;
        *self.point_count.lock().await = None;
        Ok(count)
    }

    async fn search(
        &self,
        query: &HybridQuery,
        scope: &DocumentScope,
    ) -> Result<Vec<Candidate>, PortError> {
        if scope.document_ids().is_empty() {
            return Ok(Vec::new());
        }

        let filter = crate::payload::scope_filter(scope);
        let dense_opt = query.dense.as_ref().filter(|v| !v.is_empty());
        let sparse_opt = query.sparse.as_ref().filter(|k| !k.is_empty());

        let k = query.k.max(1) as u64;

        // Below the threshold the dense arm scans exactly; the keyword arm is an
        // inverted index and is exact either way.
        let dense_params = self.dense_params().await;

        if let (Some(dense_vec), None) = (dense_opt, sparse_opt) {
            let q = with_params(
                QueryPointsBuilder::new(&self.collection)
                    .using(DENSE_VECTOR)
                    .query(Query::new_nearest(dense_vec.clone()))
                    .filter(filter)
                    .limit(k)
                    .with_payload(true),
                &dense_params,
            );
            let response = self.client.query(q).await.map_err(map)?;
            let mut candidates = Vec::new();
            for (rank, point) in ranked_arm(&scored_points(&response.result))
                .into_iter()
                .enumerate()
            {
                let rank = rank as u32;
                let scores = ArmScores {
                    dense: Some(point.score),
                    sparse: None,
                    fused: point.score,
                };
                let ranks = ArmRanks {
                    dense: Some(rank),
                    sparse: None,
                    fused: rank,
                };
                if let Some(c) = candidate_from(&point.payload, scores, ranks) {
                    candidates.push(c);
                }
            }
            sort_candidates(&mut candidates);
            candidates.truncate(query.k);
            return Ok(candidates);
        }

        if let (None, Some(sparse_query)) = (dense_opt, sparse_opt) {
            let (indices, values) = (
                sparse_query.vector.indices.clone(),
                sparse_query.vector.values.clone(),
            );
            let q = QueryPointsBuilder::new(&self.collection)
                .using(SPARSE_VECTOR)
                .query(VectorInput::new_sparse(indices, values))
                .filter(filter)
                .limit(k)
                .with_payload(true);
            let response = self.client.query(q).await.map_err(map)?;
            let mut candidates = Vec::new();
            for (rank, point) in ranked_arm(&scored_points(&response.result))
                .into_iter()
                .enumerate()
            {
                let rank = rank as u32;
                let scores = ArmScores {
                    dense: None,
                    sparse: Some(point.score),
                    fused: point.score,
                };
                let ranks = ArmRanks {
                    dense: None,
                    sparse: Some(rank),
                    fused: rank,
                };
                if let Some(c) = candidate_from(&point.payload, scores, ranks) {
                    candidates.push(c);
                }
            }
            sort_candidates(&mut candidates);
            candidates.truncate(query.k);
            return Ok(candidates);
        }

        let Some(dense_vec) = dense_opt else {
            return Ok(Vec::new());
        };
        let Some(sparse_query) = sparse_opt else {
            return Ok(Vec::new());
        };

        // Both arms present! ONE round trip with the Query API.
        let (sparse_indices, sparse_values) = (
            sparse_query.vector.indices.clone(),
            sparse_query.vector.values.clone(),
        );

        let per_arm_k = (query.per_arm_k.max(1) as u64).max(k);
        let is_rrf = matches!(query.fusion, PortFusion::Rrf);

        if is_rrf {
            let dense_q = with_params(
                QueryPointsBuilder::new(&self.collection)
                    .using(DENSE_VECTOR)
                    .query(Query::new_nearest(dense_vec.clone()))
                    .filter(filter.clone())
                    .limit(per_arm_k)
                    .with_payload(true),
                &dense_params,
            );

            let sparse_q = QueryPointsBuilder::new(&self.collection)
                .using(SPARSE_VECTOR)
                .query(VectorInput::new_sparse(
                    sparse_indices.clone(),
                    sparse_values.clone(),
                ))
                .filter(filter.clone())
                .limit(per_arm_k)
                .with_payload(false);

            let rrf_q = QueryPointsBuilder::new(&self.collection)
                .query(Query::new_fusion(QdrantFusion::Rrf))
                .add_prefetch(with_params(
                    PrefetchQueryBuilder::default()
                        .using(DENSE_VECTOR)
                        .query(Query::new_nearest(dense_vec.clone()))
                        .filter(filter.clone())
                        .limit(per_arm_k),
                    &dense_params,
                ))
                .add_prefetch(
                    PrefetchQueryBuilder::default()
                        .using(SPARSE_VECTOR)
                        .query(VectorInput::new_sparse(sparse_indices, sparse_values))
                        .filter(filter.clone())
                        .limit(per_arm_k),
                )
                .filter(filter)
                .limit(per_arm_k)
                .with_payload(true);

            let batch = QueryBatchPointsBuilder::new(
                &self.collection,
                vec![dense_q.build(), sparse_q.build(), rrf_q.build()],
            );
            let batch_resp = self.client.query_batch(batch).await.map_err(map)?;

            // Ranks come from [`ranked_arm`], not from the order the store
            // answered in: two ingests of the same corpus produce the same scores
            // but can hand back tied points in different orders, and RRF turns
            // that reshuffle into a different answer.
            let dense_map = batch_resp
                .result
                .first()
                .map(|dense_res| rank_map(&scored_points(&dense_res.result)))
                .unwrap_or_default();
            let sparse_map = batch_resp
                .result
                .get(1)
                .map(|sparse_res| rank_map(&scored_points(&sparse_res.result)))
                .unwrap_or_default();

            let mut candidates = Vec::new();
            if let Some(rrf_res) = batch_resp.result.get(2) {
                for p in &rrf_res.result {
                    let (dense_score, dense_rank) =
                        p.id.as_ref()
                            .and_then(|id| dense_map.get(id))
                            .map(|(s, r)| (Some(*s), Some(*r)))
                            .unwrap_or((None, None));
                    let (sparse_score, sparse_rank) =
                        p.id.as_ref()
                            .and_then(|id| sparse_map.get(id))
                            .map(|(s, r)| (Some(*s), Some(*r)))
                            .unwrap_or((None, None));

                    // The fused score is **Qdrant's own RRF score**, used as it
                    // comes back, and the order is the order Qdrant fused.
                    //
                    // The adapter used to re-sort that list with a k=60 RRF of
                    // its own (`1/(60 + rank + 1)` summed over the arms). A
                    // large k flattens the difference between rank 1 and rank
                    // 3, so a passage both arms put near the top no longer
                    // clearly beats one only a single arm liked. Measured on
                    // `miracl-id-200`, that re-sort cost 0.01 nDCG@10 and
                    // 0.017 MRR@10 — in this engine and in moco-kf's — and
                    // accounted for the whole significant MRR gap against it
                    // (`docs/kf-vplane-eval.md`). Qdrant's server-side RRF is
                    // `sum over arms of 1/(2 + rank)`, and it is the ranking
                    // that was measured.
                    //
                    // The per-arm scores and ranks stay: they are the
                    // provenance (`retrieval.denseRank`, `sparseRank`) and the
                    // only way to audit which arm liked what.
                    let scores = ArmScores {
                        dense: dense_score,
                        sparse: sparse_score,
                        fused: p.score,
                    };
                    let ranks = ArmRanks {
                        dense: dense_rank,
                        sparse: sparse_rank,
                        fused: 0,
                    };
                    if let Some(c) = candidate_from(&p.payload, scores, ranks) {
                        candidates.push(c);
                    }
                }
            }

            // `sort_candidates` orders by the fused score, so Qdrant's order
            // survives, and breaks the structural RRF ties (ranks (0,1) and
            // (1,0) sum to the same score) on the document id and chunk index
            // instead of on whatever order the store happened to answer in.
            sort_candidates(&mut candidates);
            candidates.truncate(query.k);
            Ok(candidates)
        } else {
            let dense_q = with_params(
                QueryPointsBuilder::new(&self.collection)
                    .using(DENSE_VECTOR)
                    .query(Query::new_nearest(dense_vec.clone()))
                    .filter(filter.clone())
                    .limit(k)
                    .with_payload(true),
                &dense_params,
            );

            let sparse_q = QueryPointsBuilder::new(&self.collection)
                .using(SPARSE_VECTOR)
                .query(VectorInput::new_sparse(sparse_indices, sparse_values))
                .filter(filter)
                .limit(k)
                .with_payload(true);

            let batch = QueryBatchPointsBuilder::new(
                &self.collection,
                vec![dense_q.build(), sparse_q.build()],
            );
            let batch_resp = self.client.query_batch(batch).await.map_err(map)?;

            let mut candidates = Vec::new();
            if let Some(dense_res) = batch_resp.result.first() {
                for (rank, point) in ranked_arm(&scored_points(&dense_res.result))
                    .into_iter()
                    .enumerate()
                {
                    let rank = rank as u32;
                    let scores = ArmScores {
                        dense: Some(point.score),
                        sparse: None,
                        fused: point.score,
                    };
                    let ranks = ArmRanks {
                        dense: Some(rank),
                        sparse: None,
                        fused: rank,
                    };
                    if let Some(c) = candidate_from(&point.payload, scores, ranks) {
                        candidates.push(c);
                    }
                }
            }
            if let Some(sparse_res) = batch_resp.result.get(1) {
                for (rank, point) in ranked_arm(&scored_points(&sparse_res.result))
                    .into_iter()
                    .enumerate()
                {
                    let rank = rank as u32;
                    let scores = ArmScores {
                        dense: None,
                        sparse: Some(point.score),
                        fused: point.score,
                    };
                    let ranks = ArmRanks {
                        dense: None,
                        sparse: Some(rank),
                        fused: rank,
                    };
                    if let Some(c) = candidate_from(&point.payload, scores, ranks) {
                        candidates.push(c);
                    }
                }
            }
            sort_candidates(&mut candidates);
            candidates.truncate(query.k);
            Ok(candidates)
        }
    }

    async fn fetch(
        &self,
        scope: &DocumentScope,
        ids: &[ChunkId],
    ) -> Result<Vec<Candidate>, PortError> {
        if ids.is_empty() || scope.document_ids().is_empty() {
            return Ok(Vec::new());
        }
        let filter = crate::payload::scoped_ids_filter(scope, ids);
        let limit = ids.len().min(u32::MAX as usize) as u32;
        let response = self
            .client
            .scroll(
                ScrollPointsBuilder::new(&self.collection)
                    .filter(filter)
                    .limit(limit)
                    .with_payload(true),
            )
            .await
            .map_err(map)?;

        let mut out = Vec::new();
        for p in response.result {
            if let Some(c) = candidate_from(&p.payload, ArmScores::default(), ArmRanks::default()) {
                out.push(c);
            }
        }
        out.sort_by(|a, b| a.unit_id.as_str().cmp(b.unit_id.as_str()));
        Ok(out)
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        document_id: &DocumentId,
        chunk_indices: &[u32],
    ) -> Result<Vec<Candidate>, PortError> {
        if chunk_indices.is_empty() || !scope.document_ids().contains(document_id) {
            return Ok(Vec::new());
        }

        let mut adjacent = HashSet::new();
        for &anchor in chunk_indices {
            if anchor > 0 {
                adjacent.insert(anchor - 1);
            }
            adjacent.insert(anchor + 1);
        }
        for &anchor in chunk_indices {
            adjacent.remove(&anchor);
        }
        if adjacent.is_empty() {
            return Ok(Vec::new());
        }

        let min_idx = *adjacent.iter().min().unwrap_or(&0);
        let max_idx = *adjacent.iter().max().unwrap_or(&0);

        let filter =
            crate::payload::chunk_range_filter(scope.tenant(), document_id, min_idx, max_idx);

        let limit = (max_idx - min_idx + 1).max(1);
        let response = self
            .client
            .scroll(
                ScrollPointsBuilder::new(&self.collection)
                    .filter(filter)
                    .limit(limit)
                    .with_payload(true),
            )
            .await
            .map_err(map)?;

        let mut out = Vec::new();
        for p in response.result {
            if let Some(c) = candidate_from(&p.payload, ArmScores::default(), ArmRanks::default()) {
                if adjacent.contains(&c.chunk_index) {
                    out.push(c);
                }
            }
        }
        out.sort_by_key(|c| c.chunk_index);
        Ok(out)
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        document_ids: &[DocumentId],
    ) -> Result<Vec<Candidate>, PortError> {
        let matching_docs: Vec<String> = document_ids
            .iter()
            .filter(|d| scope.document_ids().contains(d))
            .map(|d| d.as_str().to_string())
            .collect();
        if matching_docs.is_empty() {
            return Ok(Vec::new());
        }
        let filter = crate::payload::figures_filter(scope.tenant(), &matching_docs);
        let response = self
            .client
            .scroll(
                ScrollPointsBuilder::new(&self.collection)
                    .filter(filter)
                    .limit(500)
                    .with_payload(true),
            )
            .await
            .map_err(map)?;
        let mut out = Vec::new();
        for p in response.result {
            if let Some(c) = candidate_from(&p.payload, ArmScores::default(), ArmRanks::default()) {
                out.push(c);
            }
        }
        out.sort_by(|a, b| a.unit_id.as_str().cmp(b.unit_id.as_str()));
        Ok(out)
    }

    async fn ordered_chunks(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<Vec<Candidate>, PortError> {
        let filter = crate::payload::one_document_filter(tenant, document_id);
        let response = self
            .client
            .scroll(
                ScrollPointsBuilder::new(&self.collection)
                    .filter(filter)
                    .limit(2000)
                    .with_payload(true),
            )
            .await
            .map_err(map)?;
        let mut out = Vec::new();
        for p in response.result {
            if let Some(c) = candidate_from(&p.payload, ArmScores::default(), ArmRanks::default()) {
                out.push(c);
            }
        }
        out.sort_by_key(|c| c.chunk_index);
        Ok(out)
    }

    async fn health_check(&self) -> Result<(), PortError> {
        self.client.health_check().await.map_err(map).map(|_| ())
    }

    fn capabilities(&self) -> IndexCapabilities {
        IndexCapabilities {
            dense_dims: self.settings.dense_dims,
            sparse: Some(KeywordSemantics::AnyTerm),
            server_side_fusion: true,
            multivector: false,
            approximate_dense: true,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn point(id: u64, unit_id: &str, score: f32) -> ScoredPoint {
        ScoredPoint {
            id: PointId::from(id),
            unit_id: Some(unit_id.to_string()),
            score,
            payload: std::collections::HashMap::new(),
        }
    }

    fn order(points: &[ScoredPoint]) -> Vec<String> {
        ranked_arm(points)
            .into_iter()
            .map(|p| p.unit_id.clone().unwrap_or_default())
            .collect()
    }

    #[test]
    fn tied_scores_are_broken_by_unit_id_not_by_the_order_the_store_answered_in() {
        // The bug this fixes: with an 8-dim stub embedding a 19-chunk corpus has
        // 4- and 5-way score ties, and Qdrant hands tied points back in segment
        // order — which follows ingest order, which is not stable across runs.
        let a = point(1, "doc_0", 0.627);
        let b = point(2, "doc_1", 0.627);
        let c = point(3, "doc_2", 0.627);
        assert_eq!(
            order(&[a.clone(), b.clone(), c.clone()]),
            ["doc_0", "doc_1", "doc_2"]
        );
        assert_eq!(
            order(&[c.clone(), a.clone(), b.clone()]),
            ["doc_0", "doc_1", "doc_2"]
        );
        assert_eq!(
            order(&[b.clone(), c.clone(), a.clone()]),
            ["doc_0", "doc_1", "doc_2"]
        );
    }

    #[test]
    fn a_higher_score_always_beats_a_lower_one_however_the_ties_fall() {
        let points = [
            point(1, "doc_z", 0.1),
            point(2, "doc_a", 0.9),
            point(3, "doc_m", 0.1),
        ];
        assert_eq!(order(&points), ["doc_a", "doc_m", "doc_z"]);
    }

    #[test]
    fn ranks_are_the_sorted_positions_not_the_input_positions() {
        let map = rank_map(&[
            point(3, "doc_2", 0.5),
            point(1, "doc_0", 0.5),
            point(2, "doc_1", 0.9),
        ]);
        assert_eq!(map[&PointId::from(2)], (0.9, 0));
        assert_eq!(map[&PointId::from(1)], (0.5, 1));
        assert_eq!(map[&PointId::from(3)], (0.5, 2));
    }

    #[test]
    fn a_point_without_a_payload_keeps_the_stores_order() {
        // Nothing to break ties on, so the store's order is what it gets: worse
        // than a tie-break, never worse than the old behaviour.
        let points: Vec<ScoredPoint> = [point(7, "doc_1", 0.5), point(8, "doc_0", 0.5)]
            .into_iter()
            .map(|mut p| {
                p.unit_id = None;
                p
            })
            .collect();
        let ids: Vec<u64> = ranked_arm(&points)
            .into_iter()
            .map(|p| match p.id.point_id_options {
                Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => n,
                _ => 0,
            })
            .collect();
        assert_eq!(ids, vec![7, 8], "the order the store gave in");
    }

    #[test]
    fn the_exact_threshold_is_the_one_the_measurement_justifies() {
        // Below it exact search costs nothing measurable (2.38 vs 2.34 ms p50 at
        // 1.5k points); above 15k it starts to cost and HNSW recall falls away.
        assert_eq!(EXACT_SEARCH_THRESHOLD, 20_000);
    }
}
