//! HTTP server. axum + tokio. Reads a typed `Config` from env, wires the
//! engine and the adapters, serves the API, and — unless
//! `KB_WORKER_ENABLED=false` — runs the ingest worker in the same process.

pub mod auth;
pub mod boundary;
pub mod cli;
pub mod config;
pub mod dto;
pub mod egress;
pub mod error;
pub mod events;
pub mod routes;
pub mod runtime;
pub mod state;
pub mod tier;

pub use boundary::{ADAPTER_CRATES, INNER_CRATES};
pub use cli::{CliError, ReindexArgs, dispatch};
pub use config::{Config, WorkerConfig};
pub use dto::{ChunkDto, RetrievalDto, SearchRequestDto, SearchResponseDto, SourceDto};
pub use events::EventBus;
pub use state::AppState;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use axum::routing::any;
use tracing_subscriber::EnvFilter;

use kb_engine::CancelToken;

/// Build the Axum router for the given state.
///
/// Two layers wrap every route: the CORS layer (preflights answered before
/// authentication, allow-origin stamped on every response) and a fallback that
/// authenticates an unmatched path before answering 404.
pub fn build_router(state: AppState) -> Router {
    let shared = Arc::new(state);
    routes::health_router()
        .merge(routes::router())
        .fallback(any(routes::not_found))
        .layer(axum::middleware::from_fn(routes::cors))
        .with_state(shared)
}

/// Build the router and the socket address to bind, without binding.
pub async fn build(config: Config) -> anyhow::Result<(Router, SocketAddr)> {
    let bind_addr: SocketAddr = config.bind_addr.parse().context("invalid bind address")?;
    let state = AppState::from_config(config).await.context("app state")?;
    Ok((build_router(state), bind_addr))
}

/// Top-level wiring: connect the adapters, optionally start the ingest worker,
/// and serve until a signal arrives.
pub async fn run(config: Config) -> anyhow::Result<()> {
    let bind_addr: SocketAddr = config.bind_addr.parse().context("invalid bind address")?;
    let state = AppState::from_config(config.clone())
        .await
        .context("app state")?;
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .context("could not bind")?;

    let (worker, maintenance) = if config.worker.enabled {
        let worker = state.build_worker(&config, config.worker.ingest_settings())?;
        let maintenance = runtime::Maintenance {
            jobs: state.jobs.clone(),
            blobs: state.blob.clone(),
            reap_days: config.worker.reap_days,
            reap_interval: std::time::Duration::from_millis(config.worker.reap_ms),
            reap_batch: 100,
        };
        (Some(worker), Some(maintenance))
    } else {
        tracing::info!("worker disabled (KB_WORKER_ENABLED=false) — API only");
        (None, None)
    };

    runtime::serve(
        build_router(state),
        listener,
        worker,
        maintenance,
        CancelToken::new(),
    )
    .await
}

/// Initialize tracing subscriber once at process boot.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}
