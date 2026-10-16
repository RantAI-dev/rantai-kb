//! Black-box HTTP runner.
//!
//! The runner only knows the public API of a running `kb-server`: upload a file,
//! poll its job, then search. That is deliberate — the same harness measures any
//! storage backend behind the same API, and it never links a store crate.
//!
//! A search that returns an HTTP error is a **failed query**, counted
//! separately and never folded into "no results".

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::dataset::{Dataset, DatasetError, Qrel, gold_for_documents, gold_for_pages, page_key};
use crate::metrics::{
    Gain, LatencyReport, QueryMetrics, evaluate_query, has_positive_gold, latency_report,
    unique_first_occurrence,
};
use crate::report::{
    Failure, FailureStage, ReportParts, RunReport, TrecRanking, build_report, write_outputs,
};

/// Everything a run needs.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub base_url: String,
    pub api_key: String,
    pub dataset_dir: PathBuf,
    pub k: usize,
    pub max_chunks: u32,
    pub out_dir: PathBuf,
    pub skip_ingest: bool,
    pub label: String,
    /// Which nDCG gain the whole run is scored under. Recorded in every output.
    pub gain: Gain,
    /// How long to wait for one ingest job before giving up on it.
    pub job_timeout: Duration,
    pub poll_interval: Duration,
}

impl RunnerConfig {
    /// Sensible defaults besides the three required inputs.
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        dataset_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            dataset_dir: dataset_dir.into(),
            k: 10,
            max_chunks: 50,
            out_dir: PathBuf::from("eval-out"),
            skip_ingest: false,
            label: "run".to_string(),
            gain: Gain::default(),
            job_timeout: Duration::from_secs(900),
            poll_interval: Duration::from_millis(500),
        }
    }
}

/// Runner failures.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("dataset: {0}")]
    Dataset(#[from] DatasetError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// A request the server answered with an error status. Kept apart from the
    /// transport failures above because it carries the evidence — status, error
    /// code, body — that turns "a query failed" into "this query failed because
    /// the key is wrong".
    #[error("{}", failure_summary(.0))]
    Failure(Failure),
    #[error("{0}")]
    Other(String),
}

impl RunError {
    /// Turn any runner error into the record a reader can act on.
    ///
    /// A transport error, a timeout or an unreadable body becomes status `0`:
    /// the harness is honest that no HTTP response arrived rather than inventing
    /// a status for one. `id` always wins over whatever the error carried, so a
    /// failure is named by the query id rather than by its text.
    fn into_failure(self, stage: FailureStage, id: &str) -> Failure {
        match self {
            Self::Failure(mut failure) => {
                failure.id = id.to_string();
                failure
            }
            other => Failure::local(stage, id, &other.to_string()),
        }
    }
}

fn failure_summary(failure: &Failure) -> String {
    if failure.body.is_empty() {
        failure.summary()
    } else {
        format!("{}: {}", failure.summary(), failure.body)
    }
}

/// What one `POST /v1/search` answered with.
#[derive(Debug, Default, Deserialize)]
struct SearchAnswer {
    /// `Option` because the route omits `chunks` on some paths, and an
    /// explicit `null` must read as "no results", not as a broken client.
    #[serde(default)]
    chunks: Option<Vec<Chunk>>,
    /// Arm-level degradations the server reported, e.g. a vector arm that
    /// failed and let BM25 answer alone.
    #[serde(default)]
    degrades: Vec<String>,
    /// The server's "I did not search what you asked for" message.
    #[serde(default)]
    note: Option<String>,
}

/// One answered query, as the scoreboard needs it.
struct Answered<'a> {
    id: &'a str,
    qrels: &'a [Qrel],
    doc_keys: &'a [String],
    page_keys: &'a [String],
    latency_ms: f64,
    degrades: &'a [String],
    note: Option<&'a str>,
}

/// The per-query accounting of a run, kept separate from the HTTP loop so the
/// scoring rules can be tested without a server.
///
/// Two populations are kept side by side at each level, and the difference
/// between them is the point:
///
/// * `document` / `page` — the **strict** headline. Every evaluable query is
///   here, including the ones that failed, which enter as
///   [`QueryMetrics::ZERO`].
/// * `document_answered` / `page_answered` — the **secondary** diagnostic over
///   answered queries only, which is what tells "worse" apart from "down".
///
/// A failed query never enters the run files: a missing query in a TREC run means
/// "not run", which is not the same measurement as "ran and found nothing".
#[derive(Debug, Default)]
struct Scoreboard {
    gain: Gain,
    document: Vec<QueryMetrics>,
    page: Vec<QueryMetrics>,
    document_answered: Vec<QueryMetrics>,
    page_answered: Vec<QueryMetrics>,
    document_run: Vec<TrecRanking>,
    page_run: Vec<TrecRanking>,
    latencies: Vec<f64>,
    n_queries: usize,
    n_failed: usize,
    n_degraded: usize,
    n_noted: usize,
    degrades: BTreeSet<String>,
    /// Every failure, with the server's own words for it.
    failures: Vec<Failure>,
}

impl Scoreboard {
    /// Record a query the server answered, degrading or not.
    fn record_answer(&mut self, k: usize, answer: Answered<'_>) {
        self.n_queries += 1;
        self.latencies.push(answer.latency_ms);
        if !answer.degrades.is_empty() {
            self.n_degraded += 1;
            self.degrades.extend(answer.degrades.iter().cloned());
        }
        if answer.note.is_some() {
            self.n_noted += 1;
        }
        // A query with no positive judgement at a level is not evaluable
        // there and is left out of that level's mean, as BEIR does.
        if let Some(m) = evaluate_query(
            answer.doc_keys,
            &gold_for_documents(answer.qrels),
            k,
            self.gain,
        ) {
            self.document.push(m);
            self.document_answered.push(m);
        }
        if let Some(m) = evaluate_query(
            answer.page_keys,
            &gold_for_pages(answer.qrels),
            k,
            self.gain,
        ) {
            self.page.push(m);
            self.page_answered.push(m);
        }
        // The run files carry every answered query, including one that found
        // nothing: an empty ranking is a measurement, a missing query is not.
        self.document_run.push(TrecRanking {
            query_id: answer.id.to_string(),
            docnos: answer.doc_keys.to_vec(),
        });
        self.page_run.push(TrecRanking {
            query_id: answer.id.to_string(),
            docnos: answer.page_keys.to_vec(),
        });
    }

    /// Record a query the server could not answer.
    ///
    /// Counted, and scored as a zero at every level where it was evaluable. The
    /// earlier rule — exclude it — was a quiet way to reward a backend for
    /// timing out on exactly the queries that would have exposed it: drop the
    /// hard ones and the mean over the rest goes up.
    ///
    /// It stays out of the run files and out of the answered-only means, and its
    /// cause is kept: a failure whose reason is not recorded cannot be fixed.
    fn record_failure(&mut self, qrels: &[Qrel], cause: Failure) {
        self.n_queries += 1;
        self.n_failed += 1;
        self.failures.push(cause);
        if has_positive_gold(&gold_for_documents(qrels)) {
            self.document.push(QueryMetrics::ZERO);
        }
        if has_positive_gold(&gold_for_pages(qrels)) {
            self.page.push(QueryMetrics::ZERO);
        }
    }
}

/// Run the evaluation and write its outputs. Returns the same report it wrote.
///
/// # Errors
///
/// [`RunError`] when the dataset cannot be loaded, the ingest map is missing on
/// a `--skip-ingest` run, or the output cannot be written. Per-file ingest
/// failures and per-query search failures do **not** abort the run; they are
/// reported in the metrics.
pub async fn run(config: RunnerConfig) -> Result<RunReport, RunError> {
    let dataset = Dataset::load(&config.dataset_dir)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let (ingest_map, n_ingest_failed, ingest_failures) = if config.skip_ingest {
        (load_ingest_map(&config.out_dir)?, 0, Vec::new())
    } else {
        ingest_all(&client, &config, &dataset).await?
    };
    let reverse: HashMap<String, String> = ingest_map
        .iter()
        .map(|(k, v)| (v.clone(), k.clone()))
        .collect();

    let mut board = Scoreboard {
        gain: config.gain,
        failures: ingest_failures,
        ..Scoreboard::default()
    };
    for query in &dataset.queries {
        let started = Instant::now();
        let search = search_one(&client, &config, &query.text).await;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let answer = match search {
            Ok(answer) => answer,
            Err(err) => {
                let mut cause = err.into_failure(FailureStage::Query, &query.id);
                cause.redact(&config.api_key);
                eprintln!("{}: {}", cause.summary(), cause.body);
                board.record_failure(&dataset.qrels_for(&query.id), cause);
                continue;
            }
        };
        if let Some(note) = &answer.note {
            eprintln!("query {} answered with a note: {note}", query.id);
        }
        let (doc_keys, page_keys) = map_chunks(answer.chunks.as_deref().unwrap_or(&[]), &reverse);
        board.record_answer(
            config.k,
            Answered {
                id: &query.id,
                qrels: &dataset.qrels_for(&query.id),
                doc_keys: &doc_keys,
                page_keys: &page_keys,
                latency_ms: elapsed_ms,
                degrades: &answer.degrades,
                note: answer.note.as_deref(),
            },
        );
    }

    let server_version = fetch_server_version(&client, &config).await;
    let latency: LatencyReport = latency_report(std::mem::take(&mut board.latencies));
    let report = build_report(ReportParts {
        label: &config.label,
        dataset: &dataset.name,
        gain: config.gain,
        server_version: server_version.as_deref(),
        k: config.k,
        max_chunks: config.max_chunks,
        n_queries: board.n_queries,
        n_failed: board.n_failed,
        n_degraded: board.n_degraded,
        n_noted: board.n_noted,
        n_documents: ingest_map.len(),
        n_ingest_failed,
        degrades: board.degrades.into_iter().collect(),
        failures: board.failures,
        document: board.document,
        page: board.page,
        document_answered: board.document_answered,
        page_answered: board.page_answered,
        latency,
        document_run: board.document_run,
        page_run: board.page_run,
    });
    write_outputs(&config.out_dir, &report)?;
    Ok(report)
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    #[serde(default)]
    id: String,
    #[serde(rename = "jobId", default)]
    job_id: String,
}

/// `GET /health`. Unauthenticated by design, so the harness can identify the
/// server it is about to measure without holding a key.
#[derive(Debug, Deserialize)]
struct HealthAnswer {
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JobStatus {
    #[serde(default)]
    status: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Chunk {
    #[serde(rename = "documentId", default)]
    document_id: String,
    /// 0-based, as the API returns it. qrels pages are 1-based; the conversion
    /// happens once, in [`map_chunks`].
    #[serde(default)]
    page: Option<u32>,
}

fn bearer(key: &str) -> String {
    format!("Bearer {key}")
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// Upload every corpus file, polling each job to completion.
///
/// Reuses an existing `ingest-map.json` so a partial re-run does not re-upload
/// files that already landed. Failures are counted, logged and skipped: the
/// queries that touch them score as misses, which is visible in the report
/// rather than hidden by aborting the run. Each one keeps its cause, so
/// "3 corpus files failed" comes with the status and body that explain them.
async fn ingest_all(
    client: &reqwest::Client,
    config: &RunnerConfig,
    dataset: &Dataset,
) -> Result<(HashMap<String, String>, usize, Vec<Failure>), RunError> {
    let map_path = config.out_dir.join("ingest-map.json");
    let mut map: HashMap<String, String> = if map_path.is_file() {
        load_ingest_map(&config.out_dir).unwrap_or_default()
    } else {
        HashMap::new()
    };
    let mut failures: Vec<Failure> = Vec::new();
    for file in &dataset.corpus {
        if map.contains_key(&file.key) {
            continue;
        }
        match ingest_one(client, config, file).await {
            Ok(IngestOutcome::Landed(document_id)) => {
                map.insert(file.key.clone(), document_id);
                save_ingest_map(&map_path, &map)?;
            }
            Ok(IngestOutcome::Failed(mut cause)) => {
                cause.redact(&config.api_key);
                eprintln!("ingest: {}", cause.summary());
                eprintln!("  {}", cause.body);
                failures.push(cause);
            }
            Err(err) => {
                let cause = err.into_failure(FailureStage::Ingest, &file.key);
                eprintln!("ingest error for {}: {cause}", file.key);
                failures.push(cause);
            }
        }
    }
    let n = failures.len();
    if n > 0 {
        eprintln!("{n} corpus file(s) failed to ingest; continuing with the rest");
    }
    Ok((map, n, failures))
}

/// What one upload did.
enum IngestOutcome {
    Landed(String),
    Failed(Failure),
}

/// What an ingest job did.
enum JobOutcome {
    Success,
    Failed(Failure),
}

async fn ingest_one(
    client: &reqwest::Client,
    config: &RunnerConfig,
    file: &crate::dataset::CorpusFile,
) -> Result<IngestOutcome, RunError> {
    let bytes = std::fs::read(&file.path)?;
    let file_name = file
        .path
        .file_name()
        .map_or_else(|| file.key.clone(), |n| n.to_string_lossy().into_owned());
    let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
    let form = reqwest::multipart::Form::new().part("file", part);
    let response = client
        .post(endpoint(&config.base_url, "/v1/documents"))
        .header(reqwest::header::AUTHORIZATION, bearer(&config.api_key))
        .multipart(form)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Ok(IngestOutcome::Failed(Failure::from_response(
            FailureStage::Ingest,
            &file.key,
            status,
            &body,
        )));
    }
    let body: UploadResponse = response.json().await?;
    if body.job_id.is_empty() {
        return Ok(IngestOutcome::Failed(Failure::local(
            FailureStage::Ingest,
            &file.key,
            "the upload returned neither a job id nor an error",
        )));
    }
    match poll_job(client, config, &body.job_id).await? {
        JobOutcome::Success => Ok(IngestOutcome::Landed(body.id)),
        JobOutcome::Failed(cause) => Ok(IngestOutcome::Failed(cause)),
    }
}

/// Poll `GET /v1/jobs/:id` until the job is terminal or the timeout elapses.
///
/// A `failed` job and a job that never finished are both failures, and both keep
/// the server's own error text: "the worker died" and "the job took longer than
/// 900s" need different fixes.
async fn poll_job(
    client: &reqwest::Client,
    config: &RunnerConfig,
    job_id: &str,
) -> Result<JobOutcome, RunError> {
    let deadline = Instant::now() + config.job_timeout;
    loop {
        let response = client
            .get(endpoint(&config.base_url, &format!("/v1/jobs/{job_id}")))
            .header(reqwest::header::AUTHORIZATION, bearer(&config.api_key))
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(RunError::Failure(Failure::from_response(
                FailureStage::Ingest,
                job_id,
                status,
                &text,
            )));
        }
        let job: JobStatus = response.json().await?;
        match job.status.as_str() {
            "success" => return Ok(JobOutcome::Success),
            "failed" => {
                return Ok(JobOutcome::Failed(Failure::local(
                    FailureStage::Ingest,
                    job_id,
                    job.error.as_deref().unwrap_or("the job failed with no error message"),
                )));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Ok(JobOutcome::Failed(Failure::local(
                FailureStage::Ingest,
                job_id,
                &format!(
                    "the job did not finish within {:?}",
                    config.job_timeout
                ),
            )));
        }
        tokio::time::sleep(config.poll_interval).await;
    }
}

/// Ask the server what version it is, best effort.
///
/// Non-fatal: a run against a server that cannot answer `/health` still
/// produces metrics, and the report says the build is unidentified. What must
/// never happen is inventing a value here.
async fn fetch_server_version(client: &reqwest::Client, config: &RunnerConfig) -> Option<String> {
    let response = client
        .get(endpoint(&config.base_url, "/health"))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        eprintln!(
            "GET /health returned HTTP {}; the server version is unknown",
            response.status()
        );
        return None;
    }
    let health: HealthAnswer = response.json().await.ok()?;
    health.version.filter(|v| !v.trim().is_empty())
}

/// POST one search and return its answer.
///
/// An HTTP error keeps the body. "HTTP 401" alone leaves the reader guessing
/// between a bad key, a revoked key and a missing scope; the body says which,
/// and the report groups on it.
async fn search_one(
    client: &reqwest::Client,
    config: &RunnerConfig,
    text: &str,
) -> Result<SearchAnswer, RunError> {
    let response = client
        .post(endpoint(&config.base_url, "/v1/search"))
        .header(reqwest::header::AUTHORIZATION, bearer(&config.api_key))
        .json(&json!({ "query": text, "maxChunks": config.max_chunks }))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        // `into_failure` replaces this placeholder with the query id, which only
        // the caller knows; nothing is lost by carrying the text meanwhile.
        let failure = Failure::from_response(FailureStage::Query, text, status, &body);
        return Err(RunError::Failure(failure));
    }
    // A body that does not parse is a failure of this query, not of the run.
    let body: SearchAnswer = response
        .json()
        .await
        .map_err(|e| RunError::Other(format!("unreadable search response: {e}")))?;
    Ok(body)
}

/// Map chunks to doc-level and page-level keys, first occurrence winning.
fn map_chunks(chunks: &[Chunk], reverse: &HashMap<String, String>) -> (Vec<String>, Vec<String>) {
    let mut document_ids = Vec::with_capacity(chunks.len());
    let mut page_ids = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let Some(doc_key) = reverse.get(&chunk.document_id) else {
            continue;
        };
        document_ids.push(doc_key.clone());
        if let Some(page0) = chunk.page {
            page_ids.push(page_key(doc_key, page0.saturating_add(1)));
        }
    }
    (
        unique_first_occurrence(document_ids),
        unique_first_occurrence(page_ids),
    )
}

/// Read `<out>/ingest-map.json`.
fn load_ingest_map(out_dir: &Path) -> Result<HashMap<String, String>, RunError> {
    let path = out_dir.join("ingest-map.json");
    if !path.is_file() {
        return Err(RunError::Other(format!(
            "--skip-ingest was set but {} does not exist; run an ingest first",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(&path)?;
    let map: HashMap<String, String> = serde_json::from_str(&text).map_err(|e| {
        RunError::Other(format!("{} is not a doc-key → id map: {e}", path.display()))
    })?;
    Ok(map)
}

fn save_ingest_map(path: &Path, map: &HashMap<String, String>) -> Result<(), RunError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(map).map_err(|e| RunError::Other(e.to_string()))?;
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::{base_doc_key, page_of};
    use crate::metrics::aggregate_level;

    #[test]
    fn a_health_answer_without_a_version_is_none() {
        let ok: HealthAnswer = serde_json::from_str(r#"{"ok":true,"version":"1.4.2"}"#).unwrap();
        assert_eq!(ok.version.as_deref(), Some("1.4.2"));
        let blank: HealthAnswer = serde_json::from_str(r#"{"ok":true,"version":"  "}"#).unwrap();
        assert_eq!(blank.version.as_deref(), Some("  "));
        let missing: HealthAnswer = serde_json::from_str(r#"{"ok":true}"#).unwrap();
        assert!(missing.version.is_none());
    }

    #[test]
    fn endpoint_handles_a_trailing_slash() {
        assert_eq!(
            endpoint("http://x:1/", "/v1/search"),
            "http://x:1/v1/search"
        );
        assert_eq!(endpoint("http://x:1", "/v1/search"), "http://x:1/v1/search");
    }

    #[test]
    fn chunks_map_to_unique_doc_and_page_keys() {
        let reverse: HashMap<String, String> = [
            ("doc-1".to_string(), "alpha".to_string()),
            ("doc-2".to_string(), "beta".to_string()),
        ]
        .into_iter()
        .collect();
        let chunks = vec![
            Chunk {
                document_id: "doc-1".into(),
                page: Some(0),
            },
            Chunk {
                document_id: "doc-1".into(),
                page: Some(1),
            },
            Chunk {
                document_id: "unknown".into(),
                page: Some(0),
            },
            Chunk {
                document_id: "doc-2".into(),
                page: None,
            },
            Chunk {
                document_id: "doc-1".into(),
                page: Some(0),
            },
        ];
        let (docs, pages) = map_chunks(&chunks, &reverse);
        assert_eq!(docs, vec!["alpha", "beta"]);
        // page 0 becomes 1-based page 1; pages are unique in order.
        assert_eq!(pages, vec!["alpha#p1", "alpha#p2"]);
    }

    /// The API counts pages from 0, qrels count them from 1. This is the test
    /// that pins the offset: `page: 0` is the first page, and its qrel key is
    /// `#p1`, not `#p0`.
    #[test]
    fn a_zero_based_api_page_becomes_a_one_based_qrels_key() {
        let reverse = HashMap::from([("d".to_string(), "doc".to_string())]);
        let (docs, pages) = map_chunks(
            &[
                Chunk {
                    document_id: "d".into(),
                    page: Some(0),
                },
                Chunk {
                    document_id: "d".into(),
                    page: Some(2),
                },
            ],
            &reverse,
        );
        assert_eq!(docs, vec!["doc"]);
        // 0 → 1 and 2 → 3: exactly one added, everywhere.
        assert_eq!(pages, vec!["doc#p1", "doc#p3"]);
        // And those are keys a qrels file can actually name.
        assert_eq!(page_of("doc#p1"), Some(1));
        assert_eq!(base_doc_key("doc#p1"), "doc");
        // A qrel of `#p0` is impossible: `page_of` refuses it, so it can never
        // be produced by accident and match a mapped page.
        assert_eq!(page_of("doc#p0"), None);
    }

    fn qrel(query: &str, corpus: &str, score: i32) -> Qrel {
        Qrel {
            query_id: query.to_string(),
            corpus_id: corpus.to_string(),
            score,
        }
    }

    /// A failure that carries its cause, as the HTTP path would record it.
    fn refused() -> Failure {
        Failure::from_response(
            FailureStage::Query,
            "q",
            403,
            r#"{"error":"API key lacks scope kb:read"}"#,
        )
    }

    #[test]
    fn a_failure_keeps_the_reason_it_had() {
        let mut board = Scoreboard::default();
        board.record_failure(&[qrel("q1", "alpha", 2)], refused());
        assert_eq!(board.failures.len(), 1);
        assert_eq!(board.failures[0].status, 403);
        assert_eq!(board.failures[0].stage, FailureStage::Query);
        // kb-server sends `{"error": …}` and no code, so none is invented.
        assert_eq!(board.failures[0].code, None);
        assert!(board.failures[0].body.contains("kb:read"));
    }

    #[test]
    fn a_transport_error_is_recorded_without_an_http_status() {
        let err = RunError::Other("connection refused".to_string());
        let cause = err.into_failure(FailureStage::Query, "q7");
        assert_eq!(cause.id, "q7");
        assert_eq!(cause.status, 0);
        assert_eq!(cause.code, None);
        assert_eq!(cause.body, "connection refused");
        assert!(cause.summary().contains("no HTTP response"), "{cause:?}");
    }

    #[test]
    fn a_server_failure_is_recorded_under_the_query_id_not_its_text() {
        let cause = Failure::from_response(
            FailureStage::Query,
            "the query text",
            401,
            r#"{"code":"unauthorized"}"#,
        );
        let err = RunError::Failure(cause);
        let recorded = err.into_failure(FailureStage::Query, "q3");
        assert_eq!(recorded.id, "q3");
        assert_eq!(recorded.status, 401);
        assert_eq!(recorded.code.as_deref(), Some("unauthorized"));
        assert!(recorded.summary().contains("q3"), "{recorded:?}");
    }

    #[test]
    fn the_api_key_is_never_written_into_a_failure() {
        let mut cause = Failure::from_response(
            FailureStage::Query,
            "q1",
            401,
            "rejected key rkb_secret-Abc_123 for tenant t1",
        );
        cause.redact("rkb_secret-Abc_123");
        assert!(!cause.body.contains("rkb_secret"), "{}", cause.body);
        assert!(cause.body.contains("[redacted]"), "{}", cause.body);
    }

    #[test]
    fn a_failed_query_is_counted_and_scored_as_a_zero() {
        let mut board = Scoreboard::default();
        // q1 is answered and its gold is found at rank 1.
        board.record_answer(
            10,
            Answered {
                id: "q1",
                qrels: &[qrel("q1", "alpha", 2)],
                doc_keys: &["alpha".to_string()],
                page_keys: &[],
                latency_ms: 12.0,
                degrades: &[],
                note: None,
            },
        );
        // q2 could not be answered. It has gold at the document level, so it is
        // a zero there rather than an absence.
        board.record_failure(&[qrel("q2", "alpha", 2)], refused());

        assert_eq!(board.n_queries, 2);
        assert_eq!(board.n_failed, 1);
        assert_eq!(
            board.document.len(),
            2,
            "the failure is scored, not skipped"
        );
        assert!((board.document[0].ndcg - 1.0).abs() < 1e-9);
        assert!((board.document[1].ndcg - 0.0).abs() < 1e-9);
        // No page judgement for either query, so the page level stays empty.
        assert!(board.page.is_empty());
        // The answered-only population has only q1.
        assert_eq!(board.document_answered.len(), 1);
        assert!((board.document_answered[0].ndcg - 1.0).abs() < 1e-9);
        // The run files carry q1 only: a missing query is not an empty ranking.
        assert_eq!(board.document_run.len(), 1);
        assert_eq!(board.document_run[0].query_id, "q1");
        // Latency counts answered queries only.
        assert_eq!(board.latencies, vec![12.0]);
    }

    #[test]
    fn an_answered_query_with_no_hits_scores_zero_and_is_still_in_the_run() {
        let mut board = Scoreboard::default();
        board.record_answer(
            10,
            Answered {
                id: "q1",
                qrels: &[qrel("q1", "alpha", 2)],
                doc_keys: &[],
                page_keys: &[],
                latency_ms: 5.0,
                degrades: &[],
                note: None,
            },
        );
        assert_eq!(board.n_queries, 1);
        assert_eq!(board.n_failed, 0);
        assert_eq!(board.document.len(), 1);
        assert!((board.document[0].ndcg - 0.0).abs() < 1e-9);
        assert_eq!(board.document_run.len(), 1);
        assert!(board.document_run[0].docnos.is_empty());
    }

    #[test]
    fn a_failed_query_lowers_the_strict_mean_and_not_the_answered_one() {
        // Three evaluable document queries; the server answered two, both
        // perfectly, and failed the third.
        let mut board = Scoreboard::default();
        for id in ["q1", "q2"] {
            board.record_answer(
                10,
                Answered {
                    id,
                    qrels: &[qrel(id, "alpha", 2)],
                    doc_keys: &["alpha".to_string()],
                    page_keys: &[],
                    latency_ms: 4.0,
                    degrades: &[],
                    note: None,
                },
            );
        }
        board.record_failure(&[qrel("q3", "alpha", 2)], refused());

        let strict = aggregate_level(&board.document, &board.document_answered);
        assert_eq!(strict.n_queries, 3);
        assert!(
            (strict.ndcg.mean - 2.0 / 3.0).abs() < 1e-9,
            "strict {:?}",
            strict.ndcg
        );
        assert_eq!(strict.answered.n_queries, 2);
        assert!((strict.answered.ndcg.mean - 1.0).abs() < 1e-9);
        // A backend that fails the hard queries scores lower here, never higher.
        assert!(strict.ndcg.mean < strict.answered.ndcg.mean);
    }

    #[test]
    fn a_failed_query_with_no_page_gold_is_not_a_page_zero() {
        let mut board = Scoreboard::default();
        board.record_failure(&[qrel("q1", "alpha", 2)], refused());
        assert_eq!(board.page.len(), 0);
        assert_eq!(board.page_answered.len(), 0);
        assert_eq!(board.document.len(), 1);

        let mut page_only = Scoreboard::default();
        page_only.record_failure(&[qrel("q1", "alpha#p3", 3)], refused());
        // Page gold is judged, document gold collapses onto alpha, so both
        // levels are evaluable and both take the zero.
        assert_eq!(page_only.page.len(), 1);
        assert_eq!(page_only.document.len(), 1);
        assert_eq!(page_only.page_answered.len(), 0);
    }

    #[test]
    fn degradations_and_notes_are_surfaced_not_swallowed() {
        let mut board = Scoreboard::default();
        board.record_answer(
            10,
            Answered {
                id: "q1",
                qrels: &[qrel("q1", "alpha", 1)],
                doc_keys: &["alpha".to_string()],
                page_keys: &[],
                latency_ms: 5.0,
                degrades: &["vector arm unavailable".to_string()],
                note: Some("API key is not bound to any of the requested knowledge bases"),
            },
        );
        assert_eq!(board.n_degraded, 1);
        assert_eq!(board.n_noted, 1);
        assert_eq!(board.degrades.len(), 1);
        assert!(board.degrades.contains("vector arm unavailable"));
        // A degraded answer is still scored: the numbers stay comparable, and
        // the report says why they may not be.
        assert_eq!(board.document.len(), 1);
    }

    #[test]
    fn a_query_without_judgements_is_scored_at_neither_level() {
        let mut board = Scoreboard::default();
        board.record_answer(
            10,
            Answered {
                id: "q1",
                qrels: &[qrel("q1", "alpha#p2", 3)],
                doc_keys: &["alpha".to_string()],
                page_keys: &["alpha#p1".to_string()],
                latency_ms: 5.0,
                degrades: &[],
                note: None,
            },
        );
        // Document gold: alpha#p2 collapses onto alpha, which was found.
        assert_eq!(board.document.len(), 1);
        assert!((board.document[0].ndcg - 1.0).abs() < 1e-9);
        // Page gold: only alpha#p2 is judged and page 1 was retrieved → miss.
        assert_eq!(board.page.len(), 1);
        assert!((board.page[0].ndcg - 0.0).abs() < 1e-9);

        let mut empty = Scoreboard::default();
        empty.record_answer(
            10,
            Answered {
                id: "q2",
                qrels: &[qrel("q2", "alpha", 0)],
                doc_keys: &["beta".to_string()],
                page_keys: &[],
                latency_ms: 5.0,
                degrades: &[],
                note: None,
            },
        );
        // A judgement of 0 is not relevant, so the query is not evaluable.
        assert!(empty.document.is_empty());
        assert!(empty.page.is_empty());
        assert_eq!(empty.n_queries, 1);
    }

    #[test]
    fn an_answer_without_chunks_parses_as_empty_not_as_an_error() {
        let answer: SearchAnswer =
            serde_json::from_str(r#"{"chunks":null,"degrades":["bm25 only"]}"#).unwrap();
        assert!(answer.chunks.is_none());
        assert_eq!(answer.degrades, vec!["bm25 only"]);
        let missing: SearchAnswer = serde_json::from_str(r#"{"sources":[]}"#).unwrap();
        assert!(missing.chunks.is_none());
        assert!(missing.note.is_none());
    }
}
