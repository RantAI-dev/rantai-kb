//! HTTP server. axum + tokio. Reads a typed `Config` from env, wires the
//! engine and the adapters, and serves the read-path endpoints.

pub mod auth;
pub mod config;
pub mod dto;
pub mod error;
pub mod routes;
pub mod state;

pub use config::Config;
pub use dto::{ChunkDto, SearchRequestDto, SearchResponseDto, SourceDto};
pub use state::AppState;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use tracing_subscriber::EnvFilter;

/// Build the Axum router for the given state.
pub fn build_router(state: AppState) -> Router {
    let shared = Arc::new(state);
    routes::health_router()
        .merge(routes::router())
        .with_state(shared)
}

/// Top-level wiring. Builds `AppState` from `Config` and returns a configured
/// axum `Router` plus the socket address to bind.
pub async fn build(config: Config) -> anyhow::Result<(Router, SocketAddr)> {
    let state = AppState::from_config(config.clone())
        .await
        .context("db::build")?;
    let bind_addr: SocketAddr = config.bind_addr.parse().context("invalid bind address")?;
    let app = build_router(state);
    Ok((app, bind_addr))
}

/// Initialize tracing subscriber once at process boot.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}
