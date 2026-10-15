//! `kb-server` binary: reads env, wires adapters, serves HTTP and (unless
//! `KB_WORKER_ENABLED=false`) runs the ingest worker in the same process.

use kb_server::{Config, init_tracing, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    run(Config::from_env()?).await
}