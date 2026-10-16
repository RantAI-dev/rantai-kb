//! `kb-server eval` argument handling.
//!
//! Three modes:
//!
//! * `eval <flags>` — run the black-box harness against a live server.
//! * `eval score --qrels <file> --run <run.trec>` — score an existing run file
//!   offline, with no server, so our arithmetic can be checked against
//!   `trec_eval`/`ranx`.
//! * `eval leak <flags>` — the red-team matrix: every retrieval arm × every
//!   scope type, with a positive control on every cell.
//!
//! Argument handling mirrors `kb-server create-api-key`: `--flag value` pairs
//! and a positional-free shape, so it feels the same to an operator.

use std::path::PathBuf;
use std::time::Duration;

use crate::dataset::parse_qrels;
use crate::leak::{Arm, ScopeKind};
use crate::leak_http::{self, LeakConfig};
use crate::metrics::Gain;
use crate::report::{MetricsFile, parse_trec, score_offline};
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
        Some("leak") => leak(&args[1..]).await,
        _ => eval(args).await,
    }
}

fn print_help() {
    println!(
        "kb-server eval — measure retrieval quality

USAGE
    kb-server eval --base-url URL --dataset DIR [flags]
    kb-server eval score --qrels FILE --run FILE [--k N] [--gain G]
    kb-server eval leak --base-url URL [--api-key KEY] [--peer-key KEY]

RUN FLAGS
    --base-url URL      kb-server to measure against (required)
    --api-key KEY       API key; defaults to $KB_EVAL_API_KEY
    --dataset DIR       BEIR/TREC dataset directory (required)
    --k N               cutoff for @k metrics (default 10)
    --gain G            nDCG gain: linear (default, trec_eval/BEIR/MTEB) or
                        exponential (Burges; --gain exponential)
    --max-chunks N      chunks requested per query (default 50)
    --out DIR           output directory (default eval-out)
    --skip-ingest       reuse <out>/ingest-map.json instead of uploading
    --label LABEL       run label written into the outputs (default run)
    --timeout SECS      per-job ingest timeout (default 900)
    --poll-ms MS        job poll interval (default 500)
    --allow-failures    exit 0 even when queries or corpus files failed
                        (the outputs are written either way)

LEAK FLAGS
    --base-url URL      kb-server to probe (required)
    --api-key KEY       full-access key of the first tenant; defaults to
                        $KB_EVAL_API_KEY
    --peer-key KEY      full-access key of a *different* tenant; defaults to
                        $KB_EVAL_PEER_API_KEY. Without it the cross-tenant row
                        of the matrix is UNTESTED, not passing.
    --out DIR           output directory (default eval-out/leak)
    --max-chunks N      chunks requested per probe (default 20)
    --canary-seed N     seed for the planted canary tokens (default: fixed)
    --timeout SECS      per-document ingest timeout (default 900)
    --poll-ms MS        job poll interval (default 400)
    --keep              leave the seeded corpus behind
    --strict            also exit non-zero when an arm is UNTESTED

OUTPUTS (in --out)
    metrics.json  every metric with a 95% bootstrap CI, plus latency
    report.md     the same, as a table
    run.trec      document-level TREC run
    run.page.trec page-level TREC run
    ingest-map.json  doc-key -> documentId, reused by --skip-ingest

LEAK OUTPUTS (in --out)
    leak-report.md   the arm x scope matrix, with every leak's evidence
    leak.json        the same, machine-readable, plus the seeded corpus"
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
    config.gain = parse_gain(args)?;
    config.job_timeout = Duration::from_secs(parse_uint_u64(args, "--timeout", 900)?);
    config.poll_interval = Duration::from_millis(parse_uint_u64(args, "--poll-ms", 500)?);

    let allow_failures = has_flag(args, "--allow-failures");
    let report = run_harness(config).await?;
    let m = &report.metrics;
    println!(
        "eval {}: {} queries ({} failed, gain {}) — document nDCG@{} {:.3}, page nDCG@{} {:.3}",
        m.label, m.n_queries, m.n_failed, m.gain, m.k, m.document.ndcg.mean, m.k, m.page.ndcg.mean
    );
    // The strict means are printed next to the answered-only ones so nobody has
    // to open the file to see whether a failure was folded in.
    println!(
        "answered-only (secondary): document nDCG@{} {:.3} over {} queries",
        m.k, m.document.answered.ndcg.mean, m.document.answered.n_queries
    );
    println!(
        "measured server version: {}",
        m.server_version.as_deref().unwrap_or("unknown")
    );
    println!("wrote metrics.json, report.md, run.trec, run.page.trec");
    gate_on_failures(m, allow_failures)
}

/// Fail the command when the run was not clean, unless the caller opted out.
///
/// The outputs are written before this is called, so a failing run still leaves
/// evidence behind: `--allow-failures` is for a CI job that wants the numbers
/// *and* the non-zero exit, not for suppressing a failure quietly.
fn gate_on_failures(m: &MetricsFile, allow_failures: bool) -> Result<(), EvalError> {
    if allow_failures || (m.n_failed == 0 && m.n_ingest_failed == 0) {
        return Ok(());
    }
    Err(EvalError::Failed(format!(
        "{} of {} queries and {} corpus file(s) failed; pass --allow-failures to exit 0 \
         anyway (metrics.json and report.md were written)",
        m.n_failed, m.n_queries, m.n_ingest_failed
    )))
}

async fn leak(args: &[String]) -> Result<(), EvalError> {
    let base_url = flag(args, "--base-url")
        .ok_or_else(|| EvalError::Usage("missing --base-url".to_string()))?;
    let api_key = flag(args, "--api-key")
        .or_else(|| std::env::var("KB_EVAL_API_KEY").ok())
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            EvalError::Usage("missing API key: pass --api-key or set KB_EVAL_API_KEY".to_string())
        })?;
    let peer_key = flag(args, "--peer-key")
        .or_else(|| std::env::var("KB_EVAL_PEER_API_KEY").ok())
        .filter(|k| !k.trim().is_empty());

    let mut config = LeakConfig::new(&base_url, &api_key);
    config.peer_key = peer_key;
    config.out_dir = PathBuf::from(flag(args, "--out").unwrap_or_else(|| "eval-out/leak".into()));
    config.max_chunks = u32::try_from(parse_uint(args, "--max-chunks", 20)?)
        .map_err(|_| EvalError::Usage("--max-chunks is too large".to_string()))?;
    config.canary_seed = parse_uint_u64(args, "--canary-seed", config.canary_seed)?;
    config.job_timeout = Duration::from_secs(parse_uint_u64(args, "--timeout", 900)?);
    config.poll_interval = Duration::from_millis(parse_uint_u64(args, "--poll-ms", 400)?);
    config.keep = has_flag(args, "--keep");
    config.strict = has_flag(args, "--strict");

    let run = leak_http::run(config)
        .await
        .map_err(|e| EvalError::Failed(e.to_string()))?;
    let matrix = &run.matrix;

    print_leak_matrix(matrix);
    println!("wrote {}", run.out_dir.join("leak-report.md").display());
    println!("wrote {}", run.out_dir.join("leak.json").display());
    for note in &matrix.notes {
        println!("note: {note}");
    }

    if run.failed {
        let detail = if matrix.any_leak() {
            format!(
                "{} leak(s) across {} probes — see {}",
                matrix.total_leaks(),
                matrix.total_probes(),
                run.out_dir.join("leak-report.md").display()
            )
        } else {
            format!(
                "{} arm(s) UNTESTED; --strict makes that a failure",
                matrix
                    .arms
                    .iter()
                    .filter(|a| a.status == kb_eval_status_untested())
                    .count()
            )
        };
        return Err(EvalError::Failed(detail));
    }
    Ok(())
}

/// The one comparison the CLI needs, so it does not have to import the enum's
/// variants by name in a match it would only use once.
fn kb_eval_status_untested() -> crate::leak::Status {
    crate::leak::Status::Untested
}

/// Print the arm × scope matrix as a table, one line per arm.
fn print_leak_matrix(matrix: &crate::leak::LeakMatrix) {
    println!(
        "leak matrix: {} probes, {} leak(s)",
        matrix.total_probes(),
        matrix.total_leaks()
    );
    let header: String = ScopeKind::ALL.iter().fold(
        String::from("| arm | probes | leaks | positive | status |"),
        |mut acc, k| {
            acc.push_str(&format!("{} | ", k.as_str()));
            acc
        },
    );
    println!("{header}");
    println!("|---|---:|---:|---|---|---|---|---|");
    for arm in Arm::ALL {
        let Some(report) = matrix.arms.iter().find(|a| a.arm == arm) else {
            continue;
        };
        let cells: String = ScopeKind::ALL
            .iter()
            .map(
                |kind| match report.cells.iter().find(|c| c.scope == *kind) {
                    Some(cell) => format!("{} | ", cell.status.as_str()),
                    None => "n/a | ".to_string(),
                },
            )
            .collect();
        println!(
            "| {} | {} | {} | {}/{} | {} | {cells}",
            arm.as_str(),
            report.probes,
            report.leaks,
            report.positive_hits,
            report.positive_probes,
            report.status.as_str()
        );
    }
}

fn score(args: &[String]) -> Result<(), EvalError> {
    let qrels_path =
        flag(args, "--qrels").ok_or_else(|| EvalError::Usage("missing --qrels".to_string()))?;
    let run_path =
        flag(args, "--run").ok_or_else(|| EvalError::Usage("missing --run".to_string()))?;
    let k = parse_uint(args, "--k", 10)?;
    let gain = parse_gain(args)?;

    let qrels_text = std::fs::read_to_string(&qrels_path)
        .map_err(|e| EvalError::Failed(format!("could not read {qrels_path}: {e}")))?;
    let qrels = parse_qrels(&qrels_text)
        .map_err(|e| EvalError::Failed(format!("could not parse {qrels_path}: {e}")))?;
    let run_text = std::fs::read_to_string(&run_path)
        .map_err(|e| EvalError::Failed(format!("could not read {run_path}: {e}")))?;
    let rankings = parse_trec(&run_text)
        .map_err(|e| EvalError::Failed(format!("could not parse {run_path}: {e}")))?;

    let markdown = score_offline(&qrels, &rankings, k, gain);
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

/// `--gain linear|exponential`. Absent means the default, linear, so a run file
/// scored with no flag is comparable with trec_eval and BEIR.
fn parse_gain(args: &[String]) -> Result<Gain, EvalError> {
    match flag(args, "--gain") {
        None => Ok(Gain::default()),
        Some(value) => Gain::parse(&value).ok_or_else(|| {
            EvalError::Usage(format!(
                "--gain expects linear or exponential, got {value:?}"
            ))
        }),
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

    #[test]
    fn gain_defaults_to_linear_and_accepts_exponential() {
        assert_eq!(parse_gain(&args(&[])).unwrap(), Gain::Linear);
        assert_eq!(
            parse_gain(&args(&["--gain", "linear"])).unwrap(),
            Gain::Linear
        );
        assert_eq!(
            parse_gain(&args(&["--gain", "exponential"])).unwrap(),
            Gain::Exponential
        );
        assert!(matches!(
            parse_gain(&args(&["--gain", "quadratic"])),
            Err(EvalError::Usage(_))
        ));
    }

    fn metrics(n_failed: usize, n_ingest_failed: usize) -> MetricsFile {
        MetricsFile {
            label: "run".to_string(),
            dataset: "tiny".to_string(),
            timestamp: "2026-10-16T00:00:00Z".to_string(),
            harness_commit: "abc1234".to_string(),
            gain: "linear".to_string(),
            server_version: Some("dev".to_string()),
            server_commit: None,
            k: 10,
            max_chunks: 50,
            n_queries: 12,
            n_failed,
            n_degraded: 0,
            n_noted: 0,
            n_documents: 6,
            n_ingest_failed,
            degrades: Vec::new(),
            failures: Vec::new(),
            document: crate::metrics::aggregate_level(&[], &[]),
            page: crate::metrics::aggregate_level(&[], &[]),
            latency_ms: crate::metrics::latency_report(Vec::new()),
        }
    }

    #[test]
    fn a_run_with_failures_exits_non_zero_unless_allowed() {
        assert!(gate_on_failures(&metrics(0, 0), false).is_ok());
        // One query the server could not answer is enough.
        assert!(matches!(
            gate_on_failures(&metrics(1, 0), false),
            Err(EvalError::Failed(_))
        ));
        // So is one corpus file that never ingested.
        assert!(matches!(
            gate_on_failures(&metrics(0, 1), false),
            Err(EvalError::Failed(_))
        ));
        // The opt-out keeps the exit at zero, and says what it saw.
        assert!(gate_on_failures(&metrics(3, 2), true).is_ok());
        let err = gate_on_failures(&metrics(3, 2), false)
            .expect_err("a failed run must not exit zero")
            .to_string();
        assert!(err.contains("--allow-failures"), "{err}");
        assert!(err.contains("3 of 12 queries"), "{err}");
        assert!(err.contains("2 corpus file(s)"), "{err}");
    }

    #[tokio::test]
    async fn missing_required_flags_are_usage_errors() {
        let err = eval(&args(&["--dataset", "d"])).await.unwrap_err();
        assert!(matches!(err, EvalError::Usage(_)), "{err:?}");
    }
}
