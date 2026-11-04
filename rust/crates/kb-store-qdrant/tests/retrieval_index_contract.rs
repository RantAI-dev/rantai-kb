//! The Qdrant adapter against the shared `RetrievalIndex` contract.
//!
//! Gated twice:
//! * `--features integration`, so `cargo test --workspace` needs no containers;
//! * `KB_QDRANT_CONTRACT=1`, so `--features integration` on a machine without the
//!   contract environment fails loudly instead of reporting a quiet pass.
//!
//! ```sh
//! KB_STACK_QDRANT=1 rust/scripts/stack-up.sh
//! KB_QDRANT_CONTRACT=1 cargo test -p kb-store-qdrant --features integration \
//!   --test retrieval_index_contract -- --nocapture
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use kb_ports::PortError;
use kb_ports::contract::{self, ContractReport};
use kb_store_qdrant::{QdrantIndex, QdrantSettings, create_collection, drop_collection};

const DIM: u32 = 8;

fn collection_name() -> String {
    format!("contract_retrieval_index_{}", std::process::id())
}

async fn fresh() -> Result<QdrantIndex, PortError> {
    let name = collection_name();
    let mut settings = QdrantSettings::from_env();
    settings.dense_dims = Some(DIM);
    let client = settings.client()?;
    let _ = drop_collection(&client, &name).await;
    create_collection(&client, &settings, &name).await?;
    QdrantIndex::new_with_collection(settings, name)
}

#[tokio::test]
async fn qdrant_index_runs_the_retrieval_index_contract() {
    if std::env::var("KB_QDRANT_CONTRACT").as_deref() != Ok("1") {
        panic!(
            "KB_QDRANT_CONTRACT=1 is required to run the Qdrant contract test suite against a running Qdrant instance.\n\
             Start the stack with:\n  KB_STACK_QDRANT=1 rust/scripts/stack-up.sh\n\
             Then run:\n  KB_QDRANT_CONTRACT=1 cargo test -p kb-store-qdrant --features integration --test retrieval_index_contract -- --nocapture"
        );
    }

    let name = collection_name();
    let mut settings = QdrantSettings::from_env();
    settings.dense_dims = Some(DIM);
    let client = settings
        .client()
        .expect("could not construct Qdrant client from settings");

    let report: ContractReport = contract::run_contract(fresh).await;
    println!(
        "RetrievalIndex contract — QdrantIndex\n{}",
        report.summary()
    );

    // Clean up after the test suite finishes
    if let Err(err) = drop_collection(&client, &name).await {
        println!("warning: could not drop contract collection `{name}`: {err}");
    }

    let failures = report.failures();
    assert!(
        failures.is_empty(),
        "the Qdrant adapter failed the contract: {failures:?}"
    );

    let skips = report.skips();
    assert!(
        skips.is_empty(),
        "the Qdrant adapter must pass with zero skips, but had skips: {skips:?}"
    );
}
