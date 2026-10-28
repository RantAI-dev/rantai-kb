//! The Postgres graph adapter against the shared `GraphStore` contract, plus the
//! `load_tenant_graph` timing.
//!
//! Gated twice, like the Qdrant contract test:
//! * `--features integration`, so `cargo test --workspace` needs no database;
//! * `KB_PG_GRAPH_CONTRACT=1`, so `--features integration` on a machine without the
//!   contract database fails loudly instead of reporting a quiet pass.
//!
//! ```sh
//! rust/scripts/stack-up.sh            # brings up kb-smoke-pg and applies migrations
//! KB_PG_GRAPH_CONTRACT=1 cargo test -p kb-store-pg --features integration \
//!   --test graph_contract -- --nocapture
//! KB_PG_GRAPH_CONTRACT=1 cargo test -p kb-store-pg --features integration \
//!   --test graph_contract -- --ignored --nocapture   # the 100k-relation timing
//! ```
//!
//! The contract needs a *fresh* store per property, and one Postgres cannot give
//! that. The factory therefore deletes the fixture's two tenants before every
//! property, which is the isolation a live backend can honestly offer: it makes the
//! suite fail loudly on leftover state instead of quietly passing on it.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use kb_ports::graph_contract::{self, CONTRACT_TENANTS, ContractReport};
use kb_ports::{
    ChunkId, DocumentId, DocumentScope, EntityUpsert, GraphStore, PortError, RelationUpsert,
    TenantGraph, TenantId, adapter::ScopeToken,
};
use kb_store_pg::{PgGraphStore, PgSettings};

/// Relations in the timing fixture. The brief asks for ~100k.
const TIMING_RELATIONS: usize = 100_000;
/// Relations per write batch.
const TIMING_BATCH: usize = 1_000;
/// Entities in the timing fixture: a mean degree of 10 over 100k relations.
const TIMING_ENTITIES: usize = 10_000;
const TIMING_DOCUMENTS: usize = 100;
/// The tenant the timing fixture uses. Distinct from the contract's, so a timing
/// run and a contract run cannot tread on each other.
const TIMING_TENANT: &str = "graph-timing-tenant";

/// The database URL, preferring the explicit variable so a test run cannot pick up
/// a production URL by accident.
fn database_url() -> String {
    std::env::var("KB_PG_GRAPH_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| {
            panic!(
                "KB_PG_GRAPH_URL (or DATABASE_URL) must point at the Postgres the contract runs \
                 against.\n\
                 Start the stack with:\n  rust/scripts/stack-up.sh\n\
                 Then run:\n  KB_PG_GRAPH_CONTRACT=1 cargo test -p kb-store-pg --features integration \
                 --test graph_contract -- --nocapture"
            )
        })
}

async fn connect() -> Result<PgGraphStore, PortError> {
    let settings = PgSettings {
        database_url: database_url(),
        ..PgSettings::default()
    };
    PgGraphStore::connect(&settings).await
}

/// Remove everything the suite's tenants own, so each property starts from nothing.
///
/// Relations and mentions first: they cascade from the entities, but deleting
/// explicitly keeps this independent of the FK actions in the schema.
async fn reset(store: &PgGraphStore) -> Result<(), PortError> {
    let pool = store.pool();
    for tenant in CONTRACT_TENANTS {
        sqlx::query(r#"DELETE FROM "KbRelation" WHERE "tenantId" = $1"#)
            .bind(tenant)
            .execute(pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("reset relations: {e}")))?;
        sqlx::query(r#"DELETE FROM "KbMention" WHERE "tenantId" = $1"#)
            .bind(tenant)
            .execute(pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("reset mentions: {e}")))?;
        sqlx::query(r#"DELETE FROM "KbEntity" WHERE "tenantId" = $1"#)
            .bind(tenant)
            .execute(pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("reset entities: {e}")))?;
        sqlx::query(r#"DELETE FROM "KbGraphVersion" WHERE "tenantId" = $1"#)
            .bind(tenant)
            .execute(pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("reset version: {e}")))?;
    }
    Ok(())
}

async fn fresh() -> Result<PgGraphStore, PortError> {
    let store = connect().await?;
    reset(&store).await?;
    Ok(store)
}

/// A loud, unmissable skip when the gate is unset. A `return` here would print
/// nothing and the run would look like a pass, which is the failure mode this gate
/// exists to prevent.
fn require_gate() {
    if std::env::var("KB_PG_GRAPH_CONTRACT").as_deref() != Ok("1") {
        panic!(
            "KB_PG_GRAPH_CONTRACT=1 is required to run the GraphStore contract suite against a \
             running Postgres.\n\
             Start the stack with:\n  rust/scripts/stack-up.sh\n\
             Then run:\n  KB_PG_GRAPH_CONTRACT=1 cargo test -p kb-store-pg --features integration \
             --test graph_contract -- --nocapture"
        );
    }
}

#[tokio::test]
async fn pg_graph_store_runs_the_graph_store_contract() {
    require_gate();
    let report: ContractReport = graph_contract::run_contract(fresh).await;
    println!("GraphStore contract — PgGraphStore\n{}", report.summary());

    let failures = report.failures();
    assert!(
        failures.is_empty(),
        "the Postgres adapter failed the contract: {:?}",
        failures
            .iter()
            .map(|f| (f.name, format!("{:?}", f.outcome)))
            .collect::<Vec<_>>()
    );

    let skips = report.skips();
    assert!(
        skips.is_empty(),
        "the Postgres adapter must pass with zero skips, but had skips: {skips:?}"
    );
}

/// What `load_tenant_graph` costs at the size the plan cares about.
///
/// This is the number that decides whether the CSR cache is worth having at all: the
/// cache saves the walk, not the load, and a load that happens on every read is a
/// network round trip with extra steps. The target from
/// `docs/architecture-target.md` §11 is a p95 below 10 ms for *PPR + expansion*,
/// so the load has to be well inside that to leave room for the walk.
///
/// `#[ignore]`d rather than gated: it writes 100k rows, which is not something a
/// contract run should do behind the operator's back.
#[tokio::test]
#[ignore = "writes ~100k relations; run with --nocapture"]
async fn load_tenant_graph_timing_at_100k_relations() {
    require_gate();
    let store = connect().await.expect("could not connect to Postgres");
    let tenant = TenantId::new_unchecked(TIMING_TENANT);

    // Reset the timing tenant, so a re-run measures the same fixture.
    for table in ["KbRelation", "KbMention", "KbEntity", "KbGraphVersion"] {
        sqlx::query(&format!(r#"DELETE FROM "{table}" WHERE "tenantId" = $1"#))
            .bind(TIMING_TENANT)
            .execute(store.pool())
            .await
            .expect("could not reset the timing tenant");
    }

    let started = Instant::now();
    // Entities: one write per batch of 500, which is roughly what ingest does.
    let mut written_entities = 0usize;
    for batch in 0..TIMING_ENTITIES.div_ceil(500) {
        let first = batch * 500;
        let entities: Vec<EntityUpsert> = (first..(first + 500).min(TIMING_ENTITIES))
            .map(|i| {
                let document = format!("timing-doc-{}", i % TIMING_DOCUMENTS);
                EntityUpsert {
                    key: format!("entity-{i}"),
                    name: format!("Entity {i}"),
                    entity_type: "thing".to_string(),
                    aliases: vec![format!("ent-{i}")],
                    mentions: vec![ChunkId::new_unchecked(format!("{document}_0"))],
                }
            })
            .collect();
        let document =
            DocumentId::new_unchecked(format!("timing-doc-{}", first % TIMING_DOCUMENTS));
        store
            .upsert_entities(&tenant, &document, entities)
            .await
            .expect("could not write the timing entities");
        written_entities += (first..(first + 500).min(TIMING_ENTITIES)).count();
    }
    let entities_elapsed = started.elapsed();

    // Relations: one document per batch of 1000, so every `(src, dst, type,
    // document)` tuple is distinct. That tuple is the relation's identity — the
    // same relation asserted by two documents is two rows — so a generator that
    // reused document ids would collapse its own rows and measure a smaller graph
    // than it claims to have written. That is not hypothetical: an earlier version
    // of this fixture reused them, the adapter dutifully upserted, and the
    // assertion caught 10,000 relations where 100,000 were intended.
    let started = Instant::now();
    let mut written_relations = 0usize;
    for batch in 0..TIMING_RELATIONS.div_ceil(TIMING_BATCH) {
        let base = batch * TIMING_BATCH;
        let relations: Vec<RelationUpsert> = (base..(base + TIMING_BATCH).min(TIMING_RELATIONS))
            .map(|i| {
                // 10 relations per source entity, each to a distinct target, so
                // the 100,000 rows are 100,000 *distinct* edges. Deriving both
                // endpoints from `i % TIMING_ENTITIES` instead gives 100,000 rows
                // over 10,000 pairs, which the CSR collapses to 20,000 arcs — a
                // smaller graph than the fixture claimed to write.
                let src = i / 10;
                let dst = (i * 7 + 3) % TIMING_ENTITIES;
                RelationUpsert {
                    src_key: format!("entity-{src}"),
                    dst_key: format!("entity-{dst}"),
                    relation_type: "MENTIONED_WITH".to_string(),
                    weight: 1.0,
                    chunk_id: Some(ChunkId::new_unchecked(format!(
                        "timing-doc-{}_0",
                        i % TIMING_DOCUMENTS
                    ))),
                }
            })
            .collect();
        let document = DocumentId::new_unchecked(format!("timing-doc-{batch}"));
        store
            .relate(&tenant, &document, relations)
            .await
            .expect("could not write the timing relations");
        written_relations += (base..(base + TIMING_BATCH).min(TIMING_RELATIONS)).count();
    }
    let relations_elapsed = started.elapsed();

    // The scope is every document: a tenant-wide read is what a cache miss costs.
    let document_ids: Vec<DocumentId> = (0..TIMING_DOCUMENTS)
        .map(|i| DocumentId::new_unchecked(format!("timing-doc-{i}")))
        .collect();
    let token = ScopeToken::new_for_adapter();
    let scope = DocumentScope::new(&token, tenant.clone(), document_ids);

    // Warm the connection and the page cache, so the first sample is not a
    // first-ever-read artefact.
    let _ = store.load_tenant_graph(&scope).await;

    const SAMPLES: usize = 20;
    let mut samples: Vec<Duration> = Vec::with_capacity(SAMPLES);
    let mut graph = TenantGraph::empty();
    for _ in 0..SAMPLES {
        let started = Instant::now();
        graph = store
            .load_tenant_graph(&scope)
            .await
            .expect("could not load the timing graph");
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p50 = samples[SAMPLES / 2];
    let p95 = samples[(SAMPLES * 95 / 100).min(SAMPLES - 1)];

    println!();
    println!(
        "fixture: {written_entities} entities, {written_relations} relations over {TIMING_DOCUMENTS} documents"
    );
    println!("write:  entities {entities_elapsed:?}, relations {relations_elapsed:?}");
    println!(
        "loaded: {} entities, {} arcs",
        graph.entity_count(),
        graph.arc_count()
    );
    println!(
        "load_tenant_graph: p50 {:.2} ms, p95 {:.2} ms over {SAMPLES} samples",
        p50.as_secs_f64() * 1000.0,
        p95.as_secs_f64() * 1000.0,
    );
    println!(
        "CSR memory: {:.1} MB",
        graph.csr().heap_bytes() as f64 / (1024.0 * 1024.0)
    );
    println!(
        "budget: the §11 target is p95 < 10 ms for PPR + expansion, so a load under 10 ms leaves the walk room to breathe"
    );

    assert_eq!(
        graph.entity_count(),
        TIMING_ENTITIES,
        "every entity should be in the scoped graph"
    );
    assert!(
        graph.arc_count() >= TIMING_RELATIONS,
        "every relation should be in the scoped graph: got {} arcs for {TIMING_RELATIONS} relations",
        graph.arc_count()
    );
}
