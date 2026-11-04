//! Process model: one binary serves HTTP and runs the ingest worker.
//!
//! `src/server.ts` starts Bun.serve and, unless `KB_WORKER_ENABLED=false`,
//! the in-process worker in the same process. This is the same shape: the
//! worker claims with `FOR UPDATE SKIP LOCKED`, so scaling out is just running
//! more replicas — no queue broker, no separate deployment.
//!
//! Shutdown stops accepting connections, then cancels the worker, which is only
//! observed *between* jobs: a half-indexed document is exactly the state the
//! stale-reclaim exists to clean up, so a job in flight is finished first.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use tokio::net::TcpListener;

use kb_engine::{CancelToken, IngestWorker, reap_failed_job_uploads};
use kb_ports::{BlobStore, JobStore};

/// Background maintenance that is not the worker's own loop: the orphan reaper
/// that deletes stored files of failed jobs nobody retried.
pub struct Maintenance {
    /// Queue holding the jobs whose uploads may be reaped.
    pub jobs: Arc<dyn JobStore>,
    /// Object storage holding those uploads.
    pub blobs: Arc<dyn BlobStore>,
    /// Age at which a failed job's upload is reaped (`KB_INGEST_REAP_DAYS`).
    pub reap_days: u32,
    /// How often the sweep runs (`KB_INGEST_REAP_MS`).
    pub reap_interval: Duration,
    /// Most uploads considered per sweep, matching the TypeScript call.
    pub reap_batch: u32,
}

impl Maintenance {
    /// Run one sweep. Failures are logged and swallowed: a reaper that throws
    /// would otherwise stop sweeping at all.
    pub async fn reap_once(&self) {
        match reap_failed_job_uploads(
            self.jobs.as_ref(),
            self.blobs.as_ref(),
            self.reap_days,
            self.reap_batch,
        )
        .await
        {
            Ok(n) if n > 0 => tracing::info!(count = n, "reaped orphaned upload(s)"),
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "reap failed"),
        }
    }

    /// Sweep on boot, then on every interval until cancelled.
    pub async fn run(self, cancel: CancelToken) {
        let mut cancelled = cancel.subscribe();
        self.reap_once().await;
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.reap_interval) => self.reap_once().await,
                _ = cancelled.changed() => {
                    if *cancelled.borrow() {
                        return;
                    }
                }
            }
        }
    }
}

/// How long shutdown waits for the in-flight job before giving up on a clean
/// exit. Long enough for a typical document, short enough that a stuck
/// extractor cannot hold the process open indefinitely.
const WORKER_DRAIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Serve until the listener stops.
///
/// # Errors
///
/// Propagates the server error and a failure to join the worker task.
pub async fn serve(
    app: Router,
    listener: TcpListener,
    worker: Option<IngestWorker>,
    maintenance: Option<Maintenance>,
    cancel: CancelToken,
) -> anyhow::Result<()> {
    let worker_task = worker.map(|worker| {
        let cancel = cancel.clone();
        tokio::spawn(async move { worker.run(cancel).await })
    });
    let reaper_task = maintenance.map(|m| tokio::spawn(m.run(cancel.clone())));

    let local_addr = listener
        .local_addr()
        .context("could not read the bound address")?;
    tracing::info!(address = %local_addr, "kb-server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cancel.clone()))
        .await
        .context("http server")?;

    // The listener is closed; tell the worker to stop and let the job it is
    // running finish rather than abandoning it half-indexed.
    cancel.cancel();
    if let Some(handle) = worker_task {
        match tokio::time::timeout(WORKER_DRAIN_TIMEOUT, handle).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::error!(error = %err, "ingest worker task failed"),
            Err(_) => tracing::warn!("ingest worker did not stop within the drain window"),
        }
    }
    if let Some(handle) = reaper_task {
        let _ = handle.await;
    }
    tracing::info!("kb-server stopped");
    Ok(())
}

/// Resolve when the process is asked to stop: SIGINT, SIGTERM, or the
/// cancellation token being cancelled by an embedder.
pub async fn shutdown_signal(cancel: CancelToken) {
    let mut cancelled = cancel.subscribe();
    tokio::select! {
        () = wait_for_signal() => {}
        _ = cancelled.changed() => tracing::info!("shutdown requested"),
    }
}

/// Resolve on SIGINT or SIGTERM.
///
/// A signal handler that cannot be installed is logged and turns into a
/// never-resolving future: the process keeps serving rather than exiting
/// because it lost the ability to notice a signal.
pub async fn wait_for_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            tracing::warn!("could not listen for SIGINT");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => {
                tracing::warn!("could not listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("SIGINT — shutting down"),
        () = terminate => tracing::info!("SIGTERM — shutting down"),
    }
}
