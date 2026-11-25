//! The CLI glue for `kb-server eval quant-bench`.
//!
//! The measurement itself lives in `kb_store_qdrant::bench`: it speaks gRPC to
//! Qdrant, so it belongs with the Qdrant adapter. This module owns the user's
//! terminal — flag errors, progress lines and the final summary.

use kb_store_qdrant::bench::{BenchError, QuantBenchArgs};

use crate::cli::CliError;

/// Run `eval quant-bench <flags>`.
///
/// # Errors
///
/// Bad flags are a [`CliError::Usage`]; a failed run is a [`CliError::Failed`].
pub async fn run(args: &[String]) -> Result<(), CliError> {
    let parsed = QuantBenchArgs::parse(args).map_err(map_error)?;
    println!(
        "quant-bench: {} point(s), {} dims, {} queries, seed {}, modes {}",
        parsed.points,
        parsed.dim,
        parsed.queries,
        parsed.seed,
        parsed
            .modes
            .iter()
            .map(|mode| mode.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let report = kb_store_qdrant::bench::run(&parsed)
        .await
        .map_err(map_error)?;
    for result in &report.results {
        println!(
            "  {:>6}: p50 {:>7.2} ms  p95 {:>7.2} ms  recall@20 {:.4}  RAM {}",
            result.mode, result.p50_ms, result.p95_ms, result.recall_at_20, result.ram
        );
    }
    println!(
        "quant-bench: wrote {}/results.json and {}/report.md",
        parsed.out.display(),
        parsed.out.display()
    );
    Ok(())
}

fn map_error(err: BenchError) -> CliError {
    match err {
        BenchError::Usage(message) => CliError::Usage(message),
        BenchError::Failed(message) => CliError::Failed(message),
    }
}
