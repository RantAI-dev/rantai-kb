//! Errors shared across the engine and its adapters.
//!
//! One `thiserror` enum per library crate. Adapters translate their own
//! driver-level failures into these. The engine turns degradation causes into
//! [`RetrievalDegrade`] values rather than propagating them as errors.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A port call failed. Adapters should wrap every driver-specific error in
/// this so the engine never sees a `sqlx::Error` or `reqwest::Error` directly.
#[derive(Debug, Error)]
pub enum PortError {
    /// Backend not reachable, timed out, or returned a 5xx-class failure.
    /// The engine treats this as a soft degradation of the affected arm
    /// (vector or BM25) rather than as a fatal request error.
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    /// Caller-supplied input is invalid (e.g. an empty document id, a malformed
    /// API key). The engine surfaces this to the HTTP layer as 4xx.
    #[error("invalid input: {0}")]
    Invalid(String),
    /// Backend reachable but its content is malformed (a row missing a column,
    /// a JSON value of the wrong shape). Indicates a schema drift or a
    /// regression in the parser; usually a bug to be reported.
    #[error("backend response malformed: {0}")]
    Malformed(String),
}

/// A retrieval arm (rerank, expansion, figures) failed in a way the engine
/// can absorb without failing the request. Recorded in the response and the
/// trace so operators can see degradation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalDegrade {
    /// Query expansion backend (LLM) failed or is not configured; returned the
    /// original query only.
    ExpansionUnavailable,
    /// Rerank backend failed or timed out; returned the fused order.
    RerankUnavailable,
    /// Neighbour-window expansion ran with errors; some or all neighbours missing.
    NeighboursPartial,
    /// Figure co-retrieval ran with errors; figures may be incomplete.
    FiguresPartial,
}

impl RetrievalDegrade {
    /// Short label for tracing and JSON output.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::ExpansionUnavailable => "expansion_unavailable",
            Self::RerankUnavailable => "rerank_unavailable",
            Self::NeighboursPartial => "neighbours_partial",
            Self::FiguresPartial => "figures_partial",
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
    NotFound,
    /// The engine could not produce any context AND no degradation path was
    /// available. Rare; usually returns empty context instead.
    #[error("retrieval failed: {0}")]
    RetrievalFailed(String),
}