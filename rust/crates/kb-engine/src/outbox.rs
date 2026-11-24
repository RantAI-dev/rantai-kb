//! The scope-outbox reconciler (SEC-2b).
//!
//! Postgres is the authority; this worker only repairs the Qdrant pre-filter so
//! predicate mode keeps its recall. It names ports only: `ScopeOutbox` for the
//! durable queue, `DocumentStore` for the authoritative KB membership and
//! `RetrievalIndex` for the payload write. No adapter crate appears here.
//!
//! The outbox row is written in the same transaction as the Postgres change,
//! so a crash between the two is impossible. A crash *after* a claim is safe:
//! claiming increments `attempts` and releases the lock, so another worker can
//! pick the row up.

use std::sync::Arc;
use std::time::Duration;

use kb_ports::{
    DEFAULT_OUTBOX_MAX_ATTEMPTS, DocumentStore, KnowledgeBaseId, OutboxOp, PortError,
    RetrievalIndex, ScopeOutbox, outbox_interval_from_env,
};

use crate::CancelToken;

/// What one drain pass did. Counters are additive diagnostics, not a contract
/// callers branch on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxReport {
    pub claimed: usize,
    pub repaired: usize,
    pub deleted: usize,
    pub failed: usize,
    /// Failures at or above the retry ceiling: left in Postgres for an operator.
    pub abandoned: usize,
}

/// Drains the scope outbox. Cloneable as an `Arc` so the server can run it in
/// its own task and a test can call `reconcile_once` directly.
pub struct OutboxReconciler {
    outbox: Arc<dyn ScopeOutbox>,
    documents: Arc<dyn DocumentStore>,
    index: Arc<dyn RetrievalIndex>,
    batch: u32,
    interval: Duration,
    max_attempts: i32,
}

impl OutboxReconciler {
    /// Assemble from ports. The interval comes from
    /// `KB_SCOPE_OUTBOX_INTERVAL_MS` (`2000` by default).
    #[must_use]
    pub fn new(
        outbox: Arc<dyn ScopeOutbox>,
        documents: Arc<dyn DocumentStore>,
        index: Arc<dyn RetrievalIndex>,
    ) -> Self {
        Self {
            outbox,
            documents,
            index,
            batch: 100,
            interval: outbox_interval_from_env(),
            max_attempts: DEFAULT_OUTBOX_MAX_ATTEMPTS,
        }
    }

    /// Rows per pass.
    #[must_use]
    pub const fn with_batch(mut self, batch: u32) -> Self {
        self.batch = batch;
        self
    }

    /// Poll interval, used by [`Self::run`].
    #[must_use]
    pub const fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Attempts after which a failing row is logged at error level and left.
    #[must_use]
    pub const fn with_max_attempts(mut self, max_attempts: i32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Apply one batch. A failed item is recorded on the row and the rest of
    /// the batch still proceeds: one bad document must not block every other
    /// repair.
    ///
    /// # Errors
    ///
    /// Fails on a port error that prevents the batch itself from being claimed
    /// or completed. An individual item failure is counted in the report, not
    /// returned: it is durable in the outbox and will be retried.
    pub async fn reconcile_once(&self) -> Result<OutboxReport, PortError> {
        let items = self.outbox.claim_batch(self.batch).await?;
        let mut report = OutboxReport {
            claimed: items.len(),
            ..OutboxReport::default()
        };
        let mut completed = Vec::with_capacity(items.len());

        for item in items {
            let result = match item.op {
                OutboxOp::KbChanged => {
                    // Postgres has the final word. `include_deleted = true`
                    // matters here: a row written just before a soft delete
                    // must converge to the same state as the delete row.
                    match self
                        .documents
                        .find_meta_by_id(&item.tenant, &item.document, true)
                        .await
                    {
                        Ok(Some(meta)) => {
                            let kb_ids: Vec<KnowledgeBaseId> = meta
                                .knowledge_base_ids
                                .iter()
                                .map(|id| KnowledgeBaseId::new_unchecked(id.clone()))
                                .collect();
                            self.index
                                .set_document_kb_payload(&item.tenant, &item.document, &kb_ids)
                                .await
                        }
                        Ok(None) => {
                            // Hard-deleted between write and repair: removing
                            // points is idempotent, so doing it twice is safe.
                            self.index
                                .delete_document(&item.tenant, &item.document)
                                .await
                                .map(|_| ())
                        }
                        Err(err) => Err(err),
                    }
                }
                OutboxOp::Deleted => self
                    .index
                    .delete_document(&item.tenant, &item.document)
                    .await
                    .map(|_| ()),
            };

            match result {
                Ok(()) => {
                    completed.push(item.id);
                    if item.op == OutboxOp::KbChanged {
                        report.repaired += 1;
                    } else {
                        report.deleted += 1;
                    }
                }
                Err(err) => {
                    let message = err.to_string();
                    report.failed += 1;
                    if item.attempts >= self.max_attempts {
                        report.abandoned += 1;
                        tracing::error!(
                            document = %item.document.as_str(),
                            tenant = %item.tenant.as_str(),
                            attempts = item.attempts,
                            error = %message,
                            "scope outbox row abandoned after the retry ceiling"
                        );
                    } else {
                        tracing::warn!(
                            document = %item.document.as_str(),
                            attempts = item.attempts,
                            error = %message,
                            "scope outbox repair failed; it will be retried"
                        );
                    }
                    // `fail` only records the error; `claim` already incremented
                    // the attempt count. A second failure on the same claim
                    // must not make the whole batch look unprocessed.
                    if let Err(fail_err) = self.outbox.fail(item.id, &message).await {
                        tracing::warn!(error = %fail_err.to_string(), "could not record outbox failure");
                    }
                }
            }
        }

        if !completed.is_empty() {
            self.outbox.complete(&completed).await?;
        }
        Ok(report)
    }

    /// Drain on boot, then every `interval` until cancellation.
    pub async fn run(self: Arc<Self>, cancel: CancelToken) {
        let mut cancelled = cancel.subscribe();
        self.drain_once().await;
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.interval) => self.drain_once().await,
                _ = cancelled.changed() => {
                    if *cancelled.borrow() {
                        return;
                    }
                }
            }
        }
    }

    async fn drain_once(&self) {
        match self.reconcile_once().await {
            Ok(report) if report.repaired > 0 || report.deleted > 0 || report.failed > 0 => {
                tracing::info!(
                    claimed = report.claimed,
                    repaired = report.repaired,
                    deleted = report.deleted,
                    failed = report.failed,
                    "scope outbox drain"
                );
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err.to_string(), "scope outbox claim failed"),
        }
    }
}

#[cfg(test)]
mod outbox_tests;
