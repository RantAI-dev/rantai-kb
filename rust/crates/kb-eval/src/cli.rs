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
        Some("compare") => compare(&args[1..]),
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
    kb-server eval compare --baseline FILE --current FILE [--max-drop 0.02] [--metric ndcg,hit1,recall]

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
    leak.json        the same, machine-readable, plus the seeded corpus

COMPARE FLAGS
    --baseline FILE     baseline metrics.json to compare against (required)
    --current FILE      current metrics.json to compare against (required)
    --max-drop DROP     maximum allowed absolute drop for strict means (default 0.02)
    --metric LIST       comma-separated metric list (default ndcg,hit1,recall)"
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
                    .filter(|a| matches!(
                        a.status,
                        crate::leak::Status::Untested | crate::leak::Status::Partial
                    ))
                    .count()
            )
        };
        return Err(EvalError::Failed(detail));
    }
    Ok(())
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

fn compare(args: &[String]) -> Result<(), EvalError> {
    let baseline_path = flag(args, "--baseline")
        .ok_or_else(|| EvalError::Usage("missing --baseline".to_string()))?;
    let current_path =
        flag(args, "--current").ok_or_else(|| EvalError::Usage("missing --current".to_string()))?;
    let max_drop = parse_f64(args, "--max-drop", 0.02)?;
    let metric_str = flag(args, "--metric").unwrap_or_else(|| "ndcg,hit1,recall".to_string());
    let metrics: Vec<&str> = metric_str
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if metrics.is_empty() {
        return Err(EvalError::Usage("--metric cannot be empty".to_string()));
    }

    let baseline = MetricsFile::load(std::path::Path::new(&baseline_path))
        .map_err(|e| EvalError::Failed(format!("could not read baseline {baseline_path}: {e}")))?;
    let current = MetricsFile::load(std::path::Path::new(&current_path))
        .map_err(|e| EvalError::Failed(format!("could not read current {current_path}: {e}")))?;

    let (table, violations) = evaluate_comparison(&baseline, &current, max_drop, &metrics)?;
    print!("{table}");

    if !violations.is_empty() {
        return Err(EvalError::Failed(format!(
            "comparison failed:
  {}",
            violations.join(
                "
  "
            )
        )));
    }
    Ok(())
}

/// Compare baseline vs current metrics and generate a markdown delta table.
///
/// Refuses comparison if dataset, k cutoff, or gain differ.
/// Fails if `n_failed` or `n_ingest_failed` increased, or if any chosen
/// strict mean drops by more than `max_drop` absolute.
pub fn evaluate_comparison(
    baseline: &MetricsFile,
    current: &MetricsFile,
    max_drop: f64,
    metrics: &[&str],
) -> Result<(String, Vec<String>), EvalError> {
    if baseline.dataset != current.dataset {
        return Err(EvalError::Failed(format!(
            "refusing to compare different datasets: baseline '{}' vs current '{}'",
            baseline.dataset, current.dataset
        )));
    }
    if baseline.k != current.k {
        return Err(EvalError::Failed(format!(
            "refusing to compare different k cutoffs: baseline {} vs current {}",
            baseline.k, current.k
        )));
    }
    if baseline.gain != current.gain {
        return Err(EvalError::Failed(format!(
            "refusing to compare different nDCG gains: baseline '{}' vs current '{}'",
            baseline.gain, current.gain
        )));
    }

    let mut violations = Vec::new();
    if current.n_failed > baseline.n_failed {
        violations.push(format!(
            "failed queries increased: {} in baseline -> {} in current",
            baseline.n_failed, current.n_failed
        ));
    }
    if current.n_ingest_failed > baseline.n_ingest_failed {
        violations.push(format!(
            "ingest failures increased: {} in baseline -> {} in current",
            baseline.n_ingest_failed, current.n_ingest_failed
        ));
    }

    let mut table = String::from(
        "| level | metric | baseline | current | delta |
|---|---|---:|---:|---:|
",
    );

    let levels = [
        ("document", &baseline.document, &current.document),
        ("page", &baseline.page, &current.page),
    ];

    for (level_name, b_level, c_level) in levels {
        for &metric in metrics {
            let b_val = get_metric_mean(b_level, metric)?;
            let c_val = get_metric_mean(c_level, metric)?;
            let delta = c_val - b_val;
            let clean_delta = if delta.abs() < 1e-9 { 0.0 } else { delta };
            table.push_str(&format!(
                "| {} | {} | {:.3} | {:.3} | {:+.3} |
",
                level_name, metric, b_val, c_val, clean_delta
            ));

            let drop = b_val - c_val;
            if drop > max_drop + 1e-9 {
                violations.push(format!(
                    "{level_name} {metric} dropped by {drop:.3} (from {b_val:.3} to {c_val:.3}, max allowed drop: {max_drop:.3})"
                ));
            }
        }
    }

    Ok((table, violations))
}

fn get_metric_mean(level: &crate::metrics::LevelReport, metric: &str) -> Result<f64, EvalError> {
    match metric.to_ascii_lowercase().as_str() {
        "ndcg" | "ndcg@k" => Ok(level.ndcg.mean),
        "recall" | "recall@k" => Ok(level.recall.mean),
        "mrr" | "mrr@k" => Ok(level.mrr.mean),
        "hit1" | "hit@1" => Ok(level.hit1.mean),
        "precision" | "precision@k" | "p" | "p@k" => Ok(level.precision.mean),
        other => Err(EvalError::Usage(format!(
            "unknown metric: {other:?} (supported: ndcg, hit1, recall, mrr, precision)"
        ))),
    }
}

fn parse_f64(args: &[String], name: &str, default: f64) -> Result<f64, EvalError> {
    match flag(args, name) {
        None => Ok(default),
        Some(value) => value
            .parse()
            .map_err(|_| EvalError::Usage(format!("{name} expects a number, got {value:?}"))),
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
    use crate::metrics::{Aggregate, LatencyReport, LevelReport, Means};

    fn test_metrics_file(
        dataset: &str,
        k: usize,
        gain: &str,
        doc_ndcg: f64,
        page_ndcg: f64,
        n_failed: usize,
        n_ingest_failed: usize,
    ) -> MetricsFile {
        let make_level = |ndcg: f64| LevelReport {
            ndcg: Aggregate {
                mean: ndcg,
                ci_low: ndcg,
                ci_high: ndcg,
                n: 10,
            },
            recall: Aggregate {
                mean: 0.8,
                ci_low: 0.8,
                ci_high: 0.8,
                n: 10,
            },
            mrr: Aggregate {
                mean: 0.5,
                ci_low: 0.5,
                ci_high: 0.5,
                n: 10,
            },
            hit1: Aggregate {
                mean: 0.4,
                ci_low: 0.4,
                ci_high: 0.4,
                n: 10,
            },
            precision: Aggregate {
                mean: 0.2,
                ci_low: 0.2,
                ci_high: 0.2,
                n: 10,
            },
            n_queries: 10,
            answered: Means {
                ndcg: Aggregate {
                    mean: ndcg,
                    ci_low: ndcg,
                    ci_high: ndcg,
                    n: 10,
                },
                recall: Aggregate {
                    mean: 0.8,
                    ci_low: 0.8,
                    ci_high: 0.8,
                    n: 10,
                },
                mrr: Aggregate {
                    mean: 0.5,
                    ci_low: 0.5,
                    ci_high: 0.5,
                    n: 10,
                },
                hit1: Aggregate {
                    mean: 0.4,
                    ci_low: 0.4,
                    ci_high: 0.4,
                    n: 10,
                },
                precision: Aggregate {
                    mean: 0.2,
                    ci_low: 0.2,
                    ci_high: 0.2,
                    n: 10,
                },
                n_queries: 10,
            },
        };

        MetricsFile {
            label: "test".to_string(),
            dataset: dataset.to_string(),
            timestamp: "2026-10-16T00:00:00Z".to_string(),
            harness_commit: "commit".to_string(),
            gain: gain.to_string(),
            server_version: Some("dev".to_string()),
            server_commit: None,
            k,
            max_chunks: 50,
            n_queries: 10,
            n_failed,
            n_degraded: 0,
            n_noted: 0,
            n_documents: 5,
            n_ingest_failed,
            degrades: Vec::new(),
            failures: Vec::new(),
            document: make_level(doc_ndcg),
            page: make_level(page_ndcg),
            latency_ms: LatencyReport {
                mean: 10.0,
                p50: 10.0,
                p95: 10.0,
                p99: 10.0,
                n: 10,
            },
        }
    }

    #[test]
    fn compare_passes_when_identical_or_better() {
        let b = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 0);
        let c = test_metrics_file("tiny", 10, "linear", 0.51, 0.50, 0, 0);
        let (table, violations) =
            evaluate_comparison(&b, &c, 0.02, &["ndcg", "hit1", "recall"]).unwrap();
        assert!(violations.is_empty(), "{violations:?}");
        assert!(table.contains("| document | ndcg | 0.500 | 0.510 | +0.010 |"));
        assert!(table.contains("| page | ndcg | 0.500 | 0.500 | +0.000 |"));
    }

    #[test]
    fn compare_fails_when_document_strict_mean_drops_more_than_max_drop() {
        let b = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 0);
        let c = test_metrics_file("tiny", 10, "linear", 0.47, 0.50, 0, 0);
        let (_, violations) =
            evaluate_comparison(&b, &c, 0.02, &["ndcg", "hit1", "recall"]).unwrap();
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("document ndcg dropped by 0.030"));
    }

    #[test]
    fn compare_fails_when_page_strict_mean_drops_more_than_max_drop() {
        let b = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 0);
        let c = test_metrics_file("tiny", 10, "linear", 0.50, 0.45, 0, 0);
        let (_, violations) =
            evaluate_comparison(&b, &c, 0.02, &["ndcg", "hit1", "recall"]).unwrap();
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("page ndcg dropped by 0.050"));
    }

    #[test]
    fn compare_refuses_different_datasets() {
        let b = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 0);
        let c = test_metrics_file("beir-scifact", 10, "linear", 0.50, 0.50, 0, 0);
        let err = evaluate_comparison(&b, &c, 0.02, &["ndcg"]).unwrap_err();
        assert!(matches!(err, EvalError::Failed(msg) if msg.contains("different datasets")));
    }

    #[test]
    fn compare_refuses_different_k() {
        let b = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 0);
        let c = test_metrics_file("tiny", 20, "linear", 0.50, 0.50, 0, 0);
        let err = evaluate_comparison(&b, &c, 0.02, &["ndcg"]).unwrap_err();
        assert!(matches!(err, EvalError::Failed(msg) if msg.contains("different k cutoffs")));
    }

    #[test]
    fn compare_refuses_different_gain() {
        let b = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 0);
        let c = test_metrics_file("tiny", 10, "exponential", 0.50, 0.50, 0, 0);
        let err = evaluate_comparison(&b, &c, 0.02, &["ndcg"]).unwrap_err();
        assert!(matches!(err, EvalError::Failed(msg) if msg.contains("different nDCG gains")));
    }

    #[test]
    fn compare_fails_when_failed_queries_increase() {
        let b = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 0);
        let c = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 1, 0);
        let (_, violations) = evaluate_comparison(&b, &c, 0.02, &["ndcg"]).unwrap();
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("failed queries increased"));
    }

    #[test]
    fn compare_fails_when_ingest_failures_increase() {
        let b = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 0);
        let c = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 2);
        let (_, violations) = evaluate_comparison(&b, &c, 0.02, &["ndcg"]).unwrap();
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("ingest failures increased"));
    }

    #[test]
    fn compare_cli_from_files_and_custom_options() {
        let dir = std::env::temp_dir().join(format!("kb-eval-compare-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let b_path = dir.join("baseline.json");
        let c_path = dir.join("current.json");

        let b = test_metrics_file("tiny", 10, "linear", 0.50, 0.50, 0, 0);
        let c = test_metrics_file("tiny", 10, "linear", 0.46, 0.50, 0, 0);
        std::fs::write(&b_path, serde_json::to_string(&b).unwrap()).unwrap();
        std::fs::write(&c_path, serde_json::to_string(&c).unwrap()).unwrap();

        // Drop is 0.04. With default max-drop (0.02), it fails.
        let err = compare(&args(&[
            "--baseline",
            b_path.to_str().unwrap(),
            "--current",
            c_path.to_str().unwrap(),
            "--metric",
            "ndcg",
        ]))
        .unwrap_err();
        assert!(matches!(err, EvalError::Failed(msg) if msg.contains("dropped by 0.040")));

        // With max-drop 0.05, it passes.
        let ok = compare(&args(&[
            "--baseline",
            b_path.to_str().unwrap(),
            "--current",
            c_path.to_str().unwrap(),
            "--max-drop",
            "0.05",
            "--metric",
            "ndcg",
        ]));
        assert!(ok.is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
