//! `kb-server eval` argument handling.
//!
//! Two modes:
//!
//! * `eval <flags>` — run the black-box harness against a live server.
//! * `eval score --qrels <file> --run <run.trec>` — score an existing run file
//!   offline, with no server, so our arithmetic can be checked against
//!   `trec_eval`/`ranx`.
//!
//! Argument handling mirrors `kb-server create-api-key`: `--flag value` pairs
//! and a positional-free shape, so it feels the same to an operator.

use std::path::PathBuf;
use std::time::Duration;

use crate::dataset::parse_qrels;
use crate::report::{parse_trec, score_offline};
use crate::runner::{RunnerConfig, run as run_harness};

/// A usage mistake (`Usage`) versus the operation failing (`Failed`).
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Failed(String),
}

impl From<crate::runner::RunError> for EvalError {
    fn from(err: crate::runner::RunError) -> Self {
        Self::Failed(err.to_string())
    }
}

/// Entry point for `kb-server eval …`. `args` excludes the word `eval`.
///
/// # Errors
///
/// [`EvalError::Usage`] for a bad argument list; [`EvalError::Failed`] when the
/// run or the offline score cannot be produced.
pub async fn run(args: &[String]) -> Result<(), EvalError> {
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print_help();
            Ok(())
        }
        Some("score") => score(&args[1..]),
        _ => eval(args).await,
    }
}

fn print_help() {
    println!(
        "kb-server eval — measure retrieval quality

USAGE
    kb-server eval --base-url URL --dataset DIR [flags]
    kb-server eval score --qrels FILE --run FILE [--k N]

RUN FLAGS
    --base-url URL      kb-server to measure against (required)
    --api-key KEY       API key; defaults to $KB_EVAL_API_KEY
    --dataset DIR       BEIR/TREC dataset directory (required)
    --k N               cutoff for @k metrics (default 10)
    --max-chunks N      chunks requested per query (default 50)
    --out DIR           output directory (default eval-out)
    --skip-ingest       reuse <out>/ingest-map.json instead of uploading
    --label LABEL       run label written into the outputs (default run)
    --timeout SECS      per-job ingest timeout (default 900)
    --poll-ms MS        job poll interval (default 500)

OUTPUTS (in --out)
    metrics.json  every metric with a 95% bootstrap CI, plus latency
    report.md     the same, as a table
    run.trec      document-level TREC run
    run.page.trec page-level TREC run
    ingest-map.json  doc-key -> documentId, reused by --skip-ingest"
    );
}

async fn eval(args: &[String]) -> Result<(), EvalError> {
    let base_url = flag(args, "--base-url")
        .ok_or_else(|| EvalError::Usage("missing --base-url".to_string()))?;
    let dataset =
        flag(args, "--dataset").ok_or_else(|| EvalError::Usage("missing --dataset".to_string()))?;
    let api_key = flag(args, "--api-key")
        .or_else(|| std::env::var("KB_EVAL_API_KEY").ok())
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            EvalError::Usage("missing API key: pass --api-key or set KB_EVAL_API_KEY".to_string())
        })?;

    let mut config = RunnerConfig::new(&base_url, &api_key, PathBuf::from(&dataset));
    config.k = parse_uint(args, "--k", 10)?;
    config.max_chunks = u32::try_from(parse_uint(args, "--max-chunks", 50)?)
        .map_err(|_| EvalError::Usage("--max-chunks is too large".to_string()))?;
    config.out_dir = PathBuf::from(flag(args, "--out").unwrap_or_else(|| "eval-out".to_string()));
    config.skip_ingest = has_flag(args, "--skip-ingest");
    config.label = flag(args, "--label").unwrap_or_else(|| "run".to_string());
    config.job_timeout = Duration::from_secs(parse_uint_u64(args, "--timeout", 900)?);
    config.poll_interval = Duration::from_millis(parse_uint_u64(args, "--poll-ms", 500)?);

    let report = run_harness(config).await?;
    let m = &report.metrics;
    println!(
        "eval {}: {} queries ({} failed) — document nDCG@{} {:.3}, page nDCG@{} {:.3}",
        m.label, m.n_queries, m.n_failed, m.k, m.document.ndcg.mean, m.k, m.page.ndcg.mean
    );
    println!("wrote metrics.json, report.md, run.trec, run.page.trec");
    Ok(())
}

fn score(args: &[String]) -> Result<(), EvalError> {
    let qrels_path =
        flag(args, "--qrels").ok_or_else(|| EvalError::Usage("missing --qrels".to_string()))?;
    let run_path =
        flag(args, "--run").ok_or_else(|| EvalError::Usage("missing --run".to_string()))?;
    let k = parse_uint(args, "--k", 10)?;

    let qrels_text = std::fs::read_to_string(&qrels_path)
        .map_err(|e| EvalError::Failed(format!("could not read {qrels_path}: {e}")))?;
    let qrels = parse_qrels(&qrels_text)
        .map_err(|e| EvalError::Failed(format!("could not parse {qrels_path}: {e}")))?;
    let run_text = std::fs::read_to_string(&run_path)
        .map_err(|e| EvalError::Failed(format!("could not read {run_path}: {e}")))?;
    let rankings = parse_trec(&run_text)
        .map_err(|e| EvalError::Failed(format!("could not parse {run_path}: {e}")))?;

    let markdown = score_offline(&qrels, &rankings, k);
    print!("{markdown}");
    Ok(())
}

/// The value following `--flag`, if the flag is present and has one.
fn flag(args: &[String], name: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == name)?;
    let value = args.get(idx + 1)?;
    if value.starts_with("--") {
        return None;
    }
    Some(value.clone())
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn parse_uint(args: &[String], name: &str, default: usize) -> Result<usize, EvalError> {
    match flag(args, name) {
        None => Ok(default),
        Some(value) => value
            .parse()
            .map_err(|_| EvalError::Usage(format!("{name} expects a number, got {value:?}"))),
    }
}

fn parse_uint_u64(args: &[String], name: &str, default: u64) -> Result<u64, EvalError> {
    match flag(args, name) {
        None => Ok(default),
        Some(value) => value
            .parse()
            .map_err(|_| EvalError::Usage(format!("{name} expects a number, got {value:?}"))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn flags_are_read_as_pairs() {
        let a = args(&["--base-url", "http://x", "--k", "5", "--skip-ingest"]);
        assert_eq!(flag(&a, "--base-url").as_deref(), Some("http://x"));
        assert_eq!(parse_uint(&a, "--k", 10).unwrap(), 5);
        assert!(has_flag(&a, "--skip-ingest"));
        assert!(!has_flag(&a, "--out"));
    }

    #[test]
    fn a_trailing_flag_has_no_value() {
        let a = args(&["--dataset", "--k", "3"]);
        assert_eq!(flag(&a, "--dataset"), None);
    }

    #[test]
    fn a_non_number_is_a_usage_error() {
        let a = args(&["--k", "many"]);
        assert!(matches!(
            parse_uint(&a, "--k", 10),
            Err(EvalError::Usage(_))
        ));
    }

    #[tokio::test]
    async fn missing_required_flags_are_usage_errors() {
        let err = eval(&args(&["--dataset", "d"])).await.unwrap_err();
        assert!(matches!(err, EvalError::Usage(_)), "{err:?}");
    }
}
