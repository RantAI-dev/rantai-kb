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
    Gain, LatencyReport, QueryMetrics, evaluate_query, latency_report, unique_first_occurrence,
};
use crate::report::{ReportParts, RunReport, TrecRanking, build_report, write_outputs};

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
    #[error("{0}")]
    Other(String),
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
/// The rule that matters most: [`Self::record_failure`] counts a query and
/// stops. A query the server could not answer is never scored as a ranking of
/// nothing, because that would turn an outage into a quality regression.
#[derive(Debug, Default)]
struct Scoreboard {
    gain: Gain,
    document: Vec<QueryMetrics>,
    page: Vec<QueryMetrics>,
    document_run: Vec<TrecRanking>,
    page_run: Vec<TrecRanking>,
    latencies: Vec<f64>,
    n_queries: usize,
    n_failed: usize,
    n_degraded: usize,
    n_noted: usize,
    degrades: BTreeSet<String>,
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
        }
        if let Some(m) = evaluate_query(
            answer.page_keys,
            &gold_for_pages(answer.qrels),
            k,
            self.gain,
        ) {
            self.page.push(m);
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

    /// Record a query that could not be answered: counted, never scored.
    fn record_failure(&mut self) {
        self.n_queries += 1;
        self.n_failed += 1;
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

    let (ingest_map, n_ingest_failed) = if config.skip_ingest {
        (load_ingest_map(&config.out_dir)?, 0)
    } else {
        ingest_all(&client, &config, &dataset).await?
    };
    let reverse: HashMap<String, String> = ingest_map
        .iter()
        .map(|(k, v)| (v.clone(), k.clone()))
        .collect();

    let mut board = Scoreboard {
        gain: config.gain,
        ..Scoreboard::default()
    };
    for query in &dataset.queries {
        let started = Instant::now();
        let search = search_one(&client, &config, &query.text).await;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let answer = match search {
            Ok(answer) => answer,
            Err(err) => {
                board.record_failure();
                eprintln!("query {} failed: {err}", query.id);
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

    let latency: LatencyReport = latency_report(std::mem::take(&mut board.latencies));
    let report = build_report(ReportParts {
        label: &config.label,
        dataset: &dataset.name,
        gain: config.gain,
        k: config.k,
        max_chunks: config.max_chunks,
        n_queries: board.n_queries,
        n_failed: board.n_failed,
        n_degraded: board.n_degraded,
        n_noted: board.n_noted,
        n_documents: ingest_map.len(),
        n_ingest_failed,
        degrades: board.degrades.into_iter().collect(),
        document: board.document,
        page: board.page,
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
/// rather than hidden by aborting the run.
async fn ingest_all(
    client: &reqwest::Client,
    config: &RunnerConfig,
    dataset: &Dataset,
) -> Result<(HashMap<String, String>, usize), RunError> {
    let map_path = config.out_dir.join("ingest-map.json");
    let mut map: HashMap<String, String> = if map_path.is_file() {
        load_ingest_map(&config.out_dir).unwrap_or_default()
    } else {
        HashMap::new()
    };
    let mut failures = 0;
    for file in &dataset.corpus {
        if map.contains_key(&file.key) {
            continue;
        }
        match ingest_one(client, config, file).await {
            Ok(Some(document_id)) => {
                map.insert(file.key.clone(), document_id);
                save_ingest_map(&map_path, &map)?;
            }
            Ok(None) => {
                failures += 1;
                eprintln!("ingest failed: {}", file.key);
            }
            Err(err) => {
                failures += 1;
                eprintln!("ingest error for {}: {err}", file.key);
            }
        }
    }
    if failures > 0 {
        eprintln!("{failures} corpus file(s) failed to ingest; continuing with the rest");
    }
    Ok((map, failures))
}

async fn ingest_one(
    client: &reqwest::Client,
    config: &RunnerConfig,
    file: &crate::dataset::CorpusFile,
) -> Result<Option<String>, RunError> {
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
        eprintln!("upload of {} returned HTTP {}", file.key, response.status());
        return Ok(None);
    }
    let body: UploadResponse = response.json().await?;
    if body.job_id.is_empty() {
        return Ok(None);
    }
    let done = poll_job(client, config, &body.job_id).await?;
    Ok(done.then_some(body.id))
}

/// Poll `GET /v1/jobs/:id` until the job is terminal or the timeout elapses.
///
/// Returns `true` on `success`, `false` on `failed` or timeout.
async fn poll_job(
    client: &reqwest::Client,
    config: &RunnerConfig,
    job_id: &str,
) -> Result<bool, RunError> {
    let deadline = Instant::now() + config.job_timeout;
    loop {
        let response = client
            .get(endpoint(&config.base_url, &format!("/v1/jobs/{job_id}")))
            .header(reqwest::header::AUTHORIZATION, bearer(&config.api_key))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(RunError::Other(format!(
                "job {job_id} returned HTTP {}",
                response.status()
            )));
        }
        let job: JobStatus = response.json().await?;
        match job.status.as_str() {
            "success" => return Ok(true),
            "failed" => {
                eprintln!(
                    "job {job_id} failed: {}",
                    job.error.as_deref().unwrap_or("(no error)")
                );
                return Ok(false);
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            eprintln!("job {job_id} timed out after {:?}", config.job_timeout);
            return Ok(false);
        }
        tokio::time::sleep(config.poll_interval).await;
    }
}

/// POST one search and return its answer.
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
        return Err(RunError::Other(format!("HTTP {}", response.status())));
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

    #[test]
    fn a_failed_query_is_counted_and_never_scored() {
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
        // q2 could not be answered. It must not enter the means at all.
        board.record_failure();

        assert_eq!(board.n_queries, 2);
        assert_eq!(board.n_failed, 1);
        assert_eq!(board.document.len(), 1, "one scored query, not two");
        assert!((board.document[0].ndcg - 1.0).abs() < 1e-9);
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
