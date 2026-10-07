//! HTTP server. axum + tokio. Reads a typed `Config` from env, wires the
//! engine and the adapters, and serves the read-path endpoints.

pub mod auth;
pub mod config;
pub mod error;
pub mod routes;
pub mod state;

pub use config::Config;
pub use state::AppState;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use kb_engine::EngineSettings;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Top-level wiring. Builds `AppState` from `Config` and returns a configured
/// axum `Router` plus the socket address to bind.
pub async fn build(config: Config) -> anyhow::Result<(Router, SocketAddr)> {
    let state = AppState::from_config(config).await.context("db::build")?;
    let app = build_router(state);
    let addr: SocketAddr = std::env::var("KB_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse()
        .context("parse KB_BIND_ADDR")?;
    Ok((app, addr))
}

/// Build the axum `Router`. Exposed so integration tests can call it directly.
#[must_use]
pub fn build_router(state: AppState) -> Router {
    let router = routes::router()
        .merge(routes::health_router())
        .with_state(Arc::new(state));
    router.layer(TraceLayer::new_for_http())
}

/// Run the server until SIGINT/SIGTERM.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    init_tracing();
    let (app, addr) = build(config.clone()).await?;
    info!(%addr, "kb-server starting");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve")?;
    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!("failed to install SIGTERM handler: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    info!("kb-server shutdown signal received");
}

/// Default engine settings used when the env doesn't override them.
#[must_use]
pub fn default_engine_settings() -> EngineSettings {
    EngineSettings::default()
}
