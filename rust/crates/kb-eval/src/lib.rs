//! `kb-eval` — the retrieval-quality instrument.
//!
//! It measures a running `kb-server` from the outside: it uploads a corpus,
//! runs every query, and turns the results into standard metrics with bootstrap
//! confidence intervals. Nothing here knows which storage or inference backend
//! answered — the HTTP API is the only contract, so the same harness measures
//! the Qdrant-backed Rust service as it measured the SurrealDB-backed Bun one.
//!
//! The crate is three pure pieces and one thin client:
//!
//! * [`dataset`] loads a BEIR/TREC dataset directory.
//! * [`metrics`] is the arithmetic (nDCG@k with linear gain, Recall@k, MRR@k,
//!   Hit@1, P@k).
//! * [`report`] writes `metrics.json`, `report.md` and TREC run files.
//! * [`runner`] is the black-box HTTP client.
//! * [`stages`] reads the server's per-stage latency out of a response.
//! * [`bench`] is the closed-loop load generator behind `kb-server bench search`.
//!
//! `kb-server eval` is the front door; see [`cli`].

pub mod bench;
pub mod cli;
pub mod dataset;
pub mod leak;
pub mod leak_http;
pub mod metrics;
pub mod pdf;
pub mod report;
pub mod runner;
pub mod stages;

pub use bench::{BenchConfig, BenchReport, run_cli as run_bench};
pub use cli::{EvalError, run};
pub use dataset::{Dataset, DatasetError};
pub use leak::{Arm, LeakMatrix, ScopeKind, SeedPlan, Status};
pub use metrics::{
    Aggregate, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED, Gain, LatencyReport, LevelReport, Means,
    QueryMetrics,
};
pub use report::{MetricsFile, RunReport};
pub use runner::{RunError, RunnerConfig};
pub use stages::{StageLatencyReport, StageSample, parse_stage_timings};
