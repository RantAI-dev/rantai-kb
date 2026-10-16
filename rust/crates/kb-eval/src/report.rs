//! Run outputs: `metrics.json`, `report.md`, `run.trec`, and the offline
//! scorer.
//!
//! The TREC files are the important interoperability surface: `trec_eval` and
//! `ranx` can re-score them, which is how this harness is checked against an
//! outside tool. Because the metrics rank documents by the first chunk that
//! mentions them, the TREC score column encodes rank (`1 / rank`) rather than
//! the cosine similarity, so a re-scoring tool reproduces the same order.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::dataset::{Qrel, gold_for_documents, gold_for_pages};
use crate::metrics::{
    Gain, Gold, LatencyReport, LevelReport, Means, QueryMetrics, aggregate_level, evaluate_query,
    unique_first_occurrence,
};

/// How much of a response body is kept per failure.
///
/// Enough to recognise the error, short enough that `metrics.json` stays
/// readable and cannot smuggle a whole HTML error page into an artifact.
pub const FAILURE_BODY_CHARS: usize = 200;

/// The top-level contents of `metrics.json`.
///
/// The counts are deliberately more than "how many documents did we find":
/// `n_queries` is what we asked, `n_failed` is what the server could not
/// answer, and `n_degraded` is what it answered while an arm was down. A run
/// that quietly scored a broken server as zero relevance would be a lie, so the
/// denominators are all published.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsFile {
    pub label: String,
    pub dataset: String,
    pub timestamp: String,
    /// The commit of the harness binary that produced these numbers, baked in
    /// at build time. It is not the commit of the server under test — see
    /// `server_version` for what the server says about itself.
    pub harness_commit: String,
    /// The nDCG gain in force. `linear` matches trec_eval/BEIR/MTEB/ViDoRe;
    /// `exponential` is Burges' variant and is not comparable with them.
    pub gain: String,
    /// What the server said about itself on `GET /health` (`KB_VERSION`).
    /// `None` when the endpoint is unreachable or says nothing.
    pub server_version: Option<String>,
    /// What the server said about the code it was built from.
    ///
    /// Always `None`: no `kb-server` route exposes a build commit — `/health`
    /// reports only `version`, which an operator sets by hand. Recorded as `null`
    /// rather than filled with the harness's own commit, which would be a lie
    /// about a different binary.
    pub server_commit: Option<String>,
    pub k: usize,
    pub max_chunks: u32,
    /// Queries attempted.
    pub n_queries: usize,
    /// Queries whose search failed; counted, never scored.
    pub n_failed: usize,
    /// Queries answered with a non-empty `degrades` list.
    pub n_degraded: usize,
    /// Queries answered with a server `note` (e.g. a knowledge-base filter the
    /// key may not use).
    pub n_noted: usize,
    /// Documents the harness knows about, from the ingest map.
    pub n_documents: usize,
    /// Corpus files whose ingest never reached `success`.
    pub n_ingest_failed: usize,
    /// Every distinct degradation label the server reported.
    pub degrades: Vec<String>,
    /// Every failed query and every corpus file that never ingested, each with
    /// the status, the error code and the head of the body that came back. This
    /// is what turns "12 of 12 queries failed" into "12 × HTTP 401
    /// `unauthorized`" — a number nobody has to guess at.
    pub failures: Vec<Failure>,
    pub document: LevelReport,
    pub page: LevelReport,
    pub latency_ms: LatencyReport,
}

/// Which step of a run failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureStage {
    /// `POST /v1/documents` (or the job it created) for one corpus file.
    Ingest,
    /// `POST /v1/search` for one query.
    Query,
}

impl MetricsFile {
    /// Load a `metrics.json` file from disk.
    ///
    /// # Errors
    ///
    /// An I/O error or a JSON deserialisation error.
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(io::Error::other)
    }
}

impl FailureStage {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Ingest => "ingest",
        }
    }
}

/// Why one query, or one corpus file, failed.
///
/// Recorded rather than summarised, because "12 of 12 queries failed" is a fact
/// an operator cannot act on: a bad API key, a 500 from the vector arm and a
/// malformed dataset all produce that sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub stage: FailureStage,
    /// The query id, or the corpus document key.
    pub id: String,
    /// The HTTP status. `0` when the request never got one — a transport error,
    /// a job that failed, a poll that timed out.
    pub status: u16,
    /// The `code` member of an RFC-9457 problem body, when the server sent one.
    /// `None` is a real answer, not a gap: `kb-server` answers `{"error": …}`,
    /// which carries no code and is not given a fabricated one.
    pub code: Option<String>,
    /// The first [`FAILURE_BODY_CHARS`] characters of what the server said.
    pub body: String,
}

impl Failure {
    /// A failure the server described in an HTTP response.
    #[must_use]
    pub fn from_response(
        stage: FailureStage,
        id: impl Into<String>,
        status: u16,
        body: &str,
    ) -> Self {
        Self {
            stage,
            id: id.into(),
            status,
            code: error_code(body),
            body: head(body),
        }
    }

    /// A failure with no HTTP response: a transport error, a failed ingest job,
    /// a poll that ran out of time. Status `0` says exactly that.
    #[must_use]
    pub fn local(stage: FailureStage, id: impl Into<String>, reason: &str) -> Self {
        Self {
            stage,
            id: id.into(),
            status: 0,
            code: None,
            body: head(reason),
        }
    }

    /// Scrub the API key out of the recorded body, in place.
    ///
    /// The harness never sends the key in a body it records, but an error page
    /// that echoes a header back would, and a metrics file is an artifact that
    /// gets uploaded. Cheap to guarantee, so it is guaranteed here rather than
    /// assumed at every call site.
    pub fn redact(&mut self, secret: &str) {
        self.body = redact_secret(&self.body, secret);
    }

    /// One line for a terminal or a log.
    #[must_use]
    pub fn summary(&self) -> String {
        let status = if self.status == 0 {
            "no HTTP response"
        } else {
            &format!("HTTP {}", self.status)
        };
        match &self.code {
            Some(code) => format!(
                "{} {} failed: {} — {}",
                self.stage.as_str(),
                self.id,
                status,
                code
            ),
            None => format!("{} {} failed: {}", self.stage.as_str(), self.id, status),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.summary())
    }
}

/// Replace every occurrence of `secret` with `[redacted]`.
#[must_use]
pub fn redact_secret(text: &str, secret: &str) -> String {
    if secret.trim().is_empty() {
        return text.to_string();
    }
    text.replace(secret, "[redacted]")
}

/// The first [`FAILURE_BODY_CHARS`] characters, with the tail marked.
///
/// Truncation happens on a character boundary, never mid-codepoint.
fn head(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= FAILURE_BODY_CHARS {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(FAILURE_BODY_CHARS).collect();
    format!("{cut}…")
}

/// The `code` member of a JSON error body, RFC-9457 style.
///
/// `None` when the body is not JSON, or is JSON without a `code`. A `code` is
/// never invented from a message: `{"error": "Internal error"}` has no code, and
/// reporting one would be a fabrication in an artifact people trust.
#[must_use]
pub fn error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    let code = value.get("code")?.as_str()?.trim();
    if code.is_empty() {
        return None;
    }
    Some(code.to_string())
}

/// Failures collapsed into one row per `(status, code)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureGroup {
    pub stage: FailureStage,
    pub status: u16,
    pub code: Option<String>,
    pub count: usize,
    /// The first id that produced this combination — enough to reproduce it.
    pub example_id: String,
}

/// Group failures by what went wrong, not by who was affected.
///
/// Twelve queries failing on HTTP 401 is one bug; twelve queries failing twelve
/// different ways is twelve. The grouping is what tells the two apart, so it is
/// ordered deterministically (status, then code) rather than by insertion.
#[must_use]
pub fn group_failures(failures: &[Failure]) -> Vec<FailureGroup> {
    let mut grouped: BTreeMap<(FailureStage, u16, String), FailureGroup> = BTreeMap::new();
    for failure in failures {
        let key = (
            failure.stage,
            failure.status,
            failure.code.clone().unwrap_or_default(),
        );
        grouped
            .entry(key)
            .or_insert_with(|| FailureGroup {
                stage: failure.stage,
                status: failure.status,
                code: failure.code.clone(),
                count: 0,
                example_id: failure.id.clone(),
            })
            .count += 1;
    }
    grouped.into_values().collect()
}

/// `status` and `code` rendered the way a reader thinks of them.
#[must_use]
pub fn describe_failure_key(status: u16, code: Option<&str>) -> String {
    let status = if status == 0 {
        "no response".to_string()
    } else {
        format!("HTTP {status}")
    };
    match code {
        Some(code) => format!("{status} `{code}`"),
        None => format!("{status} (no error code)"),
    }
}

/// The failure table `report.md` prints: one row per distinct cause.
#[must_use]
pub fn render_failure_table(failures: &[Failure]) -> String {
    let groups = group_failures(failures);
    if groups.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n## Why they failed\n\nGrouped by what the server said, not by which query it said it \
         to.\n\n| stage | what came back | count | first id |\n|---|---|---:|---|\n",
    );
    for group in groups {
        out.push_str(&format!(
            "| {} | {} | {} | `{}` |\n",
            group.stage.as_str(),
            describe_failure_key(group.status, group.code.as_deref()),
            group.count,
            group.example_id
        ));
    }
    out.push_str(
        "\n`no error code` means the body carried no RFC-9457 `code` member. Every failure, \
         one per line, is in `metrics.json` under `failures`.\n",
    );
    out
}

/// One query's ranked docnos for the TREC run files.
#[derive(Debug, Clone)]
pub struct TrecRanking {
    pub query_id: String,
    pub docnos: Vec<String>,
}

/// The full result of a run, before it is turned into files.
pub struct RunReport {
    pub metrics: MetricsFile,
    pub document_run: Vec<TrecRanking>,
    pub page_run: Vec<TrecRanking>,
}

/// The inputs to [`build_report`], grouped so the call site reads as a list of
/// named pieces rather than a wall of positional arguments.
pub struct ReportParts<'a> {
    pub label: &'a str,
    pub dataset: &'a str,
    pub gain: Gain,
    pub server_version: Option<&'a str>,
    pub k: usize,
    pub max_chunks: u32,
    pub n_queries: usize,
    pub n_failed: usize,
    pub n_degraded: usize,
    pub n_noted: usize,
    pub n_documents: usize,
    pub n_ingest_failed: usize,
    pub degrades: Vec<String>,
    /// Per-failure detail, already redacted of the API key by the caller.
    pub failures: Vec<Failure>,
    /// Strict: every evaluable query, failures included as zeros.
    pub document: Vec<QueryMetrics>,
    pub page: Vec<QueryMetrics>,
    /// Secondary: answered queries only.
    pub document_answered: Vec<QueryMetrics>,
    pub page_answered: Vec<QueryMetrics>,
    pub latency: LatencyReport,
    pub document_run: Vec<TrecRanking>,
    pub page_run: Vec<TrecRanking>,
}

/// Assemble a report from per-query metrics and rankings.
#[must_use]
pub fn build_report(parts: ReportParts<'_>) -> RunReport {
    let metrics = MetricsFile {
        label: parts.label.to_string(),
        dataset: parts.dataset.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        harness_commit: harness_commit(),
        gain: parts.gain.as_str().to_string(),
        server_version: parts.server_version.map(str::to_string),
        server_commit: None,
        k: parts.k,
        max_chunks: parts.max_chunks,
        n_queries: parts.n_queries,
        n_failed: parts.n_failed,
        n_degraded: parts.n_degraded,
        n_noted: parts.n_noted,
        n_documents: parts.n_documents,
        n_ingest_failed: parts.n_ingest_failed,
        degrades: parts.degrades,
        failures: parts.failures,
        document: aggregate_level(&parts.document, &parts.document_answered),
        page: aggregate_level(&parts.page, &parts.page_answered),
        latency_ms: parts.latency,
    };
    RunReport {
        metrics,
        document_run: parts.document_run,
        page_run: parts.page_run,
    }
}

/// The git commit this binary was **built** from, or `unknown`.
///
/// Baked in by `build.rs` (`git rev-parse` at compile time), never read from
/// the working directory at run time: a harness invoked from `/tmp` must still
/// report the code that produced the number, and a working-tree read would
/// silently attribute the harness's checkout to the server under test.
#[must_use]
pub fn harness_commit() -> String {
    option_env!("KB_EVAL_HARNESS_COMMIT")
        .unwrap_or("unknown")
        .to_string()
}

/// Write `metrics.json`, `report.md`, `run.trec` and `run.page.trec` into `dir`.
///
/// # Errors
///
/// An I/O error from creating the directory or writing a file.
pub fn write_outputs(dir: &Path, report: &RunReport) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let json = serde_json::to_string_pretty(&report.metrics).map_err(io::Error::other)?;
    fs::write(dir.join("metrics.json"), json)?;
    fs::write(dir.join("report.md"), render_markdown(&report.metrics))?;
    write_trec(
        &dir.join("run.trec"),
        &report.document_run,
        &report.metrics.label,
    )?;
    write_trec(
        &dir.join("run.page.trec"),
        &report.page_run,
        &report.metrics.label,
    )?;
    Ok(())
}

/// Render the human-readable report.
#[must_use]
pub fn render_markdown(m: &MetricsFile) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Retrieval evaluation — {}\n\n", m.label));
    out.push_str(&format!("- dataset: `{}`\n", m.dataset));
    out.push_str(&format!("- harness commit: `{}`\n", m.harness_commit));
    out.push_str(&format!("- nDCG gain: **{}**\n", m.gain));
    match m.server_version.as_deref() {
        Some(version) => out.push_str(&format!("- server version: `{version}` (GET /health)\n")),
        None => out.push_str(
            "- server version: unknown (GET /health did not answer; the run measured an \
             unidentified build)\n",
        ),
    }
    out.push_str(&format!(
        "- server commit: {}\n",
        m.server_commit
            .as_deref()
            .map_or_else(|| "null — no kb-server route exposes a build commit (`/health` reports only `version`)".to_string(), |c| format!("`{c}`"))
    ));
    out.push_str(&format!("- timestamp: {}\n", m.timestamp));
    out.push_str(&format!("- cutoff k: {}\n", m.k));
    out.push_str(&format!("- max chunks per query: {}\n", m.max_chunks));
    out.push_str(&format!("- documents known: {}\n", m.n_documents));
    out.push_str(&format!(
        "- queries: {} (failed: {}, degraded: {}, noted: {})\n",
        m.n_queries, m.n_failed, m.n_degraded, m.n_noted
    ));
    if m.n_ingest_failed > 0 {
        out.push_str(&format!(
            "- corpus files that failed to ingest: {}\n",
            m.n_ingest_failed
        ));
    }
    if !m.degrades.is_empty() {
        out.push_str(&format!(
            "- degradations reported by the server: {}\n",
            m.degrades.join(", ")
        ));
    }
    out.push('\n');
    out.push_str(
        "Strict means: every evaluable query is in them, and a query the server failed\n\
         to answer is scored as a zero at every level where it was evaluable.\n\n",
    );
    out.push_str("| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    out.push_str(&level_row("document", &m.document));
    out.push_str(&level_row("page", &m.page));
    out.push('\n');
    out.push_str("### Answered only (secondary)\n\n");
    out.push_str(
        "Failed queries are left out of this table. It separates \"the backend is worse\"\n\
         from \"the backend is down\"; it is never the number to publish, because a\n\
         backend that fails exactly the hard queries scores higher here.\n\n",
    );
    out.push_str("| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    out.push_str(&answered_row("document", &m.document.answered));
    out.push_str(&answered_row("page", &m.page.answered));
    out.push('\n');
    out.push_str(&format!(
        "Latency over {} answered queries (ms): mean {:.1}, p50 {:.1}, p95 {:.1}, p99 {:.1}\n",
        m.latency_ms.n, m.latency_ms.mean, m.latency_ms.p50, m.latency_ms.p95, m.latency_ms.p99,
    ));
    if m.n_failed > 0 {
        out.push_str(&format!(
            "\n> {} of {} queries failed. Each one is a zero in the strict table above and\n\
             > is missing from the answered-only table.\n",
            m.n_failed, m.n_queries
        ));
    }
    if m.n_ingest_failed > 0 {
        out.push_str(&format!(
            "\n> {} corpus file(s) never reached `success`, so every query about them\n\
             > is scored as a miss.\n",
            m.n_ingest_failed
        ));
    }
    out.push_str(&render_failure_table(&m.failures));
    out.push_str(
        "\n`n` is the number of queries evaluable at that level: a query with no judged\n\
         document, or no judged page, is left out rather than scored as zero.\n",
    );
    out
}

fn answered_row(name: &str, means: &Means) -> String {
    format!(
        "| {} (answered only) | {} | {} | {} | {} | {} | {} |\n",
        name,
        means.n_queries,
        fmt_agg(means.ndcg.mean, means.ndcg.ci_low, means.ndcg.ci_high),
        fmt_agg(means.recall.mean, means.recall.ci_low, means.recall.ci_high),
        fmt_agg(means.mrr.mean, means.mrr.ci_low, means.mrr.ci_high),
        fmt_agg(means.hit1.mean, means.hit1.ci_low, means.hit1.ci_high),
        fmt_agg(
            means.precision.mean,
            means.precision.ci_low,
            means.precision.ci_high
        ),
    )
}

fn level_row(name: &str, level: &LevelReport) -> String {
    format!(
        "| {} | {} | {} | {} | {} | {} | {} |\n",
        name,
        level.n_queries,
        fmt_agg(level.ndcg.mean, level.ndcg.ci_low, level.ndcg.ci_high),
        fmt_agg(level.recall.mean, level.recall.ci_low, level.recall.ci_high),
        fmt_agg(level.mrr.mean, level.mrr.ci_low, level.mrr.ci_high),
        fmt_agg(level.hit1.mean, level.hit1.ci_low, level.hit1.ci_high),
        fmt_agg(
            level.precision.mean,
            level.precision.ci_low,
            level.precision.ci_high
        ),
    )
}

fn fmt_agg(mean: f64, low: f64, high: f64) -> String {
    // Normalise negative zero: a metric that is exactly zero prints as 0.000,
    // never as -0.000.
    let clean = |v: f64| if v == 0.0 { 0.0 } else { v };
    format!("{:.3} [{:.3}–{:.3}]", clean(mean), clean(low), clean(high))
}

/// Write a TREC run file: `qid Q0 docno rank score tag`.
///
/// # Errors
///
/// An I/O error from writing the file.
pub fn write_trec(path: &Path, rankings: &[TrecRanking], tag: &str) -> io::Result<()> {
    let mut out = String::new();
    for ranking in rankings {
        for (i, docno) in ranking.docnos.iter().enumerate() {
            let rank = i + 1;
            let score = 1.0 / rank as f64;
            out.push_str(&format!(
                "{} Q0 {} {} {score:.6} {tag}\n",
                ranking.query_id, docno, rank
            ));
        }
    }
    fs::write(path, out)
}

/// Parse a TREC run file into per-query ranked docnos, ordered by rank.
///
/// # Errors
///
/// A message naming the first malformed line.
pub fn parse_trec(text: &str) -> Result<Vec<TrecRanking>, String> {
    struct Line {
        qid: String,
        docno: String,
        rank: u64,
        order: usize,
    }
    let mut lines = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() < 4 {
            return Err(format!(
                "line {}: expected at least 4 fields, got {}",
                idx + 1,
                fields.len()
            ));
        }
        let rank: u64 = fields[3].parse().map_err(|e| {
            format!(
                "line {}: rank {:?} is not an integer: {e}",
                idx + 1,
                fields[3]
            )
        })?;
        lines.push(Line {
            qid: fields[0].to_string(),
            docno: fields[2].to_string(),
            rank,
            order: idx,
        });
    }
    lines.sort_by(|a, b| {
        a.qid
            .cmp(&b.qid)
            .then(a.rank.cmp(&b.rank))
            .then(a.order.cmp(&b.order))
    });
    let mut out: Vec<TrecRanking> = Vec::new();
    for line in lines {
        match out.last_mut() {
            Some(last) if last.query_id == line.qid => last.docnos.push(line.docno),
            _ => out.push(TrecRanking {
                query_id: line.qid,
                docnos: vec![line.docno],
            }),
        }
    }
    Ok(out)
}

/// Score an existing document-level run file against qrels, offline.
///
/// This is the same arithmetic the online runner uses, so a cross-check against
/// an external tool compares like with like. Returns the report as a string.
#[must_use]
pub fn score_offline(qrels: &[Qrel], rankings: &[TrecRanking], k: usize, gain: Gain) -> String {
    let mut per_query: HashMap<&str, Vec<&Qrel>> = HashMap::new();
    let mut documents: HashSet<String> = HashSet::new();
    for qrel in qrels {
        per_query
            .entry(qrel.query_id.as_str())
            .or_default()
            .push(qrel);
        documents.insert(crate::dataset::base_doc_key(&qrel.corpus_id).to_string());
    }
    let mut doc_metrics = Vec::new();
    let mut page_metrics = Vec::new();
    let mut doc_answered = Vec::new();
    let mut page_answered = Vec::new();
    let mut n_queries = 0;
    for ranking in rankings {
        let Some(query_qrels) = per_query.get(ranking.query_id.as_str()) else {
            continue;
        };
        n_queries += 1;
        let owned: Vec<Qrel> = query_qrels.iter().map(|q| (*q).clone()).collect();
        let doc_gold: Gold = gold_for_documents(&owned);
        let page_gold: Gold = gold_for_pages(&owned);
        let doc_ranked = unique_first_occurrence(ranking.docnos.iter().cloned());
        if let Some(m) = evaluate_query(&doc_ranked, &doc_gold, k, gain) {
            doc_metrics.push(m);
            doc_answered.push(m);
        }
        // A run file of document-level keys carries no pages, so the page arm
        // is empty here; scoring `run.page.trec` is what fills it.
        let page_ranked: Vec<String> = doc_ranked
            .into_iter()
            .filter(|key| crate::dataset::page_of(key).is_some())
            .collect();
        if let Some(m) = evaluate_query(&page_ranked, &page_gold, k, gain) {
            page_metrics.push(m);
            page_answered.push(m);
        }
    }
    let metrics = MetricsFile {
        label: "score".to_string(),
        dataset: "offline".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        harness_commit: harness_commit(),
        gain: gain.as_str().to_string(),
        server_version: None,
        server_commit: None,
        k,
        max_chunks: 0,
        n_queries,
        n_failed: 0,
        n_degraded: 0,
        n_noted: 0,
        n_documents: documents.len(),
        n_ingest_failed: 0,
        degrades: Vec::new(),
        failures: Vec::new(),
        document: aggregate_level(&doc_metrics, &doc_answered),
        page: aggregate_level(&page_metrics, &page_answered),
        latency_ms: LatencyReport {
            mean: 0.0,
            p50: 0.0,
            p95: 0.0,
            p99: 0.0,
            n: 0,
        },
    };
    render_markdown(&metrics)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn trec_round_trips_through_write_and_parse() {
        let rankings = vec![
            TrecRanking {
                query_id: "q1".into(),
                docnos: vec!["a".into(), "b".into()],
            },
            TrecRanking {
                query_id: "q2".into(),
                docnos: vec!["c".into()],
            },
        ];
        let dir = std::env::temp_dir().join(format!("kb-eval-trec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.trec");
        write_trec(&path, &rankings, "test").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // rank 1 gets score 1/1, rank 2 gets 1/2.
        assert!(text.contains("q1 Q0 a 1 1.000000 test"));
        assert!(text.contains("q1 Q0 b 2 0.500000 test"));
        let parsed = parse_trec(&text).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].docnos, vec!["a", "b"]);
        assert_eq!(parsed[1].docnos, vec!["c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_trec_orders_by_rank_not_file_order() {
        let text = "q1 Q0 second 2 0.5 x\nq1 Q0 first 1 1.0 x\n";
        let parsed = parse_trec(text).unwrap();
        assert_eq!(parsed[0].docnos, vec!["first", "second"]);
    }

    #[test]
    fn score_offline_matches_a_perfect_run() {
        let qrels = vec![Qrel {
            query_id: "q1".into(),
            corpus_id: "a".into(),
            score: 1,
        }];
        let rankings = vec![TrecRanking {
            query_id: "q1".into(),
            docnos: vec!["a".into()],
        }];
        let report = score_offline(&qrels, &rankings, 10, Gain::Linear);
        assert!(report.contains("1.000"), "{report}");
    }

    fn empty_parts<'a>(server_version: Option<&'a str>) -> ReportParts<'a> {
        ReportParts {
            label: "run",
            dataset: "tiny",
            gain: Gain::Linear,
            server_version,
            k: 10,
            max_chunks: 50,
            n_queries: 0,
            n_failed: 0,
            n_degraded: 0,
            n_noted: 0,
            n_documents: 0,
            n_ingest_failed: 0,
            degrades: Vec::new(),
            failures: Vec::new(),
            document: Vec::new(),
            page: Vec::new(),
            document_answered: Vec::new(),
            page_answered: Vec::new(),
            latency: LatencyReport {
                mean: 0.0,
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                n: 0,
            },
            document_run: Vec::new(),
            page_run: Vec::new(),
        }
    }

    #[test]
    fn a_failure_carries_status_code_and_the_head_of_the_body() {
        let body = format!(
            r#"{{"code":"unauthorized","detail":"{}"}}"#,
            "x".repeat(400)
        );
        let failure = Failure::from_response(FailureStage::Query, "q1", 401, &body);
        assert_eq!(failure.status, 401);
        assert_eq!(failure.code.as_deref(), Some("unauthorized"));
        assert_eq!(failure.id, "q1");
        assert_eq!(failure.stage, FailureStage::Query);
        // Truncated on a character boundary, and marked as truncated.
        assert_eq!(failure.body.chars().count(), FAILURE_BODY_CHARS + 1);
        assert!(failure.body.ends_with('…'), "{}", failure.body);
        assert!(
            failure.summary().contains("HTTP 401"),
            "{}",
            failure.summary()
        );
    }

    #[test]
    fn a_body_with_no_code_reports_no_code() {
        // kb-server answers `{"error": …}`. Inventing a code for it would put a
        // fabrication in an artifact people trust.
        let failure = Failure::from_response(
            FailureStage::Query,
            "q1",
            500,
            r#"{"error":"Internal error"}"#,
        );
        assert_eq!(failure.code, None);
        assert_eq!(error_code("not json at all"), None);
        assert_eq!(error_code(r#"{"code":""}"#), None);
        assert_eq!(error_code(r#"{"code":7}"#), None);
        assert_eq!(
            error_code(r#"{"type":"about:blank","code":"not_found"}"#),
            Some("not_found".to_string())
        );
    }

    #[test]
    fn a_local_failure_says_it_never_got_a_response() {
        let failure = Failure::local(FailureStage::Ingest, "coffee", "the job did not finish");
        assert_eq!(failure.status, 0);
        assert_eq!(failure.stage.as_str(), "ingest");
        assert_eq!(failure.summary(), "ingest coffee failed: no HTTP response");
    }

    #[test]
    fn failures_group_by_what_went_wrong_not_by_who_it_happened_to() {
        let mut failures = vec![
            Failure::from_response(FailureStage::Query, "q1", 401, r#"{"code":"unauthorized"}"#),
            Failure::from_response(FailureStage::Query, "q2", 401, r#"{"code":"unauthorized"}"#),
            Failure::from_response(FailureStage::Query, "q3", 500, r#"{"error":"boom"}"#),
            Failure::from_response(FailureStage::Query, "q4", 401, r#"{"code":"unauthorized"}"#),
            Failure::from_response(
                FailureStage::Ingest,
                "coffee",
                413,
                r#"{"error":"too big"}"#,
            ),
        ];
        let groups = group_failures(&failures);
        assert_eq!(groups.len(), 3, "{groups:?}");
        // Ordered by stage, status, code: the ingest failure comes first, then
        // the two query causes.
        assert_eq!(groups[0].stage, FailureStage::Ingest);
        assert_eq!(groups[0].status, 413);
        assert_eq!(groups[0].count, 1);
        assert_eq!(groups[1].status, 401);
        assert_eq!(groups[1].count, 3);
        assert_eq!(groups[1].code.as_deref(), Some("unauthorized"));
        // The first id that produced it is enough to reproduce the failure.
        assert_eq!(groups[1].example_id, "q1");
        assert_eq!(groups[2].status, 500);
        assert_eq!(groups[2].code, None);

        let table = render_failure_table(&failures);
        assert!(
            table.contains("| query | HTTP 401 `unauthorized` | 3 | `q1` |"),
            "{table}"
        );
        assert!(table.contains("(no error code)"), "{table}");
        assert!(render_failure_table(&[]).is_empty());
        assert!(!group_failures(&failures).is_empty());
        failures.clear();
        assert!(failures.is_empty());
    }

    #[test]
    fn the_api_key_is_scrubbed_from_a_recorded_body() {
        let mut failure = Failure::from_response(
            FailureStage::Query,
            "q1",
            401,
            "key rkb_live_ABC-123 rejected",
        );
        failure.redact("rkb_live_ABC-123");
        assert!(!failure.body.contains("ABC-123"), "{}", failure.body);
        assert!(failure.body.contains("[redacted]"), "{}", failure.body);
        // An empty secret must not turn every gap into "[redacted]".
        assert_eq!(redact_secret("abc", "  "), "abc");
    }

    #[test]
    fn the_report_says_why_each_query_failed() {
        let report = build_report(ReportParts {
            failures: vec![
                Failure::from_response(
                    FailureStage::Query,
                    "q1",
                    401,
                    r#"{"code":"unauthorized"}"#,
                ),
                Failure::from_response(
                    FailureStage::Query,
                    "q2",
                    401,
                    r#"{"code":"unauthorized"}"#,
                ),
                Failure::from_response(
                    FailureStage::Ingest,
                    "rust-lang",
                    415,
                    r#"{"error":"nope"}"#,
                ),
            ],
            ..empty_parts(Some("9.9.9"))
        });
        assert_eq!(report.metrics.failures.len(), 3);
        let md = render_markdown(&report.metrics);
        assert!(md.contains("## Why they failed"), "{md}");
        assert!(md.contains("HTTP 401 `unauthorized`"), "{md}");
        assert!(md.contains("| ingest | HTTP 415"), "{md}");
        // Every failure, one per line, is in the JSON.
        let json = serde_json::to_string_pretty(&report.metrics).expect("json");
        assert!(json.contains("\"stage\": \"query\""), "{json}");
        assert!(json.contains("\"code\": \"unauthorized\""), "{json}");
        assert!(json.contains("\"id\": \"rust-lang\""), "{json}");
    }

    #[test]
    fn an_unidentified_server_says_so_rather_than_guessing() {
        // `/health` unreachable: the report must name the gap, not fill it in
        // with the harness's own commit (a different binary entirely).
        let report = build_report(empty_parts(None));
        assert!(report.metrics.server_version.is_none());
        assert!(report.metrics.server_commit.is_none());
        let md = render_markdown(&report.metrics);
        assert!(
            md.contains("server version: unknown (GET /health did not answer"),
            "{md}"
        );
        assert!(md.contains("server commit: null"), "{md}");
        assert!(
            md.contains(&format!("harness commit: `{}`", harness_commit())),
            "{md}"
        );
    }

    #[test]
    fn n_queries_is_what_was_asked_not_what_was_scored() {
        let scored = evaluate_query(
            &["a".to_string()],
            &HashMap::from([("a".to_string(), 1)]),
            10,
            Gain::Linear,
        )
        .expect("evaluable");
        let report = build_report(ReportParts {
            label: "run",
            dataset: "tiny",
            gain: Gain::Linear,
            server_version: Some("9.9.9"),
            k: 10,
            max_chunks: 50,
            n_queries: 5,
            n_failed: 2,
            n_degraded: 1,
            n_noted: 1,
            n_documents: 6,
            n_ingest_failed: 1,
            degrades: vec!["vector arm unavailable".to_string()],
            failures: Vec::new(),
            document: vec![scored, QueryMetrics::ZERO],
            page: Vec::new(),
            document_answered: vec![scored],
            page_answered: Vec::new(),
            latency: LatencyReport {
                mean: 1.0,
                p50: 1.0,
                p95: 1.0,
                p99: 1.0,
                n: 3,
            },
            document_run: vec![TrecRanking {
                query_id: "q1".into(),
                docnos: vec!["a".into()],
            }],
            page_run: Vec::new(),
        });
        // Five queries were attempted; one answered query and one failure were
        // evaluable at the document level, so the strict mean is over two.
        assert_eq!(report.metrics.n_queries, 5);
        assert_eq!(report.metrics.document.n_queries, 2);
        assert_eq!(report.metrics.document.answered.n_queries, 1);
        assert!((report.metrics.document.ndcg.mean - 0.5).abs() < 1e-9);
        assert!((report.metrics.document.answered.ndcg.mean - 1.0).abs() < 1e-9);
        assert_eq!(report.metrics.n_failed, 2);

        let markdown = render_markdown(&report.metrics);
        assert!(
            markdown.contains("queries: 5 (failed: 2, degraded: 1, noted: 1)"),
            "{markdown}"
        );
        assert!(markdown.contains("vector arm unavailable"), "{markdown}");
        assert!(markdown.contains("corpus files that failed to ingest: 1"));
        assert!(markdown.contains("2 of 5 queries failed"), "{markdown}");
        assert!(
            markdown.contains("Strict means: every evaluable query is in them"),
            "{markdown}"
        );
        assert!(
            markdown.contains("| document (answered only) | 1 | 1.000"),
            "{markdown}"
        );
        assert!(
            markdown.contains("1 corpus file(s) never reached `success`"),
            "{markdown}"
        );

        let json = serde_json::to_string(&report.metrics).expect("json");
        assert!(json.contains("\"n_failed\":2"), "{json}");
        assert!(json.contains("\"n_ingest_failed\":1"), "{json}");
        assert!(json.contains("\"harness_commit\""), "{json}");
        assert!(json.contains("\"gain\":\"linear\""), "{json}");
        assert!(json.contains("\"server_version\":\"9.9.9\""), "{json}");
        assert!(json.contains("\"server_commit\":null"), "{json}");
        assert!(markdown.contains("nDCG gain: **linear**"), "{markdown}");
        assert!(
            markdown.contains("server version: `9.9.9` (GET /health)"),
            "{markdown}"
        );
        assert!(
            markdown.contains("server commit: null — no kb-server route exposes a build commit"),
            "{markdown}"
        );
    }
}
