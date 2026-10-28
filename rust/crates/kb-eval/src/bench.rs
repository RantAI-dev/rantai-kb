//! `kb-server bench search` — a closed-loop load generator for `/v1/search`.
//!
//! The evaluation harness answers "was the answer right". This one answers "what
//! does a request cost", which is the other half of the same question: a
//! pipeline that ranks the right document first in 2.3 seconds is not what
//! anybody calls usable, and the plan's target (`p95 < 300 ms` with rerank,
//! `< 80 ms` without) can only be checked against a number produced the same
//! way every time.
//!
//! It is deliberately black box, like the rest of `kb-eval`: it knows the HTTP
//! API and nothing else. The latencies it reports are therefore *client-side*,
//! and the server's own per-stage breakdown is read off each response
//! (`stats.stageTimingsMs`) and reported next to them, so the difference between
//! "the store is slow" and "the network was slow" stays visible.
//!
//! Closed loop, because that is what a service actually faces: every worker has
//! one request in flight and sends the next when the last answers. Throughput
//! and latency are then two views of the same run — as concurrency rises,
//! latency rises only if the server is the bottleneck.
//!
//! Errors are counted, never dropped: a load test that silently discards failed
//! requests reports the latency of the easy ones.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::metrics::{LatencyReport, from_histogram};
use crate::stages::parse_stage_timings;

/// Microseconds, the unit the histogram records in. A request slower than an
/// hour is not a latency measurement, it is an outage, and is clamped rather
/// than dropped so it shows up in `max`.
const MAX_LATENCY_US: u64 = 3_600_000_000;
const SIGFIG: u8 = 3;

/// Clamp a measurement into the histogram's range.
///
/// A request that came back in under a microsecond measures 0, and the
/// histogram's lowest trackable value is 1 — recording 0 is refused and the
/// request would go missing from the report, which is the one thing this module
/// must not do. 1 µs is also the truth to within its own resolution.
fn clamp_us(us: u64) -> u64 {
    us.clamp(1, MAX_LATENCY_US)
}

/// A usage mistake (`Usage`) versus the run failing (`Failed`).
#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Failed(String),
}

impl From<io::Error> for BenchError {
    fn from(err: io::Error) -> Self {
        Self::Failed(err.to_string())
    }
}

impl From<reqwest::Error> for BenchError {
    fn from(err: reqwest::Error) -> Self {
        Self::Failed(err.to_string())
    }
}

/// What to measure.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    pub base_url: String,
    pub api_key: String,
    /// The queries to cycle through, in order.
    pub queries: Vec<String>,
    /// Concurrency levels to run, one after another.
    pub concurrency: Vec<usize>,
    /// How long each level runs. `None` when `--requests` decides instead.
    pub duration: Option<Duration>,
    /// Total requests per level. `None` when the duration decides.
    pub requests: Option<u64>,
    /// Requests issued before measuring starts, and thrown away.
    pub warmup: u64,
    pub max_chunks: u32,
    pub timeout: Duration,
    pub out_dir: PathBuf,
    pub label: String,
}

impl BenchConfig {
    /// A configuration with the defaults an operator wants: 30 s per level at
    /// concurrency 1 and 8, five warm-up requests.
    #[must_use]
    pub fn new(base_url: &str, api_key: &str, queries: Vec<String>) -> Self {
        Self {
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
            queries,
            concurrency: vec![1, 8],
            duration: Some(Duration::from_secs(30)),
            requests: None,
            warmup: 5,
            max_chunks: 50,
            timeout: Duration::from_secs(120),
            out_dir: PathBuf::from("eval-out/bench"),
            label: "bench".to_string(),
        }
    }
}

/// One concurrency level's result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LevelResult {
    pub concurrency: usize,
    /// Requests that completed, successes and failures both.
    pub requests: u64,
    /// Requests that did not answer with 2xx, or did not answer at all.
    pub errors: u64,
    pub duration_s: f64,
    pub throughput_rps: f64,
    pub latency_ms: LatencyReport,
    pub latency_max_ms: f64,
    pub latency_min_ms: f64,
    /// Per-stage p95, read off `stats.stageTimingsMs`.
    pub stage_p95_ms: BTreeMap<String, f64>,
    /// Per-stage p50, for symmetry with the p95 above.
    pub stage_p50_ms: BTreeMap<String, f64>,
    /// HTTP statuses seen, so a load test that "passed" on 503s cannot.
    pub statuses: BTreeMap<String, u64>,
    /// The first few errors, verbatim. A failure nobody can read is a failure
    /// nobody can fix.
    pub error_samples: Vec<String>,
    /// Responses that reported per-stage timings.
    pub stage_reports: u64,
    /// Responses that answered 2xx *with a degrade*: an arm failed and the
    /// other one answered alone, or nobody did. Counted separately from errors
    /// because the status is 200 and the body looks like a normal answer — a
    /// load test that calls those a clean sheet is measuring the error path
    /// and calling it a result.
    pub degraded: u64,
    /// The degrade labels the server reported.
    pub degrades: BTreeMap<String, u64>,
}

/// The whole bench run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchReport {
    pub label: String,
    pub timestamp: String,
    pub base_url: String,
    pub n_queries: usize,
    pub max_chunks: u32,
    pub warmup_requests: u64,
    pub harness_commit: String,
    pub levels: Vec<LevelResult>,
}

impl BenchReport {
    /// Total requests and errors across every level.
    #[must_use]
    pub fn totals(&self) -> (u64, u64) {
        self.levels
            .iter()
            .fold((0, 0), |(r, e), l| (r + l.requests, e + l.errors))
    }
}

fn new_histogram() -> Result<Histogram<u64>, hdrhistogram::CreationError> {
    Histogram::new_with_bounds(1, MAX_LATENCY_US, SIGFIG)
}

/// Run the whole bench: warm up, then every concurrency level in turn.
///
/// # Errors
///
/// [`BenchError`] when a histogram cannot be allocated, a request cannot be
/// built, or the outputs cannot be written.
pub async fn run(config: BenchConfig) -> Result<BenchReport, BenchError> {
    if config.queries.is_empty() {
        return Err(BenchError::Usage("no queries to search with".into()));
    }
    if config.concurrency.contains(&0) {
        return Err(BenchError::Usage("--concurrency must be at least 1".into()));
    }
    let client = reqwest::Client::builder().timeout(config.timeout).build()?;

    if config.warmup > 0 {
        warm_up(&client, &config).await?;
    }

    let mut levels = Vec::with_capacity(config.concurrency.len());
    for concurrency in config.concurrency.clone() {
        levels.push(run_level(&client, &config, concurrency).await?);
    }

    let report = BenchReport {
        label: config.label.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        base_url: config.base_url.clone(),
        n_queries: config.queries.len(),
        max_chunks: config.max_chunks,
        warmup_requests: config.warmup,
        harness_commit: crate::report::harness_commit(),
        levels,
    };
    write_outputs(&config.out_dir, &report)?;
    Ok(report)
}

/// Issue the warm-up requests and throw the results away.
///
/// The first request to a server pays for connection setup, page faults and a
/// cold embedder; including it would make every number a statement about the
/// first request.
async fn warm_up(client: &reqwest::Client, config: &BenchConfig) -> Result<(), BenchError> {
    for i in 0..config.warmup {
        let query = &config.queries[(i as usize) % config.queries.len()];
        if let Err(failure) = send(client, config, query).await {
            return Err(BenchError::Failed(format!(
                "warm-up request {i} failed: {}; a bench against a server that answers an \
                 error to the first query is measuring the error path",
                failure.message
            )));
        }
    }
    Ok(())
}

/// One request, with the outcome split into "answered with 2xx" and everything
/// else. A non-2xx is returned as a value, not an error, so it is counted in the
/// same bucket as a timeout.
async fn send(
    client: &reqwest::Client,
    config: &BenchConfig,
    query: &str,
) -> Result<(u16, serde_json::Value), RequestFailure> {
    let url = format!("{}/v1/search", config.base_url.trim_end_matches('/'));
    let response = client
        .post(&url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", config.api_key),
        )
        .json(&serde_json::json!({
            "query": query,
            "maxChunks": config.max_chunks,
        }))
        .send()
        .await
        .map_err(|e| RequestFailure {
            status: None,
            message: format!("transport: {e}"),
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|e| RequestFailure {
        status: Some(status.as_u16()),
        message: format!("body: {e}"),
    })?;
    if !status.is_success() {
        let head: String = body.chars().take(200).collect();
        return Err(RequestFailure {
            status: Some(status.as_u16()),
            message: format!("HTTP {}: {head}", status.as_u16()),
        });
    }
    serde_json::from_str(&body)
        .map(|value| (status.as_u16(), value))
        .map_err(|e| RequestFailure {
            status: Some(status.as_u16()),
            message: format!("unreadable body: {e}"),
        })
}

/// A request that did not answer with a 2xx, or did not answer at all.
///
/// Counted, sampled, and recorded in the latency histogram like any other
/// request: a failed request is not a fast request, and dropping it would make
/// a broken server look quick.
struct RequestFailure {
    status: Option<u16>,
    message: String,
}

/// Run one concurrency level until its request budget or its duration is spent.
async fn run_level(
    client: &reqwest::Client,
    config: &BenchConfig,
    concurrency: usize,
) -> Result<LevelResult, BenchError> {
    // One histogram behind a mutex, not one per worker: percentiles over a
    // merged run are the run's percentiles, and a per-worker histogram could
    // not answer "what is the p95 of this level" at all.
    let hist = Arc::new(Mutex::new(
        new_histogram().map_err(|e| BenchError::Failed(format!("histogram: {e}")))?,
    ));
    let stages: Arc<Mutex<BTreeMap<String, Histogram<u64>>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    let statuses: Arc<Mutex<BTreeMap<String, u64>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(AtomicU64::new(0));
    let error_count = Arc::new(AtomicU64::new(0));
    let degraded_count = Arc::new(AtomicU64::new(0));
    let degrades: Arc<Mutex<BTreeMap<String, u64>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let stage_reports = Arc::new(AtomicU64::new(0));
    let next_query = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));

    let started = Instant::now();
    let deadline = config.duration.map(|d| started + d);
    let budget = config.requests;

    let mut tasks = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let client = client.clone();
        let config = config.clone();
        let hist = Arc::clone(&hist);
        let stages = Arc::clone(&stages);
        let statuses = Arc::clone(&statuses);
        let errors = Arc::clone(&errors);
        let requests = Arc::clone(&requests);
        let error_count = Arc::clone(&error_count);
        let degraded_count = Arc::clone(&degraded_count);
        let degrades = Arc::clone(&degrades);
        let stage_reports = Arc::clone(&stage_reports);
        let next_query = Arc::clone(&next_query);
        let done = Arc::clone(&done);
        tasks.push(tokio::spawn(async move {
            let worker = async {
                loop {
                    if done.load(Ordering::Relaxed) {
                        break;
                    }
                    let seen = requests.fetch_add(1, Ordering::SeqCst);
                    if let Some(budget) = budget {
                        if seen >= budget {
                            done.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                    if let Some(deadline) = deadline {
                        if Instant::now() >= deadline {
                            done.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                    let index =
                        next_query.fetch_add(1, Ordering::SeqCst) as usize % config.queries.len();
                    let query = config.queries[index].clone();
                    let at = Instant::now();
                    match send(&client, &config, &query).await {
                        Ok((status, body)) => {
                            let elapsed_us = clamp_us(at.elapsed().as_micros() as u64);
                            let _ = hist.lock().await.record(elapsed_us);
                            bump(statuses.as_ref(), &status.to_string()).await;
                            let reported: Vec<String> = body
                                .get("degrades")
                                .and_then(|d| d.as_array())
                                .map(|labels| {
                                    labels
                                        .iter()
                                        .filter_map(|l| l.as_str())
                                        .map(ToString::to_string)
                                        .collect()
                                })
                                .unwrap_or_default();
                            if !reported.is_empty() {
                                degraded_count.fetch_add(1, Ordering::Relaxed);
                                let mut guard = degrades.lock().await;
                                for label in reported {
                                    *guard.entry(label).or_insert(0) += 1;
                                }
                            }
                            if let Some(sample) = parse_stage_timings(&body) {
                                stage_reports.fetch_add(1, Ordering::Relaxed);
                                let mut guard = stages.lock().await;
                                for (stage, ms) in &sample.stages {
                                    let value = clamp_us((ms * 1000.0) as u64);
                                    match guard.entry(stage.clone()) {
                                        Entry::Vacant(slot) => {
                                            let hist = new_histogram().map_err(|e| {
                                                BenchError::Failed(format!("stage histogram: {e}"))
                                            })?;
                                            let _ = slot.insert(hist);
                                            if let Some(h) = guard.get_mut(stage) {
                                                let _ = h.record(value);
                                            }
                                        }
                                        Entry::Occupied(mut slot) => {
                                            let _ = slot.get_mut().record(value);
                                        }
                                    }
                                }
                            }
                        }
                        Err(failure) => {
                            let elapsed_us = clamp_us(at.elapsed().as_micros() as u64);
                            let _ = hist.lock().await.record(elapsed_us);
                            error_count.fetch_add(1, Ordering::Relaxed);
                            let status_key = failure
                                .status
                                .map_or_else(|| "transport".to_string(), |s| s.to_string());
                            bump(statuses.as_ref(), &status_key).await;
                            let mut guard = errors.lock().await;
                            if guard.len() < 5 {
                                guard.push(failure.message);
                            }
                        }
                    }
                }
                Ok::<(), BenchError>(())
            };
            worker.await
        }));
    }
    for task in tasks {
        task.await
            .map_err(|e| BenchError::Failed(format!("a bench worker panicked: {e}")))??;
    }
    let elapsed_s = started.elapsed().as_secs_f64();
    let hist = hist.lock().await;
    // The counter is incremented before the request, so a worker that stopped on
    // the budget has one more than it sent; the histogram is what actually
    // answered, so it is the number of requests.
    let requests_made = hist.len();

    let stages = stages.lock().await;
    let mut stage_p95 = BTreeMap::new();
    let mut stage_p50 = BTreeMap::new();
    for (stage, hist) in stages.iter() {
        stage_p95.insert(stage.clone(), hist.value_at_quantile(0.95) as f64 / 1000.0);
        stage_p50.insert(stage.clone(), hist.value_at_quantile(0.50) as f64 / 1000.0);
    }

    Ok(LevelResult {
        concurrency,
        requests: requests_made,
        errors: error_count.load(Ordering::Relaxed),
        duration_s: elapsed_s,
        throughput_rps: if elapsed_s > 0.0 {
            requests_made as f64 / elapsed_s
        } else {
            0.0
        },
        latency_ms: from_histogram(&hist),
        latency_max_ms: hist.max() as f64 / 1000.0,
        latency_min_ms: hist.min() as f64 / 1000.0,
        stage_p95_ms: stage_p95,
        stage_p50_ms: stage_p50,
        statuses: statuses.lock().await.clone(),
        error_samples: errors.lock().await.clone(),
        stage_reports: stage_reports.load(Ordering::Relaxed),
        degraded: degraded_count.load(Ordering::Relaxed),
        degrades: degrades.lock().await.clone(),
    })
}

async fn bump(map: &Mutex<BTreeMap<String, u64>>, key: &str) {
    let mut guard = map.lock().await;
    *guard.entry(key.to_string()).or_insert(0) += 1;
}

/// Write `bench.json` and `bench.md` into `dir`.
///
/// # Errors
///
/// An I/O error from creating the directory or writing a file.
pub fn write_outputs(dir: &Path, report: &BenchReport) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join("bench.json"),
        serde_json::to_string_pretty(report).map_err(io::Error::other)?,
    )?;
    fs::write(dir.join("bench.md"), render_markdown(report))?;
    Ok(())
}

/// Render `bench.md`: one row per concurrency level, then the per-stage p95s
/// from the server's own timings.
#[must_use]
pub fn render_markdown(report: &BenchReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Search load test — {}\n\n", report.label));
    out.push_str(&format!("- target: `{}`\n", report.base_url));
    out.push_str(&format!("- harness commit: `{}`\n", report.harness_commit));
    out.push_str(&format!("- timestamp: {}\n", report.timestamp));
    out.push_str(&format!("- queries cycled: {}\n", report.n_queries));
    out.push_str(&format!("- max chunks per query: {}\n", report.max_chunks));
    out.push_str(&format!(
        "- warm-up requests: {} (discarded)\n",
        report.warmup_requests
    ));
    out.push('\n');
    out.push_str(
        "Closed loop: every worker holds one request in flight and sends the next when the\n\
         last answers. Latency is client-side (HTTP round trip plus JSON), so it is always at\n\
         least the server's own `total` below.\n\n",
    );
    out.push_str(
        "| concurrency | requests | errors | degraded | req/s | p50 ms | p95 ms | p99 ms | max ms |\n",
    );
    out.push_str("|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for level in &report.levels {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {:.2} | {:.1} | {:.1} | {:.1} | {:.1} |\n",
            level.concurrency,
            level.requests,
            level.errors,
            level.degraded,
            level.throughput_rps,
            level.latency_ms.p50,
            level.latency_ms.p95,
            level.latency_ms.p99,
            level.latency_max_ms,
        ));
    }
    let (requests, errors) = report.totals();
    let degraded: u64 = report.levels.iter().map(|l| l.degraded).sum();
    out.push_str(&format!(
        "\n{requests} requests, {errors} error(s), {degraded} degraded.\n"
    ));
    for level in &report.levels {
        if !level.degrades.is_empty() {
            out.push_str(&format!(
                "\nDegrades at concurrency {} (a 2xx answer with an arm down, not an error):\n{}\n",
                level.concurrency,
                level
                    .degrades
                    .iter()
                    .map(|(label, count)| format!("- {label} × {count}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        if !level.statuses.is_empty() {
            out.push_str(&format!(
                "\nHTTP statuses at concurrency {}: {}\n",
                level.concurrency,
                level
                    .statuses
                    .iter()
                    .map(|(status, count)| format!("{status} × {count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for sample in &level.error_samples {
            out.push_str(&format!("\n> error: {sample}\n"));
        }
    }

    out.push_str("\n## Server-side stage p95 (ms), from `stats.stageTimingsMs`\n\n");
    out.push_str(
        "`detail.*` rows are overlapping measurements — the query embed and both arms happen\n\
         inside `arms` — and are read, not summed.\n\n",
    );
    for level in &report.levels {
        if level.stage_p95_ms.is_empty() {
            out.push_str(&format!(
                "Concurrency {}: the server reported no per-stage timings.\n\n",
                level.concurrency
            ));
            continue;
        }
        out.push_str(&format!(
            "### concurrency {} ({} of {} responses reported timings)\n\n",
            level.concurrency, level.stage_reports, level.requests
        ));
        out.push_str("| stage | p50 ms | p95 ms |\n|---|---:|---:|\n");
        for (stage, p95) in &level.stage_p95_ms {
            let p50 = level.stage_p50_ms.get(stage).copied().unwrap_or(0.0);
            out.push_str(&format!("| {stage} | {p50:.1} | {p95:.1} |\n"));
        }
        out.push('\n');
    }
    out
}

/// Entry point for `kb-server bench search …`. `args` excludes `bench` and
/// `search`.
///
/// # Errors
///
/// [`BenchError::Usage`] for a bad argument list; [`BenchError::Failed`] when the
/// run cannot be produced or written.
pub async fn run_cli(args: &[String]) -> Result<(), BenchError> {
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print_help();
            return Ok(());
        }
        Some("search") | None => {}
        Some(other) => {
            return Err(BenchError::Usage(format!(
                "unknown bench subcommand {other:?}; the only one is `search`"
            )));
        }
    }
    let base_url = flag(args, "--base-url")
        .ok_or_else(|| BenchError::Usage("missing --base-url".to_string()))?;
    let api_key = flag(args, "--api-key")
        .or_else(|| std::env::var("KB_EVAL_API_KEY").ok())
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            BenchError::Usage("missing API key: pass --api-key or set KB_EVAL_API_KEY".to_string())
        })?;
    let queries = match (flag(args, "--queries"), flag(args, "--dataset")) {
        (Some(path), _) => load_queries(&PathBuf::from(path))?,
        (None, Some(dir)) => crate::dataset::Dataset::load(&dir)
            .map_err(|e| BenchError::Usage(format!("{dir}: {e}")))?
            .queries
            .into_iter()
            .map(|q| q.text)
            .collect(),
        (None, None) => {
            return Err(BenchError::Usage(
                "missing --queries FILE or --dataset DIR".to_string(),
            ));
        }
    };

    let mut config = BenchConfig::new(&base_url, &api_key, queries);
    config.max_chunks = parse_u32(args, "--max-chunks", config.max_chunks)?;
    config.out_dir =
        PathBuf::from(flag(args, "--out").unwrap_or_else(|| "eval-out/bench".to_string()));
    config.label = flag(args, "--label").unwrap_or_else(|| "bench".to_string());
    config.warmup = parse_u64(args, "--warmup", config.warmup)?;
    config.timeout = Duration::from_secs(parse_u64(args, "--timeout", 120)?);
    if let Some(list) = flag(args, "--concurrency") {
        let mut levels = Vec::new();
        for part in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let n: usize = part.parse().map_err(|_| {
                BenchError::Usage(format!("--concurrency {part:?} is not a number"))
            })?;
            if n == 0 {
                return Err(BenchError::Usage(
                    "--concurrency must be at least 1".to_string(),
                ));
            }
            levels.push(n);
        }
        if levels.is_empty() {
            return Err(BenchError::Usage("--concurrency is empty".to_string()));
        }
        config.concurrency = levels;
    }
    if let Some(raw) = flag(args, "--duration") {
        config.duration = Some(parse_duration(&raw)?);
    }
    config.requests = match flag(args, "--requests") {
        None => None,
        Some(raw) => Some(
            raw.trim()
                .parse()
                .map_err(|_| BenchError::Usage(format!("--requests {raw:?} is not a number")))?,
        ),
    };
    if config.duration.is_none() && config.requests.is_none() {
        config.duration = Some(Duration::from_secs(30));
    }

    let out_dir = flag(args, "--out").unwrap_or_else(|| "eval-out/bench".to_string());
    let report = run(config).await?;
    print_report(&report);
    println!("wrote {out_dir}/bench.json and {out_dir}/bench.md");
    let (_, errors) = report.totals();
    if errors > 0 {
        return Err(BenchError::Failed(format!(
            "{errors} of the requests failed; see the statuses and error samples in bench.md"
        )));
    }
    Ok(())
}

fn print_report(report: &BenchReport) {
    println!(
        "bench {} against {} — {} queries, maxChunks {}",
        report.label, report.base_url, report.n_queries, report.max_chunks
    );
    println!(
        "{:<4} {:>9} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "conc", "requests", "errors", "degraded", "req/s", "p50 ms", "p95 ms", "p99 ms"
    );
    for level in &report.levels {
        println!(
            "{:<4} {:>9} {:>7} {:>9} {:>9.2} {:>9.1} {:>9.1} {:>9.1}",
            level.concurrency,
            level.requests,
            level.errors,
            level.degraded,
            level.throughput_rps,
            level.latency_ms.p50,
            level.latency_ms.p95,
            level.latency_ms.p99
        );
    }
}

/// Read queries from a JSONL file: one object per line with the text in
/// `text`, `query` or `q`; a line that is not JSON is the query itself, because
/// a list of questions typed into a file is the obvious thing to reach for.
fn load_queries(path: &Path) -> Result<Vec<String>, BenchError> {
    let text = fs::read_to_string(path)
        .map_err(|e| BenchError::Usage(format!("could not read {}: {e}", path.display())))?;
    let mut queries = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let from_json = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| {
                ["text", "query", "q"]
                    .iter()
                    .find_map(|key| value.get(*key).and_then(|v| v.as_str()))
                    .map(ToString::to_string)
            });
        let query = from_json.unwrap_or_else(|| line.to_string());
        if query.trim().is_empty() {
            return Err(BenchError::Usage(format!(
                "{}:{} is an empty query",
                path.display(),
                n + 1
            )));
        }
        queries.push(query);
    }
    Ok(queries)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn parse_u64(args: &[String], name: &str, default: u64) -> Result<u64, BenchError> {
    match flag(args, name) {
        None => Ok(default),
        Some(raw) => raw
            .trim()
            .parse()
            .map_err(|_| BenchError::Usage(format!("{name} {raw:?} is not a whole number"))),
    }
}

fn parse_u32(args: &[String], name: &str, default: u32) -> Result<u32, BenchError> {
    match flag(args, name) {
        None => Ok(default),
        Some(raw) => raw
            .trim()
            .parse()
            .map_err(|_| BenchError::Usage(format!("{name} {raw:?} is not a number"))),
    }
}

/// Parse `30s`, `500ms`, `2m` or a bare number of seconds.
fn parse_duration(raw: &str) -> Result<Duration, BenchError> {
    let raw = raw.trim().to_ascii_lowercase();
    let (value, unit) = if raw.ends_with("ms") {
        (&raw[..raw.len() - 2], "ms")
    } else {
        match raw.chars().last() {
            Some('s') => (&raw[..raw.len() - 1], "s"),
            Some('m') => (&raw[..raw.len() - 1], "m"),
            _ => (raw.as_str(), "s"),
        }
    };
    let n: u64 = value
        .trim()
        .parse()
        .map_err(|_| BenchError::Usage(format!("--duration {raw:?} is not a duration")))?;
    Ok(match unit {
        "ms" => Duration::from_millis(n),
        "m" => Duration::from_secs(n * 60),
        _ => Duration::from_secs(n),
    })
}

fn print_help() {
    println!(
        "kb-server bench search — closed-loop load test for /v1/search

USAGE
    kb-server bench search --base-url URL (--queries FILE | --dataset DIR) [flags]

FLAGS
    --base-url URL      kb-server to load (required)
    --api-key KEY       API key; defaults to $KB_EVAL_API_KEY
    --queries FILE      JSONL of queries: {{\"text\": \"...\"}}, {{\"query\": ...}}
                        or {{\"q\": ...}}; a line that is not JSON is the query
    --dataset DIR       a BEIR/TREC dataset directory; its queries are used
    --concurrency LIST  comma-separated worker counts (default 1,8)
    --duration DURATION how long each level runs, e.g. 30s or 2m (default 30s)
    --requests N        requests per level instead of a duration
    --warmup N          requests issued and discarded first (default 5)
    --max-chunks N      chunks requested per query (default 50)
    --timeout SECS      per-request timeout (default 120)
    --out DIR           output directory (default eval-out/bench)
    --label LABEL       run label (default bench)

OUTPUTS (in --out)
    bench.json  every level, its latencies and its per-stage p95s
    bench.md    the same as a table"
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A minimal `/v1/search` on a raw socket.
    ///
    /// `kb-eval` is a black-box client and links no framework the service uses;
    /// its tests answer over a hand-written HTTP response for the same reason,
    /// so a passing bench test cannot be a passing axum test.
    /// The query string out of a raw request body, without a JSON parser:
    /// enough to answer with a body that echoes it.
    fn extract_query(request: &str) -> String {
        let marker = "\"query\":\"";
        request
            .split(marker)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default()
            .to_string()
    }

    async fn server(hits: Arc<AtomicUsize>, fail_every: usize) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let hits = Arc::clone(&hits);
                tokio::spawn(async move {
                    let mut buf = [0u8; 8192];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    let seen = hits.fetch_add(1, Ordering::SeqCst) + 1;
                    let failing = fail_every > 0 && seen % fail_every == 0;
                    let (status_line, body) = if failing {
                        (
                            "500 Internal Server Error",
                            r#"{"error":"boom"}"#.to_string(),
                        )
                    } else {
                        let query = extract_query(&request);
                        (
                            "200 OK",
                            serde_json::json!({
                                "chunks": [{"id": "c1", "content": query}],
                                "stats": {
                                    "totalTimeMs": 12.0,
                                    "stageTimingsMs": {
                                        "scopeBuild": 1.0,
                                        "arms": 9.0,
                                        "rerank": 0.0,
                                        "total": 12.5,
                                        "detail": {
                                            "engineTotal": 12.0,
                                            "queryEmbed": 7.0,
                                            "queryEmbedCached": query == "cached",
                                            "vectorArm": 8.0,
                                            "keywordArm": 1.0
                                        }
                                    }
                                }
                            })
                            .to_string(),
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn config(base_url: &str, queries: &[&str]) -> BenchConfig {
        BenchConfig::new(
            base_url,
            "test-key",
            queries.iter().map(|s| (*s).to_string()).collect(),
        )
    }

    #[tokio::test]
    async fn a_run_reports_latency_throughput_and_no_errors() {
        let hits = Arc::new(AtomicUsize::new(0));
        let url = server(Arc::clone(&hits), 0).await;
        let mut cfg = config(&url, &["apa itu rantai", "apa itu rantai-kb"]);
        cfg.warmup = 2;
        cfg.requests = Some(20);
        cfg.concurrency = vec![1, 4];
        cfg.out_dir = PathBuf::from("/tmp/kb-bench-test-clean");

        let report = run(cfg).await.expect("bench runs");
        assert_eq!(report.levels.len(), 2);
        for level in &report.levels {
            assert_eq!(level.requests, 20, "every request is accounted for");
            assert_eq!(level.errors, 0);
            assert!(level.latency_ms.n > 0);
            assert!(level.throughput_rps > 0.0);
        }
        // Warm-up requests are not measured.
        assert_eq!(hits.load(Ordering::SeqCst), 42);
        assert_eq!(report.warmup_requests, 2);
    }

    #[tokio::test]
    async fn the_server_stage_p95_is_read_off_every_response() {
        let hits = Arc::new(AtomicUsize::new(0));
        let url = server(Arc::clone(&hits), 0).await;
        let mut cfg = config(&url, &["cached", "fresh"]);
        cfg.warmup = 0;
        cfg.requests = Some(6);
        cfg.concurrency = vec![2];

        let report = run(cfg).await.expect("bench runs");
        let level = &report.levels[0];
        assert_eq!(level.stage_reports, 6);
        // An hdrhistogram reports the top of the bucket a value falls in, so
        // these are within its own resolution (0.1% at three significant
        // digits), not exact.
        for (stage, expected) in [
            ("arms", 9.0),
            ("detail.queryEmbed", 7.0),
            ("total", 12.5),
            ("detail.vectorArm", 8.0),
        ] {
            let actual = level.stage_p95_ms[stage];
            assert!(
                (actual - expected).abs() <= expected * 0.01,
                "{stage}: {actual} is not within 1% of {expected}"
            );
        }
        let scope_build = level.stage_p50_ms["scopeBuild"];
        assert!((scope_build - 1.0).abs() <= 0.02, "{scope_build}");
        assert!(
            !level.stage_p95_ms.contains_key("detail.queryEmbedCached"),
            "a cache flag is not a millisecond"
        );
    }

    /// Failures are counted, not dropped: a run where the server 500s must not
    /// report a clean sheet of latencies.
    #[tokio::test]
    async fn errors_are_counted_and_sampled() {
        let hits = Arc::new(AtomicUsize::new(0));
        let url = server(Arc::clone(&hits), 2).await;
        let mut cfg = config(&url, &["q"]);
        cfg.warmup = 0;
        cfg.requests = Some(6);
        cfg.concurrency = vec![1];

        let report = run(cfg).await.expect("bench runs");
        let level = &report.levels[0];
        assert_eq!(level.requests, 6);
        assert_eq!(level.errors, 3, "every third request fails");
        assert_eq!(level.statuses.get("500"), Some(&3));
        assert_eq!(level.error_samples.len(), 3);
        assert!(level.error_samples[0].contains("500"));
        // A failed request is still a request: its latency is recorded.
        assert_eq!(level.latency_ms.n, 6);
    }

    /// A 2xx answer with a degrade is not a clean answer: an arm was down and
    /// the response carries fewer results because of it. The bench counts those
    /// separately from errors, because the status is 200.
    #[tokio::test]
    async fn degraded_answers_are_counted_separately_from_errors() {
        let hits = Arc::new(AtomicUsize::new(0));
        let url = degrade_server(Arc::clone(&hits)).await;
        let mut cfg = config(&url, &["q"]);
        cfg.warmup = 0;
        cfg.requests = Some(4);
        cfg.concurrency = vec![1];

        let report = run(cfg).await.expect("bench runs");
        let level = &report.levels[0];
        assert_eq!(level.requests, 4);
        assert_eq!(level.errors, 0, "the status was 200");
        assert_eq!(level.degraded, 2);
        assert_eq!(level.degrades.get("vector_arm_failed"), Some(&2));
        assert!(render_markdown(&report).contains("vector_arm_failed × 2"));
    }

    /// Half the answers come back 200 with `degrades: ["vector_arm_failed"]`.
    async fn degrade_server(hits: Arc<AtomicUsize>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let hits = Arc::clone(&hits);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = socket.read(&mut buf).await;
                    let seen = hits.fetch_add(1, Ordering::SeqCst) + 1;
                    let body = if seen % 2 == 0 {
                        serde_json::json!({
                            "chunks": [],
                            "degrades": ["vector_arm_failed"],
                            "stats": {"stageTimingsMs": {"total": 1.0}}
                        })
                    } else {
                        serde_json::json!({
                            "chunks": [{"id": "c1"}],
                            "degrades": [],
                            "stats": {"stageTimingsMs": {"total": 1.0}}
                        })
                    }
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_server_that_fails_the_warmup_is_not_measured() {
        let hits = Arc::new(AtomicUsize::new(0));
        let url = server(Arc::clone(&hits), 1).await;
        let mut cfg = config(&url, &["q"]);
        cfg.warmup = 1;
        cfg.requests = Some(3);

        let err = run(cfg).await.expect_err("the warm-up failed");
        assert!(
            matches!(err, BenchError::Failed(_)),
            "a warm-up failure is a failure, not a usage mistake"
        );
    }

    #[test]
    fn a_jsonl_query_file_is_read_in_order_and_ignores_the_rest() {
        let path = std::env::temp_dir().join("kb-bench-queries.jsonl");
        fs::write(
            &path,
            "{\"text\": \"pertama\"}\n\n{\"query\": \"kedua\"}\n{\"q\": \"ketiga\"}\n",
        )
        .unwrap();
        let queries = load_queries(&path).unwrap();
        assert_eq!(queries, vec!["pertama", "kedua", "ketiga"]);
    }

    #[test]
    fn a_jsonl_line_can_be_the_query_itself() {
        let path = std::env::temp_dir().join("kb-bench-plain.txt");
        fs::write(
            &path,
            "# a comment\n\nplain text query\n{\"q\": \"json query\"}\n",
        )
        .unwrap();
        let queries = load_queries(&path).unwrap();
        assert_eq!(queries, vec!["plain text query", "json query"]);
    }

    #[test]
    fn durations_are_read_in_every_unit_an_operator_types() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
        assert!(parse_duration("soon").is_err());
    }

    #[test]
    fn the_markdown_report_shows_the_per_stage_p95s_and_the_errors() {
        let report = BenchReport {
            label: "t".into(),
            timestamp: "now".into(),
            base_url: "http://x".into(),
            n_queries: 1,
            max_chunks: 50,
            warmup_requests: 0,
            harness_commit: "abc".into(),
            levels: vec![LevelResult {
                concurrency: 1,
                requests: 10,
                errors: 1,
                duration_s: 1.0,
                throughput_rps: 10.0,
                latency_ms: LatencyReport {
                    mean: 5.0,
                    p50: 5.0,
                    p95: 6.0,
                    p99: 7.0,
                    n: 10,
                },
                latency_max_ms: 8.0,
                latency_min_ms: 4.0,
                stage_p95_ms: BTreeMap::from([("arms".to_string(), 4.0)]),
                stage_p50_ms: BTreeMap::from([("arms".to_string(), 3.0)]),
                statuses: BTreeMap::from([("500".to_string(), 1)]),
                error_samples: vec!["HTTP 500: boom".into()],
                stage_reports: 9,
                degraded: 2,
                degrades: BTreeMap::from([("vector_arm_failed".to_string(), 2)]),
            }],
        };
        let md = render_markdown(&report);
        assert!(md.contains("| 1 | 10 | 1 |"), "the row:\n{md}");
        assert!(
            md.contains("| arms | 3.0 | 4.0 |"),
            "the stage table:\n{md}"
        );
        assert!(md.contains("HTTP 500: boom"));
        assert!(md.contains("9 of 10 responses reported timings"));
        assert!(md.contains("vector_arm_failed × 2"), "degrades:\n{md}");
    }
}
