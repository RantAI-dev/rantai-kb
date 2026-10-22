//! The in-memory reference index against the shared contract suite.
//!
//! The reference implementation has no excuse: every property either passes or
//! the suite itself is wrong. This test is what makes "run the contract suite
//! against your adapter" a checkable claim rather than an intention — the
//! adapter that defines the properties is also the first one held to them.

use kb_ports::contract::{self, ContractReport, Outcome};
use kb_ports::{PortError, RetrievalIndex};

use crate::in_memory_index::InMemoryIndex;

/// The fixture uses 8-dimensional dense vectors.
const DIMS: u32 = 8;

async fn fresh() -> Result<InMemoryIndex, PortError> {
    Ok(InMemoryIndex::new(DIMS))
}

/// Run the suite and assert every property passed. Returns the report so the
/// caller can print it.
async fn run_and_check() -> ContractReport {
    let report = contract::run_contract(fresh).await;
    println!(
        "RetrievalIndex contract — InMemoryIndex (reference)\n{}",
        report.summary()
    );
    let failures = report.failures();
    assert!(
        failures.is_empty(),
        "the reference index must pass every property; failures: {:?}",
        failures
            .iter()
            .map(|f| (f.name, format!("{:?}", f.outcome)))
            .collect::<Vec<_>>()
    );
    report
}

#[tokio::test]
async fn in_memory_index_passes_the_whole_contract() {
    let report = run_and_check().await;
    // The reference declares OR-of-terms keyword semantics, so the OR property
    // must actually run rather than skip. If it ever skips, the suite stopped
    // testing what we think it is testing.
    let skips: Vec<&str> = report
        .results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Skipped(_)))
        .map(|r| r.name)
        .collect();
    assert!(
        skips.is_empty(),
        "the reference index skipped properties it should have run: {skips:?}"
    );
}

#[tokio::test]
async fn every_property_runs_and_passes() {
    // Named individually so a failure report says which property broke rather
    // than only that "the contract failed".
    let report = run_and_check().await;
    assert_eq!(report.results.len(), contract::PROPERTIES.len());
    for result in &report.results {
        assert!(
            matches!(result.outcome, Outcome::Pass),
            "{} did not pass: {:?}",
            result.name,
            result.outcome
        );
    }
}

#[tokio::test]
async fn an_empty_scope_returns_nothing_even_with_a_matching_query() {
    // Spelled out separately because it is the one shape a caller can get wrong
    // silently: a search with no visible documents must be empty, not "the best
    // match in the collection".
    let index = InMemoryIndex::new(DIMS);
    let scope = crate::fakes::make_fake_scope(kb_ports::TenantId::new_unchecked("t"), vec![]);
    let out = index
        .search(
            &kb_ports::HybridQuery {
                dense: Some(vec![1.0; DIMS as usize]),
                sparse: None,
                per_arm_k: 5,
                k: 5,
                fusion: kb_ports::Fusion::default(),
            },
            &scope,
        )
        .await
        .unwrap_or_else(|e| unreachable!("an empty scope must not error, got: {e}"));
    assert!(out.is_empty(), "an empty scope returned {} rows", out.len());
}

/// The fused score is Qdrant's RRF sum — `1/(2 + rank)` per arm, rank
/// 0-based — not the RRF paper's `k = 60` re-sort.
///
/// The contract suite (property 7) pins the number too, but it is about the
/// *merge*; this one is about the constant, so it is spelled out: a candidate
/// both arms rank first scores `1/2 + 1/2 = 1.0`, one only the dense arm
/// ranks first scores `1/2`, and the both-arms candidate wins. Under the old
/// `k = 60` re-sort the same two candidates scored `2/61 = 0.0328` and
/// `1/61 = 0.0164`: the same order, a different scale — and the scale is
/// what every adapter now has to reproduce exactly
/// (`docs/kf-vplane-eval.md`).
#[tokio::test]
async fn the_fused_score_is_qdrants_rrf_sum() {
    use kb_ports::{ChunkId, ChunkType, DocumentId, HybridQuery, IndexUnit, KeywordQuery};

    let index = InMemoryIndex::new(2);
    let tenant = kb_ports::TenantId::new_unchecked("t");
    let doc_a = DocumentId::new_unchecked("doc-a");
    let doc_b = DocumentId::new_unchecked("doc-b");

    let unit = |doc: &DocumentId, content: &str, dense: Vec<f32>| IndexUnit {
        id: ChunkId::new_unchecked(format!("{}_0", doc.as_str())),
        document_id: doc.clone(),
        tenant: tenant.clone(),
        knowledge_base_id: None,
        category: None,
        chunk_index: 0,
        page: None,
        chunk_type: ChunkType::Text,
        content: content.to_string(),
        contextual_prefix: None,
        metadata: serde_json::json!({}),
        dense,
        sparse: kb_core::sparse::encode_document(content),
    };

    // The dense arm's best hit is doc-a (the query is its own vector); the
    // keyword arm's best hit is doc-b, the only unit with the term in it.
    index
        .upsert(vec![
            unit(&doc_a, "alpha", vec![1.0, 0.0]),
            unit(&doc_b, "candi", vec![0.0, 1.0]),
        ])
        .await
        .unwrap_or_else(|e| unreachable!("upsert failed: {e}"));

    let scope = crate::fakes::make_fake_scope(tenant, vec![doc_a, doc_b]);
    let rows = index
        .search(
            &HybridQuery {
                dense: Some(vec![1.0, 0.0]),
                sparse: Some(KeywordQuery::from_text("candi")),
                per_arm_k: 10,
                k: 10,
                fusion: kb_ports::Fusion::Rrf,
            },
            &scope,
        )
        .await
        .unwrap_or_else(|e| unreachable!("search failed: {e}"));

    assert_eq!(rows.len(), 2, "both units must come back");
    // Both arms found doc-b (dense rank 1, keyword rank 0): it outranks the
    // dense arm's own top hit, even though its dense score is 0.
    assert_eq!(rows[0].unit_id.as_str(), "doc-b_0");
    assert!(
        (rows[0].scores.fused - (1.0 / 3.0 + 0.5)).abs() < 1e-6,
        "{:?}",
        rows[0].scores
    );
    assert_eq!(rows[0].ranks.dense, Some(1));
    assert_eq!(rows[0].ranks.sparse, Some(0));
    // Only the dense arm found doc-a: one contribution.
    assert_eq!(rows[1].unit_id.as_str(), "doc-a_0");
    assert!(
        (rows[1].scores.fused - 0.5).abs() < 1e-6,
        "{:?}",
        rows[1].scores
    );
    assert_eq!(rows[1].ranks.sparse, None);
}

/// A deliberately broken adapter: it answers every read from the whole index,
/// ignoring the scope. This is the `bm25_search_global` bug wearing a new trait.
///
/// If this test ever passes, the contract suite is not testing containment and
/// the green results above mean nothing.
struct LeakyIndex(InMemoryIndex);

#[async_trait::async_trait]
impl RetrievalIndex for LeakyIndex {
    async fn upsert(&self, units: Vec<kb_ports::IndexUnit>) -> Result<(), PortError> {
        self.0.upsert(units).await
    }
    async fn delete_document(
        &self,
        tenant: &kb_ports::TenantId,
        document_id: &kb_ports::DocumentId,
    ) -> Result<u64, PortError> {
        self.0.delete_document(tenant, document_id).await
    }
    async fn search(
        &self,
        query: &kb_ports::HybridQuery,
        _scope: &kb_ports::DocumentScope,
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        // A scope that sees everything is the cheapest way to model "the store
        // filtered nothing and the caller did not check".
        let everything = crate::fakes::make_fake_scope(
            kb_ports::TenantId::new_unchecked("contract-tenant-a"),
            vec![
                kb_ports::DocumentId::new_unchecked("doc-a1"),
                kb_ports::DocumentId::new_unchecked("doc-a2"),
                kb_ports::DocumentId::new_unchecked("doc-a3"),
                kb_ports::DocumentId::new_unchecked("doc-a4"),
            ],
        );
        self.0.search(query, &everything).await
    }
    async fn fetch(
        &self,
        _scope: &kb_ports::DocumentScope,
        ids: &[kb_ports::ChunkId],
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        let everything = crate::fakes::make_fake_scope(
            kb_ports::TenantId::new_unchecked("contract-tenant-a"),
            vec![
                kb_ports::DocumentId::new_unchecked("doc-a1"),
                kb_ports::DocumentId::new_unchecked("doc-a2"),
                kb_ports::DocumentId::new_unchecked("doc-a3"),
                kb_ports::DocumentId::new_unchecked("doc-a4"),
            ],
        );
        self.0.fetch(&everything, ids).await
    }
    async fn neighbours(
        &self,
        _scope: &kb_ports::DocumentScope,
        document_id: &kb_ports::DocumentId,
        chunk_indices: &[u32],
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        let everything = crate::fakes::make_fake_scope(
            kb_ports::TenantId::new_unchecked("contract-tenant-a"),
            vec![
                kb_ports::DocumentId::new_unchecked("doc-a1"),
                kb_ports::DocumentId::new_unchecked("doc-a2"),
                kb_ports::DocumentId::new_unchecked("doc-a3"),
                kb_ports::DocumentId::new_unchecked("doc-a4"),
            ],
        );
        self.0
            .neighbours(&everything, document_id, chunk_indices)
            .await
    }
    async fn figures(
        &self,
        _scope: &kb_ports::DocumentScope,
        document_ids: &[kb_ports::DocumentId],
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        let everything = crate::fakes::make_fake_scope(
            kb_ports::TenantId::new_unchecked("contract-tenant-a"),
            vec![
                kb_ports::DocumentId::new_unchecked("doc-a1"),
                kb_ports::DocumentId::new_unchecked("doc-a2"),
                kb_ports::DocumentId::new_unchecked("doc-a3"),
                kb_ports::DocumentId::new_unchecked("doc-a4"),
            ],
        );
        self.0.figures(&everything, document_ids).await
    }
    async fn ordered_chunks(
        &self,
        tenant: &kb_ports::TenantId,
        document_id: &kb_ports::DocumentId,
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        self.0.ordered_chunks(tenant, document_id).await
    }
    fn capabilities(&self) -> kb_ports::IndexCapabilities {
        self.0.capabilities()
    }
}

#[tokio::test]
async fn the_contract_suite_actually_catches_an_unscoped_adapter() {
    let report =
        contract::run_contract(|| async { Ok(LeakyIndex(InMemoryIndex::new(DIMS))) }).await;
    println!(
        "RetrievalIndex contract — LeakyIndex (deliberately broken)\n{}",
        report.summary()
    );
    let failed: Vec<&str> = report.failures().iter().map(|f| f.name).collect();
    assert!(
        failed.contains(&"scope_containment"),
        "an adapter that ignores the scope must fail scope_containment; it failed {failed:?}"
    );
}

/// An adapter that declares AND keyword semantics without implementing it.
///
/// The suite must notice: it either has to fail the OR property or, if the
/// property is skipped on the declared capability, that skip has to be the
/// reason the property cannot run. This pins the honesty of the capability
/// flag, which is what stops a future Qdrant adapter from declaring the wrong
/// thing to make a test go green.
struct AndSemanticsClaimer(InMemoryIndex);

#[async_trait::async_trait]
impl RetrievalIndex for AndSemanticsClaimer {
    async fn upsert(&self, units: Vec<kb_ports::IndexUnit>) -> Result<(), PortError> {
        self.0.upsert(units).await
    }
    async fn delete_document(
        &self,
        tenant: &kb_ports::TenantId,
        document_id: &kb_ports::DocumentId,
    ) -> Result<u64, PortError> {
        self.0.delete_document(tenant, document_id).await
    }
    async fn search(
        &self,
        query: &kb_ports::HybridQuery,
        scope: &kb_ports::DocumentScope,
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        self.0.search(query, scope).await
    }
    async fn fetch(
        &self,
        scope: &kb_ports::DocumentScope,
        ids: &[kb_ports::ChunkId],
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        self.0.fetch(scope, ids).await
    }
    async fn neighbours(
        &self,
        scope: &kb_ports::DocumentScope,
        document_id: &kb_ports::DocumentId,
        chunk_indices: &[u32],
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        self.0.neighbours(scope, document_id, chunk_indices).await
    }
    async fn figures(
        &self,
        scope: &kb_ports::DocumentScope,
        document_ids: &[kb_ports::DocumentId],
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        self.0.figures(scope, document_ids).await
    }
    async fn ordered_chunks(
        &self,
        tenant: &kb_ports::TenantId,
        document_id: &kb_ports::DocumentId,
    ) -> Result<Vec<kb_ports::Candidate>, PortError> {
        self.0.ordered_chunks(tenant, document_id).await
    }
    fn capabilities(&self) -> kb_ports::IndexCapabilities {
        kb_ports::IndexCapabilities {
            sparse: Some(kb_ports::KeywordSemantics::AllTerms),
            ..self.0.capabilities()
        }
    }
}

#[tokio::test]
async fn declaring_all_terms_skips_the_or_property_with_a_stated_reason() {
    let report =
        contract::run_contract(|| async { Ok(AndSemanticsClaimer(InMemoryIndex::new(DIMS))) })
            .await;
    println!(
        "RetrievalIndex contract — AndSemanticsClaimer\n{}",
        report.summary()
    );
    let skips = report.skips();
    let or_skip = skips.iter().find(|s| s.name == "keyword_or_semantics");
    assert!(
        or_skip.is_some(),
        "the OR property must be skipped for an AllTerms adapter; report:\n{}",
        report.summary()
    );
    match &or_skip.map(|s| &s.outcome) {
        Some(Outcome::Skipped(reason)) => {
            assert!(
                reason.contains("AllTerms") && reason.contains("AND"),
                "the skip reason must name the semantics and the AND behaviour, got: {reason}"
            );
        }
        other => assert!(
            matches!(other, Some(Outcome::Skipped(_))),
            "expected a skip for keyword_or_semantics, got {other:?}"
        ),
    }
    // The other properties still run: skipping one capability is not a licence
    // to skip the suite.
    assert_eq!(report.failures().len(), 0, "{:?}", report.failures());
    assert!(
        report.results.len() - skips.len() >= 7,
        "every property but the skipped one must still have run"
    );
}
