//! Per-arm provenance in `/v1/search` (step S.1c, part A).
//!
//! Three things have to be true for the red-team matrix to be able to attribute
//! a hit to an arm, and all three are asserted here against the real router:
//!
//! * a chunk only the keyword arm returned says `["sparse"]`;
//! * a chunk both arms returned says `["dense","sparse"]`, with each arm's own
//!   rank;
//! * a chunk the neighbour weave added says `["neighbour"]` and carries no
//!   score, because nothing scored it.
//!
//! Additivity is asserted too: `retrieval` is the only new key, and every field
//! Bun emits still serialises exactly as before.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kb_engine::EngineSettings;
use kb_engine::fakes::{FakeChunkStore, FakeReranker, make_fake_hit};
use kb_ports::{
    ArmRanks, ArmScores, Candidate, ChunkId, DocumentId, DocumentScope, HybridQuery,
    IndexCapabilities, IndexUnit, PortError, RetrievalIndex, TenantId,
};
use kb_server::dto::SearchResponseDto;
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};
use tower::ServiceExt;

use harness::*;

const TENANT: &str = "tenant-a";
const DOC: &str = "doc-1";

/// The engine settings the shared harness runs with.
fn engine_settings() -> EngineSettings {
    EngineSettings {
        candidate_pool_size: 20,
        rerank_candidate_limit: 10,
        rerank_char_budget: 350,
        rerank_timeout_ms: 3000,
        rerank_final_k: 10,
        max_chunks: 10,
        min_similarity: 0.30,
        neighbour_window: 1,
        figure_limit: 3,
        expansion_enabled: false,
        expansion_paraphrases: 3,
        hybrid_bm25_enabled: true,
        rerank_enabled: false,
    }
}

/// An index that returns what the wrapped one returns with every arm stripped,
/// which is what an adapter that reports neither score nor rank looks like.
struct NoArmIndex(FakeChunkStore);

#[async_trait::async_trait]
impl RetrievalIndex for NoArmIndex {
    async fn search(
        &self,
        query: &HybridQuery,
        scope: &DocumentScope,
    ) -> Result<Vec<Candidate>, PortError> {
        let mut candidates = self.0.search(query, scope).await?;
        for candidate in &mut candidates {
            candidate.scores = ArmScores {
                dense: None,
                sparse: None,
                fused: candidate.scores.fused,
            };
            candidate.ranks = ArmRanks {
                dense: None,
                sparse: None,
                fused: candidate.ranks.fused,
            };
        }
        Ok(candidates)
    }

    async fn fetch(
        &self,
        scope: &DocumentScope,
        ids: &[ChunkId],
    ) -> Result<Vec<Candidate>, PortError> {
        self.0.fetch(scope, ids).await
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        document_id: &DocumentId,
        chunk_indices: &[u32],
    ) -> Result<Vec<Candidate>, PortError> {
        self.0.neighbours(scope, document_id, chunk_indices).await
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        document_ids: &[DocumentId],
    ) -> Result<Vec<Candidate>, PortError> {
        self.0.figures(scope, document_ids).await
    }

    async fn ordered_chunks(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<Vec<Candidate>, PortError> {
        self.0.ordered_chunks(tenant, document_id).await
    }

    async fn upsert(&self, units: Vec<IndexUnit>) -> Result<(), PortError> {
        self.0.upsert(units).await
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<u64, PortError> {
        self.0.delete_document(tenant, document_id).await
    }

    fn capabilities(&self) -> IndexCapabilities {
        self.0.capabilities()
    }
}

/// Seed one in-scope document whose chunk 0 is a vector hit, chunk 1 is a
/// keyword hit, chunk 2 is a hit both arms return, and chunk 3 is the neighbour
/// the weave adds. Figures are left empty: this is about the search arms.
fn seeded() -> Harness {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta(DOC, "Provenance"));

    // The same unit from both arms, each at its own arm rank: if only one rank
    // survived the fusion the test would see it here.
    let mut dense_side = make_fake_hit("c-both", DOC, "Provenance", "both arms", 2, 0.7);
    dense_side.rank = 3;
    let mut sparse_side = make_fake_hit("c-both", DOC, "Provenance", "both arms", 2, 0.7);
    sparse_side.rank = 7;
    h.chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "c-vec",
        DOC,
        "Provenance",
        "vector only",
        0,
        0.9,
    ));
    h.chunks.bm25_hits.write().unwrap().push(make_fake_hit(
        "c-key",
        DOC,
        "Provenance",
        "keyword only",
        1,
        0.8,
    ));
    h.chunks.vector_hits.write().unwrap().push(dense_side);
    h.chunks.bm25_hits.write().unwrap().push(sparse_side);
    // Woven in as the neighbour of the rank-1 hit (chunk 0): index ±1 is 1,
    // which is already a search hit, so this one is the neighbour of chunk 1.
    h.chunks.neighbour_hits.write().unwrap().push(make_fake_hit(
        "c-neighbour",
        DOC,
        "Provenance",
        "adjacent passage",
        3,
        0.0,
    ));
    h
}

async fn search(h: &Harness) -> serde_json::Value {
    let response = h
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key(TENANT),
            serde_json::json!({"query": "anything", "maxChunks": 10}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

fn chunk(body: &serde_json::Value, id: &str) -> serde_json::Value {
    // `id` is the bare ident the fake store keys on; the wire carries Bun's
    // `document_chunk:⟨…⟩` form.
    let wire_id = kb_server::dto::chunk_id_for_client(id);
    body["chunks"]
        .as_array()
        .expect("chunks array")
        .iter()
        .find(|c| c["id"] == wire_id)
        .unwrap_or_else(|| panic!("no chunk {id} in {}", body["chunks"]))
        .clone()
}

fn arms(value: &serde_json::Value) -> Vec<String> {
    value["retrieval"]["arms"]
        .as_array()
        .expect("retrieval.arms")
        .iter()
        .map(|a| a.as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn a_keyword_only_hit_says_the_sparse_arm_and_nothing_else() {
    let body = search(&seeded()).await;
    let hit = chunk(&body, "c-key");
    assert_eq!(arms(&hit), vec!["sparse"]);
    assert!(hit["retrieval"]["denseRank"].is_null());
    assert!(
        hit["retrieval"]["sparseRank"].is_number(),
        "the sparse arm's own rank is reported: {}",
        hit["retrieval"]
    );
    // A keyword hit still looks like a keyword hit to an old client: no
    // chunkType, no page, no assetKey.
    assert!(hit.get("chunkType").is_none());
    assert!(hit.get("page").is_none());
    assert!(hit.get("assetKey").is_none());
}

#[tokio::test]
async fn a_hit_both_arms_returned_says_both_with_each_arms_rank() {
    let body = search(&seeded()).await;
    let hit = chunk(&body, "c-both");
    assert_eq!(arms(&hit), vec!["dense", "sparse"]);
    assert_eq!(hit["retrieval"]["denseRank"], 3);
    assert_eq!(hit["retrieval"]["sparseRank"], 7);
    assert!(
        hit["retrieval"]["fusedScore"].is_number(),
        "the fused score the answer was ordered by: {}",
        hit["retrieval"]
    );
}

#[tokio::test]
async fn a_vector_only_hit_says_the_dense_arm() {
    let body = search(&seeded()).await;
    let hit = chunk(&body, "c-vec");
    assert_eq!(arms(&hit), vec!["dense"]);
    assert!(hit["retrieval"]["sparseRank"].is_null());
}

#[tokio::test]
async fn a_woven_neighbour_says_neighbour_and_no_score() {
    let body = search(&seeded()).await;
    let hit = chunk(&body, "c-neighbour");
    assert_eq!(arms(&hit), vec!["neighbour"]);
    // Nothing ranked it, so nothing is reported about a score: reporting 0.0
    // would read like a score of zero.
    assert!(hit["retrieval"].get("fusedScore").is_none());
    assert!(hit["retrieval"].get("denseRank").is_none());
    assert!(hit["retrieval"].get("sparseRank").is_none());
}

#[tokio::test]
async fn retrieval_is_the_only_new_key_and_the_old_fields_are_unchanged() {
    let h = seeded();
    let body = search(&h).await;
    let vector_hit = chunk(&body, "c-vec");

    // Exactly the Bun key set plus `retrieval`, nothing renamed and nothing
    // dropped. Bun emits no `retrieval`, so this is the one difference.
    let mut keys: Vec<&str> = vector_hit
        .as_object()
        .expect("chunk object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "assetKey",
            "categories",
            "chunkType",
            "content",
            "documentId",
            "documentTitle",
            "id",
            "page",
            "retrieval",
            "section",
            "similarity",
            "subcategory",
        ],
        "vector-hit keys changed: {}",
        serde_json::to_string(&vector_hit).unwrap()
    );
    assert_eq!(vector_hit["chunkType"], "text");
    assert_eq!(vector_hit["page"], serde_json::Value::Null);
    assert_eq!(vector_hit["assetKey"], serde_json::Value::Null);
    assert_eq!(vector_hit["similarity"], 0.9);
    assert_eq!(vector_hit["documentTitle"], "Provenance");
}

#[tokio::test]
async fn the_response_deserialises_into_the_dto_with_its_own_type() {
    let h = seeded();
    let body = search(&h).await;
    let parsed: SearchResponseDto = serde_json::from_value(body).expect("SearchResponseDto");
    let chunks = parsed.chunks.expect("chunks");
    let keyword = chunks
        .iter()
        .find(|c| c.id == kb_server::dto::chunk_id_for_client("c-key"))
        .expect("the keyword hit");
    let retrieval = keyword
        .retrieval
        .as_ref()
        .expect("a keyword hit carries provenance");
    assert_eq!(
        retrieval.arms,
        vec![kb_ports::ProvenanceArm::Sparse],
        "typed round trip keeps the arm vocabulary"
    );
    assert!(retrieval.fused_score.is_some());
    assert!(retrieval.dense_rank.is_none());
}

#[tokio::test]
async fn a_hit_from_an_adapter_that_reports_no_arm_omits_the_field() {
    // The fake always reports the dense arm for a vector hit, so the only way to
    // model an adapter that fills neither score nor rank is to strip them behind
    // the index. An absent `retrieval` is the honest answer there: a caller must
    // be able to tell "no arms reported" from "old server".
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta(DOC, "Provenance"));
    h.chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "c-unscored",
        DOC,
        "Provenance",
        "no arm reported",
        0,
        0.9,
    ));
    let state = AppState::from_parts(
        StateParts {
            documents: Arc::new(h.documents.clone()),
            index: Arc::new(NoArmIndex(h.chunks.clone())),
            entities: Arc::new(h.entities.clone()),
            graph: None,
            graph_arm: None,
            blob: Arc::new(h.blobs.clone()),
            embedder: Arc::new(h.embedder.clone()),
            jobs: Arc::new(h.jobs.clone()),
            events: EventBus::new(),
            version: "test".to_string(),
        },
        Arc::new(FakeReranker::default()),
        engine_settings(),
    );
    let response = kb_server::build_router(state)
        .oneshot(
            Request::builder()
                .uri("/v1/search")
                .method("POST")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", tenant_key(TENANT)))
                .body(Body::from(
                    serde_json::json!({"query": "anything", "maxChunks": 10}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let hit = chunk(&body, "c-unscored");
    assert!(
        hit.get("retrieval").is_none(),
        "no arm reported means no provenance, not an empty list: {}",
        hit
    );
}
