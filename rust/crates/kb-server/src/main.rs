//! `kb-server` binary: reads env, wires adapters, serves HTTP.

use kb_server::{Config, build, init_tracing};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let config = Config::from_env()?;
    let (app, addr) = build(config).await?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "kb-server listening");
    axum::serve(listener, app).await?;
    Ok(())
}
