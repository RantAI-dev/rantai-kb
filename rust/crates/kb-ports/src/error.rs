//! Structured errors for ports and engine.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{DocumentId, TenantId};

/// Errors emitted by port implementations (stores, embedders, rerankers).
#[derive(Debug, Error)]
pub enum PortError {
    /// External service (Postgres, SurrealDB, LLM provider) is unreachable
    /// or returned an error status code.
    #[error("backend unavailable: {0}")]
    Unavailable(String),

    /// Payload returned by the external service failed to parse.
    #[error("malformed response from backend: {0}")]
    Malformed(String),

    /// Storage invariant violated (e.g. unique constraint, missing required row).
    #[error("storage invariant violated: {0}")]
    Storage(String),

    /// Request arguments are semantically invalid (e.g. dimension mismatch).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// Request arguments are semantically invalid.
    #[error("invalid argument: {0}")]
    Invalid(String),

    /// Timeout communicating with backend.
    #[error("timeout communicating with backend")]
    Timeout,
}

/// A retrieval arm (rerank, expansion, figures) failed in a way the engine
/// can absorb without failing the request. Recorded in the response and the
/// trace so operators can see degradation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalDegrade {
    /// Vector search arm failed; fell back to BM25 alone.
    VectorArmFailed,
    /// BM25 search arm failed; fell back to vector alone.
    Bm25ArmFailed,
    /// Query expansion backend (LLM) failed or is not configured; returned the
    /// original query only.
    ExpansionUnavailable,
    /// Rerank backend failed or timed out; returned the fused order.
    RerankUnavailable,
    /// Neighbour-window expansion ran with errors; some or all neighbours missing.
    NeighboursPartial,
    /// Figure co-retrieval ran with errors; figures may be incomplete.
    FiguresPartial,
    /// The graph arm's store failed or timed out. The other arms' results are
    /// returned unchanged — a search that lost one arm is degraded, not empty,
    /// and saying nothing about it is how a silent regression ships (AGENTS rule 6).
    GraphArmUnavailable,
}

impl RetrievalDegrade {
    /// Short label for tracing and JSON output.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::VectorArmFailed => "vector_arm_failed",
            Self::Bm25ArmFailed => "bm25_arm_failed",
            Self::ExpansionUnavailable => "expansion_unavailable",
            Self::RerankUnavailable => "rerank_unavailable",
            Self::NeighboursPartial => "neighbours_partial",
            Self::FiguresPartial => "figures_partial",
            Self::GraphArmUnavailable => "graph_arm_unavailable",
        }
    }
}

/// Engine-level errors. The HTTP layer maps these to status codes.
#[derive(Debug, Error)]
pub enum EngineError {
    /// Tenant identity missing or invalid.
    #[error("unauthorized")]
    Unauthorized,
    /// Caller is authenticated but lacks a required scope (e.g. `kb:read`).
    #[error("missing scope: {0}")]
    Forbidden(&'static str),
    /// No document with that id is visible to this tenant.
    #[error("not found")]
    NotFound(DocumentId),
    /// Retrieval pipeline failed; neither arm succeeded or fatal store error.
    #[error("retrieval failed: {0}")]
    RetrievalFailed(String),
    /// Scope check failed: the tenant is not allowed to query the requested KB.
    #[error("tenant {0:?} cannot query KB")]
    ScopeViolation(TenantId),
}
