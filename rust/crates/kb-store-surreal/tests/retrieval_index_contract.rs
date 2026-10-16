//! The legacy SurrealDB adapter against the shared `RetrievalIndex` contract.
//!
//! Gated twice, on purpose:
//!
//! * `--features integration`, so `cargo test --workspace` needs no containers;
//! * `KB_SURREAL_CONTRACT=1`, so `--features integration` on a machine with no
//!   stack up prints a skip with the reason instead of a connection error that
//!   looks like a product failure.
//!
//! ```sh
//! rust/scripts/stack-up.sh
//! source /tmp/kb-stack/stack.env          # exports SURREAL_DB_* and friends
//! KB_SURREAL_CONTRACT=1 cargo test -p kb-store-surreal --features integration \
//!   --test retrieval_index_contract -- --nocapture
//! rust/scripts/stack-down.sh
//! ```
//!
//! **Isolation.** Several properties delete and re-upsert, so a property must
//! not see the previous one's writes. The factory therefore drops and recreates
//! the whole database before handing the store over, which is isolation by
//! destruction rather than by careful bookkeeping. The database is dropped at
//! the end of the run, so a finished run leaves the stack exactly as it found it
//! even if a property failed.

#![cfg(feature = "integration")]

use std::time::Duration;

use kb_ports::PortError;
use kb_ports::contract::{self, ContractReport, Outcome};
use kb_store_surreal::{SurrealChunkStore, SurrealSettings, schema};

/// The contract fixture uses 8-dimensional dense vectors, so the schema is
/// applied at that width.
const DIM: u32 = 8;

/// The database this run uses.
///
/// **One** name for the whole run, not one per property: `fresh` drops and
/// recreates it before every property, so isolation comes from the wipe rather
/// than from a name per property. A name per property would leave one database
/// behind for each of the eight, which is exactly the mess the doc comment on
/// this file says a run does not leave behind. The process id keeps two
/// concurrent `cargo test` invocations off each other.
fn database_name() -> String {
    format!("contract_retrieval_index_{}", std::process::id())
}

fn settings(database: &str) -> SurrealSettings {
    SurrealSettings {
        url: std::env::var("SURREAL_DB_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18000".to_string()),
        namespace: std::env::var("SURREAL_DB_NAMESPACE").unwrap_or_else(|_| "rantai".to_string()),
        database: database.to_string(),
        username: std::env::var("SURREAL_DB_USER").ok(),
        password: std::env::var("SURREAL_DB_PASS").ok(),
        request_timeout: Duration::from_secs(30),
        embedding_dims: std::env::var("KB_EMBEDDING_DIM")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DIM),
    }
}

/// Connect, prove the server is up, then wipe and re-apply the schema.
///
/// The probe matters: `RETURN true` is the cheapest statement SurrealDB answers
/// without touching a table, so a failure here means "not running" rather than
/// "schema missing", and the error message can say so.
async fn fresh() -> Result<SurrealChunkStore, PortError> {
    let name = database_name();
    let config = settings(&name);
    let store = SurrealChunkStore::new(config.clone())?;
    store.query("RETURN true;").await?;
    store
        .query(&format!("REMOVE DATABASE IF EXISTS `{name}`;"))
        .await?;
    schema::apply_schema(&store, config.embedding_dims).await?;
    Ok(store)
}

#[tokio::test]
async fn surreal_index_runs_the_retrieval_index_contract() {
    if std::env::var("KB_SURREAL_CONTRACT").as_deref() != Ok("1") {
        println!(
            "SKIP: the SurrealDB contract suite needs KB_SURREAL_CONTRACT=1 and a running\n      \
             SurrealDB:\n        rust/scripts/stack-up.sh && source /tmp/kb-stack/stack.env\n        \
             cargo test -p kb-store-surreal --features integration --test \
             retrieval_index_contract"
        );
        return;
    }

    let name = database_name();
    let report: ContractReport = contract::run_contract(fresh).await;
    println!(
        "RetrievalIndex contract — SurrealChunkStore (legacy adapter)\n{}",
        report.summary()
    );

    // Tear down whatever the run left, on the failure path too. The assertions
    // below come after this, so a failing property still gets its database
    // removed.
    if let Ok(store) = SurrealChunkStore::new(settings(&name))
        && let Err(err) = store
            .query(&format!("REMOVE DATABASE IF EXISTS `{name}`;"))
            .await
    {
        println!("warning: could not drop the contract database `{name}`: {err}");
    }

    let failures = report.failures();
    assert!(
        failures.is_empty(),
        "the SurrealDB adapter failed the contract: {failures:?}"
    );

    // The one property this adapter must not be able to pass, and must say why
    // it cannot. If someone "fixes" the AND semantics here without touching
    // SurrealDB's analyzer, this is the assertion that says so.
    let or_skip = report
        .skips()
        .into_iter()
        .find(|s| s.name == "keyword_or_semantics")
        .map(|s| s.outcome.clone());
    assert!(
        matches!(or_skip, Some(Outcome::Skipped(_))),
        "an adapter declaring AllTerms must skip keyword_or_semantics with a reason, got {or_skip:?}"
    );
}
