//! `RetrievalIndex` implementation for Qdrant.

use std::collections::{HashMap, HashSet};

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
    UpsertPointsBuilder, Vector, VectorInput,
};

use crate::ids::point_id;
use crate::payload::{candidate_from, unit_payload};
use crate::settings::{DENSE_VECTOR, QdrantSettings, SPARSE_VECTOR, map};

/// The Qdrant adapter for `RetrievalIndex`.
#[derive(Clone)]
pub struct QdrantIndex {
    client: Qdrant,
    collection: String,
    settings: QdrantSettings,
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

        if let (Some(dense_vec), None) = (dense_opt, sparse_opt) {
            let q = QueryPointsBuilder::new(&self.collection)
                .using(DENSE_VECTOR)
                .query(Query::new_nearest(dense_vec.clone()))
                .filter(filter)
                .limit(k)
                .with_payload(true);
            let response = self.client.query(q).await.map_err(map)?;
            let mut candidates = Vec::new();
            for (rank, p) in response.result.into_iter().enumerate() {
                let scores = ArmScores {
                    dense: Some(p.score),
                    sparse: None,
                    fused: p.score,
                };
                let ranks = ArmRanks {
                    dense: Some(rank as u32),
                    sparse: None,
                    fused: rank as u32,
                };
                if let Some(c) = candidate_from(&p.payload, scores, ranks) {
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
            for (rank, p) in response.result.into_iter().enumerate() {
                let scores = ArmScores {
                    dense: None,
                    sparse: Some(p.score),
                    fused: p.score,
                };
                let ranks = ArmRanks {
                    dense: None,
                    sparse: Some(rank as u32),
                    fused: rank as u32,
                };
                if let Some(c) = candidate_from(&p.payload, scores, ranks) {
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
        let is_rrf = matches!(query.fusion, PortFusion::Rrf { .. });

        if is_rrf {
            let dense_q = QueryPointsBuilder::new(&self.collection)
                .using(DENSE_VECTOR)
                .query(Query::new_nearest(dense_vec.clone()))
                .filter(filter.clone())
                .limit(per_arm_k)
                .with_payload(false);

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
                .add_prefetch(
                    PrefetchQueryBuilder::default()
                        .using(DENSE_VECTOR)
                        .query(Query::new_nearest(dense_vec.clone()))
                        .filter(filter.clone())
                        .limit(per_arm_k),
                )
                .add_prefetch(
                    PrefetchQueryBuilder::default()
                        .using(SPARSE_VECTOR)
                        .query(VectorInput::new_sparse(sparse_indices, sparse_values))
                        .filter(filter.clone())
                        .limit(per_arm_k),
                )
                .filter(filter)
                .limit(k)
                .with_payload(true);

            let batch = QueryBatchPointsBuilder::new(
                &self.collection,
                vec![dense_q.build(), sparse_q.build(), rrf_q.build()],
            );
            let batch_resp = self.client.query_batch(batch).await.map_err(map)?;

            let mut dense_map: HashMap<PointId, (f32, u32)> = HashMap::new();
            if let Some(dense_res) = batch_resp.result.first() {
                for (rank, p) in dense_res.result.iter().enumerate() {
                    if let Some(id) = &p.id {
                        dense_map.insert(id.clone(), (p.score, rank as u32));
                    }
                }
            }

            let mut sparse_map: HashMap<PointId, (f32, u32)> = HashMap::new();
            if let Some(sparse_res) = batch_resp.result.get(1) {
                for (rank, p) in sparse_res.result.iter().enumerate() {
                    if let Some(id) = &p.id {
                        sparse_map.insert(id.clone(), (p.score, rank as u32));
                    }
                }
            }

            let rrf_k = match query.fusion {
                PortFusion::Rrf { k } => k,
                PortFusion::None => 60,
            };

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

                    let mut fused = 0.0f32;
                    if let Some(r) = dense_rank {
                        fused += 1.0 / (rrf_k as f32 + r as f32 + 1.0);
                    }
                    if let Some(r) = sparse_rank {
                        fused += 1.0 / (rrf_k as f32 + r as f32 + 1.0);
                    }
                    if dense_rank.is_none() && sparse_rank.is_none() {
                        fused = p.score;
                    }

                    let scores = ArmScores {
                        dense: dense_score,
                        sparse: sparse_score,
                        fused,
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

            sort_candidates(&mut candidates);
            candidates.truncate(query.k);
            Ok(candidates)
        } else {
            let dense_q = QueryPointsBuilder::new(&self.collection)
                .using(DENSE_VECTOR)
                .query(Query::new_nearest(dense_vec.clone()))
                .filter(filter.clone())
                .limit(k)
                .with_payload(true);

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
                for (rank, p) in dense_res.result.iter().enumerate() {
                    let scores = ArmScores {
                        dense: Some(p.score),
                        sparse: None,
                        fused: p.score,
                    };
                    let ranks = ArmRanks {
                        dense: Some(rank as u32),
                        sparse: None,
                        fused: rank as u32,
                    };
                    if let Some(c) = candidate_from(&p.payload, scores, ranks) {
                        candidates.push(c);
                    }
                }
            }
            if let Some(sparse_res) = batch_resp.result.get(1) {
                for (rank, p) in sparse_res.result.iter().enumerate() {
                    let scores = ArmScores {
                        dense: None,
                        sparse: Some(p.score),
                        fused: p.score,
                    };
                    let ranks = ArmRanks {
                        dense: None,
                        sparse: Some(rank as u32),
                        fused: rank as u32,
                    };
                    if let Some(c) = candidate_from(&p.payload, scores, ranks) {
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
