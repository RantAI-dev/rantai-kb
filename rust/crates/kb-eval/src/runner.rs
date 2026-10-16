//! Black-box HTTP runner.
//!
//! The runner only knows the public API of a running `kb-server`: upload a file,
//! poll its job, then search. That is deliberate — the same harness measures any
//! storage backend behind the same API, and it never links a store crate.
//!
//! A search that returns an HTTP error is a **failed query**, counted
//! separately and never folded into "no results".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::dataset::{Dataset, DatasetError, gold_for_documents, gold_for_pages, page_key};
use crate::metrics::{
    LatencyReport, QueryMetrics, evaluate_query, latency_report, unique_first_occurrence,
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

    let ingest_map = if config.skip_ingest {
        load_ingest_map(&config.out_dir)?
    } else {
        ingest_all(&client, &config, &dataset).await?
    };
    let reverse: HashMap<String, String> = ingest_map
        .iter()
        .map(|(k, v)| (v.clone(), k.clone()))
        .collect();

    let mut document_metrics: Vec<QueryMetrics> = Vec::new();
    let mut page_metrics: Vec<QueryMetrics> = Vec::new();
    let mut document_run: Vec<TrecRanking> = Vec::new();
    let mut page_run: Vec<TrecRanking> = Vec::new();
    let mut latencies: Vec<f64> = Vec::new();
    let mut n_failed: usize = 0;

    for query in &dataset.queries {
        let started = Instant::now();
        let search = search_one(&client, &config, &query.text).await;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let chunks = match search {
            Ok(chunks) => chunks,
            Err(err) => {
                n_failed += 1;
                eprintln!("query {} failed: {err}", query.id);
                continue;
            }
        };
        latencies.push(elapsed_ms);

        let (doc_keys, page_keys) = map_chunks(&chunks, &reverse);
        let qrels = dataset.qrels_for(&query.id);
        let doc_gold = gold_for_documents(&qrels);
        let page_gold = gold_for_pages(&qrels);
        if let Some(metrics) = evaluate_query(&doc_keys, &doc_gold, config.k) {
            document_metrics.push(metrics);
        }
        if let Some(metrics) = evaluate_query(&page_keys, &page_gold, config.k) {
            page_metrics.push(metrics);
        }
        document_run.push(TrecRanking {
            query_id: query.id.clone(),
            docnos: doc_keys,
        });
        page_run.push(TrecRanking {
            query_id: query.id.clone(),
            docnos: page_keys,
        });
    }

    let latency: LatencyReport = latency_report(latencies);
    let report = build_report(ReportParts {
        label: &config.label,
        dataset: &dataset.name,
        k: config.k,
        max_chunks: config.max_chunks,
        n_failed,
        document: document_metrics,
        page: page_metrics,
        latency,
        document_run,
        page_run,
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
struct SearchResponse {
    #[serde(default)]
    chunks: Vec<Chunk>,
}

#[derive(Debug, Deserialize)]
struct Chunk {
    #[serde(rename = "documentId", default)]
    document_id: String,
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
/// files that already landed. Failures are logged and skipped.
async fn ingest_all(
    client: &reqwest::Client,
    config: &RunnerConfig,
    dataset: &Dataset,
) -> Result<HashMap<String, String>, RunError> {
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
    Ok(map)
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

/// POST one search and return its chunks in rank order.
async fn search_one(
    client: &reqwest::Client,
    config: &RunnerConfig,
    text: &str,
) -> Result<Vec<Chunk>, RunError> {
    let response = client
        .post(endpoint(&config.base_url, "/v1/search"))
        .header(reqwest::header::AUTHORIZATION, bearer(&config.api_key))
        .json(&json!({ "query": text, "maxChunks": config.max_chunks }))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(RunError::Other(format!("HTTP {}", response.status())));
    }
    let body: SearchResponse = response.json().await?;
    Ok(body.chunks)
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
}
