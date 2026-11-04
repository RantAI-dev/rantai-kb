//! The in-memory graph store against the shared contract suite.
//!
//! The reference implementation has no excuse: every property either passes or the
//! suite itself is wrong. This test is what makes "run the contract suite against
//! your adapter" a checkable claim rather than an intention — the crate that
//! defines the properties is also the first one held to them.
//!
//! It also holds a deliberately broken adapter to the same suite, because a
//! contract suite nobody has seen fail is indistinguishable from a contract suite
//! that cannot fail. `LeakyGraphStore` filters *results* but not *edges*: the exact
//! bug the suite was written for, and the one an ordinary unit test cannot see,
//! because every individual read looks correct.

use async_trait::async_trait;
use kb_ports::graph_contract::{self as contract, ContractReport, Outcome};
use kb_ports::{
    ChunkId, DocumentId, DocumentScope, EntityUpsert, GraphStore, PortError, RelationUpsert,
    TenantGraph, TenantId,
};

use crate::in_memory_graph_store::InMemoryGraphStore;

async fn fresh() -> Result<InMemoryGraphStore, PortError> {
    Ok(InMemoryGraphStore::new())
}

async fn run_and_check() -> ContractReport {
    let report = contract::run_contract(fresh).await;
    println!(
        "GraphStore contract — InMemoryGraphStore (reference)\n{}",
        report.summary()
    );
    let failures = report.failures();
    assert!(
        failures.is_empty(),
        "the reference store must pass every property; failures: {:?}",
        failures
            .iter()
            .map(|f| (f.name, format!("{:?}", f.outcome)))
            .collect::<Vec<_>>()
    );
    report
}

#[tokio::test]
async fn in_memory_graph_store_passes_the_whole_contract() {
    let report = run_and_check().await;
    let skips: Vec<&str> = report
        .results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Skipped(_)))
        .map(|r| r.name)
        .collect();
    assert!(
        skips.is_empty(),
        "the reference store skipped properties it should have run: {skips:?}"
    );
}

#[tokio::test]
async fn every_property_runs_and_passes() {
    let report = run_and_check().await;
    let names: Vec<&str> = report.results.iter().map(|r| r.name).collect();
    assert_eq!(
        names,
        contract::PROPERTIES,
        "every declared property must run, so a property that stops running is visible"
    );
}

/// An adapter that filters results but not edges.
///
/// It does the tempting thing: it scopes the mentions, but it builds the CSR from
/// the tenant's whole relation list. Every read that returns mentions is correct,
/// every read that returns keys is correct, and the graph still routes through a
/// document the caller cannot see — so the caller learns the shape of a document
/// it was never shown.
struct LeakyGraphStore(InMemoryGraphStore);

#[async_trait]
impl GraphStore for LeakyGraphStore {
    async fn upsert_entities(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        entities: Vec<EntityUpsert>,
    ) -> Result<(), PortError> {
        self.0.upsert_entities(tenant, document_id, entities).await
    }

    async fn relate(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        relations: Vec<RelationUpsert>,
    ) -> Result<(), PortError> {
        self.0.relate(tenant, document_id, relations).await
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<(), PortError> {
        self.0.delete_document(tenant, document_id).await
    }

    /// The leak: the scope is widened to the whole tenant before delegating, so the
    /// CSR is built from every relation in the tenant.
    async fn load_tenant_graph(&self, scope: &DocumentScope) -> Result<TenantGraph, PortError> {
        let widened = crate::fakes::make_fake_scope(
            scope.tenant().clone(),
            self.0.known_documents(scope.tenant()),
        );
        self.0.load_tenant_graph(&widened).await
    }

    async fn graph_version(&self, tenant: &TenantId) -> Result<u64, PortError> {
        self.0.graph_version(tenant).await
    }

    async fn match_entities(
        &self,
        scope: &DocumentScope,
        query_terms: &[String],
    ) -> Result<Vec<(String, f32)>, PortError> {
        self.0.match_entities(scope, query_terms).await
    }

    async fn entity_mentions(
        &self,
        scope: &DocumentScope,
        key: &str,
    ) -> Result<Vec<ChunkId>, PortError> {
        self.0.entity_mentions(scope, key).await
    }
}

#[tokio::test]
async fn the_contract_suite_catches_an_adapter_that_leaks_through_the_graph() {
    let report =
        contract::run_contract(|| async { Ok(LeakyGraphStore(InMemoryGraphStore::new())) }).await;
    println!(
        "GraphStore contract — LeakyGraphStore (deliberately broken)\n{}",
        report.summary()
    );
    let failed: Vec<&str> = report.failures().iter().map(|f| f.name).collect();
    assert!(
        failed.contains(&"no_path_through_an_out_of_scope_document")
            || failed.contains(&"scope_containment"),
        "an adapter that scopes results but not edges must fail the suite; it failed {failed:?}"
    );
}

/// An adapter whose writes do not bump the version counter.
///
/// Nothing about this looks wrong from the caller's side: the reads are all correct.
/// The failure is invisible until a cached CSR is served after an ingest, and then
/// it is invisible as *this*, which is why the counter is a contract property rather
/// than a comment.
struct UnversionedGraphStore(InMemoryGraphStore);

#[async_trait]
impl GraphStore for UnversionedGraphStore {
    async fn upsert_entities(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        entities: Vec<EntityUpsert>,
    ) -> Result<(), PortError> {
        self.0.upsert_entities(tenant, document_id, entities).await
    }

    async fn relate(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
        relations: Vec<RelationUpsert>,
    ) -> Result<(), PortError> {
        self.0.relate(tenant, document_id, relations).await
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<(), PortError> {
        self.0.delete_document(tenant, document_id).await
    }

    async fn load_tenant_graph(&self, scope: &DocumentScope) -> Result<TenantGraph, PortError> {
        self.0.load_tenant_graph(scope).await
    }

    /// Always zero: a counter that never moves.
    async fn graph_version(&self, _tenant: &TenantId) -> Result<u64, PortError> {
        Ok(0)
    }

    async fn match_entities(
        &self,
        scope: &DocumentScope,
        query_terms: &[String],
    ) -> Result<Vec<(String, f32)>, PortError> {
        self.0.match_entities(scope, query_terms).await
    }

    async fn entity_mentions(
        &self,
        scope: &DocumentScope,
        key: &str,
    ) -> Result<Vec<ChunkId>, PortError> {
        self.0.entity_mentions(scope, key).await
    }
}

#[tokio::test]
async fn the_contract_suite_catches_a_store_whose_version_never_moves() {
    let report =
        contract::run_contract(|| async { Ok(UnversionedGraphStore(InMemoryGraphStore::new())) })
            .await;
    println!(
        "GraphStore contract — UnversionedGraphStore (deliberately broken)\n{}",
        report.summary()
    );
    let failed: Vec<&str> = report.failures().iter().map(|f| f.name).collect();
    assert!(
        failed.contains(&"version_bumps_on_every_write"),
        "a store whose version never moves must fail the version property; it failed {failed:?}"
    );
}
