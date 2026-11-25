//! The scope outbox port (SEC-2b).
//!
//! Postgres is the authority for document visibility; the Qdrant payload is a
//! fast pre-filter in predicate mode. A change to a document's knowledge-base
//! membership, or its deletion, therefore writes a row here **in the same
//! transaction** as the change. A worker drains the rows and repairs the index.
//!
//! The port is deliberately small and adapter-agnostic: the engine claims work,
//! reads the authoritative document metadata through [`crate::DocumentStore`],
//! and writes the payload through [`crate::RetrievalIndex`]. Nothing here names
//! Postgres, Qdrant, or a settings source.

use crate::{DocumentId, PortError, TenantId};

/// What the reconciler must do for one outbox row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxOp {
    /// Re-read the document's knowledge bases and overwrite the index payload.
    KbChanged,
    /// Remove the document's points from the index.
    Deleted,
}

impl OutboxOp {
    /// The value stored in the `op` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KbChanged => "kb_changed",
            Self::Deleted => "deleted",
        }
    }

    /// Parse a stored value. An unknown value is an error, never a silent no-op.
    pub fn parse(value: &str) -> Result<Self, PortError> {
        match value {
            "kb_changed" => Ok(Self::KbChanged),
            "deleted" => Ok(Self::Deleted),
            other => Err(PortError::Malformed(format!("unknown outbox op {other:?}"))),
        }
    }
}

/// One claimed row. `id` is the bigserial key; `attempts` is incremented by the
/// claim so a worker that dies mid-item does not hold it forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItem {
    pub id: i64,
    pub tenant: TenantId,
    pub document: DocumentId,
    pub op: OutboxOp,
    pub attempts: i32,
}

/// How old the oldest pending row is, for an additive health field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxLag {
    pub pending: u64,
    /// Whole seconds since the oldest pending row was written, or `None` when
    /// there is nothing pending.
    pub oldest_pending_secs: Option<u64>,
}

/// Durable queue of payload repairs. Implemented by the Postgres store adapter.
#[async_trait::async_trait]
pub trait ScopeOutbox: Send + Sync {
    /// Claim up to `limit` rows, oldest first, skipping rows another worker is
    /// processing. The claim increments `attempts`; a crashed worker's rows
    /// become claimable again.
    async fn claim_batch(&self, limit: u32) -> Result<Vec<OutboxItem>, PortError>;

    /// Remove successfully repaired rows.
    async fn complete(&self, ids: &[i64]) -> Result<(), PortError>;

    /// Record a failed repair. The row stays for another attempt.
    async fn fail(&self, id: i64, error: &str) -> Result<(), PortError>;

    /// Pending count and oldest pending age, for health reporting.
    async fn lag(&self) -> Result<OutboxLag, PortError>;
}

/// Default retry ceiling: a row that failed ten times is logged at error level
/// and left for an operator instead of being deleted or retried forever.
pub const DEFAULT_OUTBOX_MAX_ATTEMPTS: i32 = 10;

/// Default drain interval (`KB_SCOPE_OUTBOX_INTERVAL_MS`).
pub const DEFAULT_OUTBOX_INTERVAL_MS: u64 = 2_000;

/// Read `KB_SCOPE_OUTBOX_INTERVAL_MS`. Unset, blank or malformed values use the
/// default; the value is a poll interval in milliseconds.
#[must_use]
pub fn outbox_interval_from_env() -> std::time::Duration {
    let millis = std::env::var("KB_SCOPE_OUTBOX_INTERVAL_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_OUTBOX_INTERVAL_MS);
    std::time::Duration::from_millis(millis)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn op_round_trips_through_its_stored_value() {
        for op in [OutboxOp::KbChanged, OutboxOp::Deleted] {
            assert_eq!(OutboxOp::parse(op.as_str()).unwrap(), op);
        }
        assert!(OutboxOp::parse("nonsense").is_err());
    }

    #[test]
    fn the_documented_defaults_are_compile_time_constants() {
        // No environment mutation in a parallel test run: the pure default is
        // what the constants promise to an operator who leaves the var unset.
        const { assert!(DEFAULT_OUTBOX_INTERVAL_MS == 2_000) };
        const { assert!(DEFAULT_OUTBOX_MAX_ATTEMPTS == 10) };
    }
}
