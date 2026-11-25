//! EVAL-7 — the progressive-disclosure coverage gate.
//!
//! A *scripted* reader (no LLM) walks the public API for every question in a
//! page-labelled dataset:
//!
//! ```text
//! POST /v1/search             -> hits, in rank order
//! GET  /documents/:id/structure (once per document)
//! GET  /documents/:id/content   (the hit's section, or a one-page window)
//! ```
//!
//! It reads under a character budget of `budget_factor ×` the characters of the
//! human-labelled gold pages. Coverage is the fraction of gold pages whose text
//! appears, after whitespace normalisation, in the text the reader actually
//! fetched. Gold is external (ViDoRe v3 HR page qrels), so the number measures
//! the API's ability to expose the right page, not a model's ability to find it.
//!
//! The scoring arithmetic in this module is pure and unit-tested; only
//! [`score_question`] and [`run`] speak HTTP. Gold-page reads are deliberately
//! outside both the budget and the latency samples: they are the instrument's
//! calibration, not the reader's work.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::EvalError;
use crate::dataset::{Qrel, base_doc_key, page_of, parse_qrels, parse_queries};
use crate::metrics::{Aggregate, LatencyReport, bootstrap_ci, latency_report};
use crate::report::redact_secret;

/// 95% bootstrap interval over question-level coverage, with a fixed seed.
pub const BOOTSTRAP_RESAMPLES: usize = 2000;
/// Fixed seed: two runs over the same answers give the same interval.
pub const BOOTSTRAP_SEED: u64 = 0x5EED_0000_0000_0007;
/// Primary target (EVAL-9): at least one gold page fully read. ViDoRe HR has a
/// median of 3 and up to 30 gold pages per question, so demanding *every* page
/// measured the dataset more than the API.
pub const ANSWER_PAGE_COVERAGE_TARGET: f64 = 0.80;
/// Secondary target (EVAL-7, retained for continuity): every gold page read.
pub const COVERAGE_TARGET: f64 = 0.90;
/// Maximum mean chars read / gold chars.
pub const CHARS_RATIO_TARGET: f64 = 1.5;
/// Maximum p95 of `/v1/documents/:id/content`, in milliseconds.
pub const CONTENT_P95_TARGET_MS: f64 = 50.0;
/// API default, and the reader's request size.
const CONTENT_MAX_CHARS: usize = 8_000;
const CONTENT_MIN_CHARS: usize = 500;
const CONTENT_HARD_MAX: usize = 50_000;
/// A runaway cursor must not spin forever.
const MAX_CURSOR_PAGES: usize = 1_000;

// ─── configuration and outputs ───────────────────────────────────────────────

/// Everything the harness needs from the operator.
#[derive(Debug, Clone)]
pub struct DisclosureConfig {
    pub base_url: String,
    pub api_key: String,
    pub dataset: PathBuf,
    pub out_dir: PathBuf,
    pub top_k: u32,
    pub budget_factor: f64,
    pub limit: Option<usize>,
    /// Overrides auto-discovery of `<doc-key> -> documentId`.
    pub ingest_map: Option<PathBuf>,
    /// Overrides the query file auto-discovered in `--dataset`.
    pub queries: Option<PathBuf>,
    /// Overrides the qrels file auto-discovered in `--dataset`.
    pub qrels: Option<PathBuf>,
}

/// One human-labelled page: `<doc-key>#p<N>`, `N` 1-based (qrels convention).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldPage {
    pub corpus_id: String,
    pub doc_key: String,
    pub page_1based: u32,
}

/// One `/v1/search` hit as the reader saw it (EVAL-9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HitRecord {
    /// 1-based rank in the search response.
    pub rank: usize,
    pub document_id: String,
    /// 0-based page, exactly as the API reported it (`null` when absent).
    pub page: Option<u32>,
    /// Whether this (document, page) is one of the question's gold pages.
    pub gold: bool,
}

/// One budgeted `/content` read as the reader made it (EVAL-9). Recording these
/// separates "search ranked the gold page low" from "the reader read the wrong
/// page" in `per_question.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageReadRecord {
    pub document_id: String,
    /// `section:s3`, `pages:4-6` or `cursor`, so a reviewer can see the selector.
    pub selector: String,
    pub chunk_start: u32,
    pub chunk_end: u32,
    pub page_start: Option<u32>,
    pub page_end: Option<u32>,
    /// Characters kept from this read under the budget.
    pub chars: u64,
}

/// Distribution of the first gold hit's rank across questions (EVAL-9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankDistribution {
    /// `(1-based rank, question count)` for every rank that occurred, sorted.
    pub counts: Vec<(usize, usize)>,
    /// Questions with a gold page in the top-k.
    pub any: usize,
    /// Questions with no gold page in the top-k.
    pub none: usize,
    /// Mean rank among questions that had a gold hit; 0 when none did.
    pub mean: f64,
}

/// The result for one question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionResult {
    pub query_id: String,
    pub query: String,
    /// EVAL-7: fraction of *all* gold pages fully read. Secondary now.
    pub coverage: f64,
    pub covered: usize,
    pub gold_pages: usize,
    pub missing_doc: usize,
    pub gold_chars: u64,
    pub chars_read: u64,
    pub chars_ratio: f64,
    /// EVAL-9 primary: 1 when at least one gold page was fully read.
    pub answer_page_hit: u8,
    /// 1-based rank of the first search hit on a gold page, `null` if none in
    /// the top-k.
    pub best_gold_rank: Option<usize>,
    pub covered_pages: Vec<String>,
    pub missed_pages: Vec<String>,
    /// Every hit the reader saw, with rank, page and goldness.
    pub hits: Vec<HitRecord>,
    /// Every budgeted `/content` read the reader made, in order.
    pub pages_read: Vec<PageReadRecord>,
    /// Visible failures (HTTP errors, cursor loops, …). Never folded away.
    pub errors: Vec<String>,
}

/// Client-side latency of each endpoint, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencyByEndpoint {
    pub search: LatencyReport,
    pub structure: LatencyReport,
    pub content: LatencyReport,
}

/// PASS/MISS markers. A miss is reported, never hidden or retried away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Targets {
    /// Primary (EVAL-9): answer-page coverage.
    pub answer_page_coverage_pass: bool,
    pub chars_ratio_pass: bool,
    /// Secondary, kept for continuity (EVAL-7): all-gold-pages coverage.
    pub coverage_pass: bool,
    pub content_p95_pass: bool,
}

/// The whole run, written to `results.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisclosureReport {
    pub dataset: String,
    pub top_k: u32,
    pub budget_factor: f64,
    pub bootstrap_resamples: usize,
    pub bootstrap_seed: u64,
    pub n_questions: usize,
    pub missing_doc: usize,
    /// Primary (EVAL-9): mean answer-page hit, with a bootstrap CI.
    pub answer_page_coverage: Aggregate,
    /// Secondary (EVAL-7): mean fraction of *all* gold pages read, with a CI.
    pub coverage: Aggregate,
    /// Coverage restricted to questions with exactly one gold page.
    pub single_gold_coverage: Aggregate,
    /// Where the first gold hit ranked in the top-k.
    pub best_gold_rank: RankDistribution,
    pub chars_ratio: Aggregate,
    pub chars_read_total: u64,
    pub gold_chars_total: u64,
    pub latency_ms: LatencyByEndpoint,
    pub targets: Targets,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Http(String),
}

impl From<reqwest::Error> for RunError {
    fn from(err: reqwest::Error) -> Self {
        Self::Http(err.to_string())
    }
}

// ─── pure scoring ────────────────────────────────────────────────────────────

/// Collapse every run of whitespace to one space and trim the ends.
///
/// A page's text is compared as it was *served*, not as a human would reflow
/// it: this normalisation only removes formatting noise, never words.
#[must_use]
pub fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Per-page coverage of one question.
#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    pub covered: usize,
    pub total: usize,
    pub fraction: f64,
    /// In gold-page order, aligned with the caller's gold list.
    pub flags: Vec<bool>,
}

/// Whether each gold page's normalised text is a substring of the concatenated
/// text the reader fetched. An empty gold page cannot be verified, so it is
/// counted as uncovered rather than inflating the score.
#[must_use]
pub fn coverage(gold_texts: &[String], read_parts: &[String]) -> Coverage {
    let haystack = normalize_whitespace(&read_parts.join(" "));
    let flags: Vec<bool> = gold_texts
        .iter()
        .map(|gold| {
            let needle = normalize_whitespace(gold);
            !needle.is_empty() && haystack.contains(&needle)
        })
        .collect();
    let covered = flags.iter().filter(|&&yes| yes).count();
    let total = flags.len();
    Coverage {
        covered,
        total,
        fraction: if total == 0 {
            0.0
        } else {
            covered as f64 / total as f64
        },
        flags,
    }
}

/// EVAL-9 primary signal: at least one gold page was fully read.
#[must_use]
pub fn answer_page_hit(flags: &[bool]) -> u8 {
    u8::from(flags.iter().any(|&yes| yes))
}

/// The 1-based rank of the first search hit that lands on a gold page.
///
/// `gold` holds `(document id, 0-based page)` pairs resolved through the ingest
/// map. A hit without a page cannot be a gold-page hit. `None` means no gold
/// page appeared in the top-k, which is a fact about search, not the reader.
#[must_use]
pub fn best_gold_rank(
    hits: &[(String, Option<u32>)],
    gold: &HashSet<(String, u32)>,
) -> Option<usize> {
    hits.iter()
        .position(|(document, page)| {
            page.is_some_and(|page| gold.contains(&(document.clone(), page)))
        })
        .map(|position| position + 1)
}

/// Count how often the first gold hit appeared at each rank, plus the misses.
#[must_use]
pub fn rank_distribution(values: &[Option<usize>], k: usize) -> RankDistribution {
    let mut by_rank: HashMap<usize, usize> = HashMap::new();
    let mut any = 0usize;
    let mut sum = 0usize;
    for value in values.iter().flatten() {
        if *value >= 1 && *value <= k {
            *by_rank.entry(*value).or_default() += 1;
            any += 1;
            sum += *value;
        }
    }
    let mut counts: Vec<(usize, usize)> = by_rank.into_iter().collect();
    counts.sort_unstable_by_key(|(rank, _)| *rank);
    RankDistribution {
        counts,
        any,
        none: values.len() - any,
        mean: if any == 0 {
            0.0
        } else {
            sum as f64 / any as f64
        },
    }
}

/// The reader's character budget for a question.
///
/// Non-finite or non-positive factors produce a zero budget: a misconfigured
/// gate must fail visibly, not turn into an unbounded read.
#[must_use]
pub fn budget_chars(gold_chars: u64, factor: f64) -> u64 {
    if !factor.is_finite() || factor <= 0.0 {
        return 0;
    }
    (gold_chars as f64 * factor).floor() as u64
}

/// Take at most `max` Unicode scalar values, without cutting a character in two.
#[must_use]
pub fn take_chars(text: &str, max: u64) -> String {
    text.chars()
        .take(usize::try_from(max).unwrap_or(usize::MAX))
        .collect()
}

/// Group hits by document, keeping both hit order and first-seen document order.
#[must_use]
pub fn group_hits_by_document(hits: &[(String, Option<u32>)]) -> Vec<(String, Vec<Option<u32>>)> {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<Option<u32>>> = HashMap::new();
    for (id, page) in hits {
        if !grouped.contains_key(id) {
            order.push(id.clone());
        }
        grouped.entry(id.clone()).or_default().push(*page);
    }
    order
        .into_iter()
        .filter_map(|id| grouped.remove(&id).map(|pages| (id, pages)))
        .collect()
}

/// A section of `/structure` as the reader needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionRef {
    pub id: String,
    pub page_start: Option<u32>,
    pub page_end: Option<u32>,
    pub chunk_start: u32,
    pub chunk_end: u32,
}

/// The first section whose page range contains the hit's 0-based page.
#[must_use]
pub fn section_for_page(sections: &[SectionRef], page_0based: u32) -> Option<&SectionRef> {
    sections.iter().find(|section| {
        matches!(
            (section.page_start, section.page_end),
            (Some(start), Some(end)) if start <= page_0based && page_0based <= end
        )
    })
}

/// The fallback page window around a hit: `[page-1, page+1]`, clamped at zero.
#[must_use]
pub fn page_window(page_0based: u32) -> (u32, u32) {
    (page_0based.saturating_sub(1), page_0based.saturating_add(1))
}

/// Whether two inclusive chunk ranges share a chunk.
#[must_use]
pub fn intervals_overlap(a: (u32, u32), b: (u32, u32)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

/// What the reader asked `/content` for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReadSelector {
    Section(String),
    Pages(u32, u32),
    Cursor(String),
}

impl ReadSelector {
    /// Short, stable label written to `per_question.jsonl`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Section(id) => format!("section:{id}"),
            Self::Pages(start, end) => format!("pages:{start}-{end}"),
            Self::Cursor(_) => "cursor".to_string(),
        }
    }
}

/// Tracks selections and served chunk ranges so no chunk range is fetched twice.
///
/// Both keys include the document id: `s1` in document A and `s1` in document B
/// are different reads, as are chunk 0 of each.
#[derive(Debug, Default, Clone)]
pub struct ReadLedger {
    selectors: HashSet<(String, ReadSelector)>,
    ranges: Vec<(String, (u32, u32))>,
}

impl ReadLedger {
    /// Mark a selection as visited; returns `false` when it was already visited.
    pub fn mark_selector(&mut self, document_id: &str, selector: &ReadSelector) -> bool {
        self.selectors
            .insert((document_id.to_string(), selector.clone()))
    }

    /// Whether any chunk of `range` in this document was already served.
    #[must_use]
    pub fn overlaps_read(&self, document_id: &str, range: (u32, u32)) -> bool {
        self.ranges
            .iter()
            .any(|(doc, seen)| doc == document_id && intervals_overlap(*seen, range))
    }

    /// Record the chunk range a `/content` response actually served.
    pub fn record_range(&mut self, document_id: &str, range: (u32, u32)) {
        self.ranges.push((document_id.to_string(), range));
    }
}

/// Evaluate the EVAL-9 gate: answer-page coverage at a bounded char ratio, with
/// the old all-pages coverage and `/content` p95 kept as secondary diagnostics.
#[must_use]
pub fn evaluate_targets(
    answer_page_coverage: f64,
    coverage_mean: f64,
    chars_ratio: f64,
    content_p95_ms: f64,
) -> Targets {
    Targets {
        answer_page_coverage_pass: answer_page_coverage >= ANSWER_PAGE_COVERAGE_TARGET,
        chars_ratio_pass: chars_ratio <= CHARS_RATIO_TARGET,
        coverage_pass: coverage_mean >= COVERAGE_TARGET,
        content_p95_pass: content_p95_ms < CONTENT_P95_TARGET_MS,
    }
}

/// A human-readable table plus the PASS/MISS lines.
#[must_use]
pub fn render_report(report: &DisclosureReport, questions: &[QuestionResult]) -> String {
    let mut out = String::new();
    out.push_str("# EVAL-9 — answer-page disclosure coverage (EVAL-7 metrics retained)\n\n");
    out.push_str(&format!(
        "Dataset `{}`, top-k {}, budget factor {}, {} questions.\n\n",
        report.dataset, report.top_k, report.budget_factor, report.n_questions
    ));
    out.push_str(
        "| query | answer hit | best gold rank | coverage | covered/gold | missing doc | chars read | gold chars | ratio |\n",
    );
    out.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for q in questions {
        let rank = q
            .best_gold_rank
            .map_or_else(|| "none".to_string(), |rank| rank.to_string());
        out.push_str(&format!(
            "| {} | {} | {} | {:.3} | {}/{} | {} | {} | {} | {:.3} |\n",
            q.query_id,
            q.answer_page_hit,
            rank,
            q.coverage,
            q.covered,
            q.gold_pages,
            q.missing_doc,
            q.chars_read,
            q.gold_chars,
            q.chars_ratio
        ));
    }
    out.push_str(&format!(
        "\nAnswer-page coverage **{:.3}** [{:.3}, {:.3}] (n={}), single-gold-page coverage **{:.3}** [{:.3}, {:.3}] (n={}) ({} bootstrap resamples, seed {}).\n",
        report.answer_page_coverage.mean,
        report.answer_page_coverage.ci_low,
        report.answer_page_coverage.ci_high,
        report.answer_page_coverage.n,
        report.single_gold_coverage.mean,
        report.single_gold_coverage.ci_low,
        report.single_gold_coverage.ci_high,
        report.single_gold_coverage.n,
        report.bootstrap_resamples,
        report.bootstrap_seed
    ));
    let ranks: Vec<String> = report
        .best_gold_rank
        .counts
        .iter()
        .map(|(rank, count)| format!("rank {rank}: {count}"))
        .collect();
    out.push_str(&format!(
        "Best gold rank: {}; no gold in top-k: {}; mean rank among hits {:.2}.\n",
        if ranks.is_empty() {
            "no gold hits".to_string()
        } else {
            ranks.join(", ")
        },
        report.best_gold_rank.none,
        report.best_gold_rank.mean
    ));
    out.push_str(&format!(
        "All-gold-pages coverage (secondary) **{:.3}** [{:.3}, {:.3}].\n",
        report.coverage.mean, report.coverage.ci_low, report.coverage.ci_high
    ));
    out.push_str(&format!(
        "Mean chars read / gold chars **{:.3}** [{:.3}, {:.3}]; totals {} / {}.\n",
        report.chars_ratio.mean,
        report.chars_ratio.ci_low,
        report.chars_ratio.ci_high,
        report.chars_read_total,
        report.gold_chars_total
    ));
    out.push_str(&format!(
        "Client latency (ms): search p50 {:.1} p95 {:.1} (n={}); structure p50 {:.1} p95 {:.1} (n={}); content p50 {:.1} p95 {:.1} (n={}).\n",
        report.latency_ms.search.p50,
        report.latency_ms.search.p95,
        report.latency_ms.search.n,
        report.latency_ms.structure.p50,
        report.latency_ms.structure.p95,
        report.latency_ms.structure.n,
        report.latency_ms.content.p50,
        report.latency_ms.content.p95,
        report.latency_ms.content.n
    ));
    out.push_str(&format!(
        "Gold pages with an ingested document missing: {}.\n\n",
        report.missing_doc
    ));
    let verdict = |pass: bool| if pass { "PASS" } else { "MISS" };
    out.push_str("## Targets\n\n");
    out.push_str("| Target | Measured | Result |\n|---|---:|---|\n");
    out.push_str(&format!(
        "| **answer-page coverage ≥ {:.2} (primary)** | {:.3} | {} |\n",
        ANSWER_PAGE_COVERAGE_TARGET,
        report.answer_page_coverage.mean,
        verdict(report.targets.answer_page_coverage_pass)
    ));
    out.push_str(&format!(
        "| chars read / gold chars ≤ {:.1} | {:.3} | {} |\n",
        CHARS_RATIO_TARGET,
        report.chars_ratio.mean,
        verdict(report.targets.chars_ratio_pass)
    ));
    out.push_str(&format!(
        "| all-gold-pages coverage ≥ {:.2} (secondary) | {:.3} | {} |\n",
        COVERAGE_TARGET,
        report.coverage.mean,
        verdict(report.targets.coverage_pass)
    ));
    out.push_str(&format!(
        "| /content p95 < {:.0} ms | {:.1} ms | {} |\n",
        CONTENT_P95_TARGET_MS,
        report.latency_ms.content.p95,
        verdict(report.targets.content_p95_pass)
    ));
    out
}

// ─── dataset loading ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct DatasetFiles {
    queries: PathBuf,
    qrels: PathBuf,
}

fn dataset_files(config: &DisclosureConfig) -> Result<DatasetFiles, RunError> {
    if let (Some(queries), Some(qrels)) = (&config.queries, &config.qrels) {
        return Ok(DatasetFiles {
            queries: queries.clone(),
            qrels: qrels.clone(),
        });
    }
    let candidates = [
        ("queries.jsonl", "qrels/test.tsv"),
        ("queries.nonaggregative.jsonl", "qrels.nonaggregative.tsv"),
        ("queries.aggregative.jsonl", "qrels.aggregative.tsv"),
    ];
    for (query_name, qrel_name) in candidates {
        let queries = config
            .queries
            .clone()
            .unwrap_or_else(|| config.dataset.join(query_name));
        let qrels = config
            .qrels
            .clone()
            .unwrap_or_else(|| config.dataset.join(qrel_name));
        if queries.is_file() && qrels.is_file() {
            return Ok(DatasetFiles { queries, qrels });
        }
    }
    Err(RunError::Usage(format!(
        "no query/qrels pair found in {} (looked for {}, {}; pass --queries and --qrels)",
        config.dataset.display(),
        candidates[0].0,
        candidates[1].0
    )))
}

/// Positive page-level qrels, grouped by query id in file order.
#[must_use]
pub fn gold_by_query(qrels: &[Qrel]) -> HashMap<String, Vec<GoldPage>> {
    let mut out: HashMap<String, Vec<GoldPage>> = HashMap::new();
    for qrel in qrels {
        if qrel.score <= 0 {
            continue;
        }
        let Some(page) = page_of(&qrel.corpus_id) else {
            continue;
        };
        out.entry(qrel.query_id.clone())
            .or_default()
            .push(GoldPage {
                corpus_id: qrel.corpus_id.clone(),
                doc_key: base_doc_key(&qrel.corpus_id).to_string(),
                page_1based: page,
            });
    }
    out
}

fn load_ingest_map(config: &DisclosureConfig) -> Result<HashMap<String, String>, RunError> {
    let candidates = config.ingest_map.clone().map_or_else(
        || {
            vec![
                config.out_dir.join("ingest-map.json"),
                config.dataset.join("ingest-map.json"),
                config.dataset.join("../ingest-map.json"),
            ]
        },
        |path| vec![path],
    );
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let map: HashMap<String, String> = serde_json::from_str(&fs::read_to_string(&path)?)?;
        return Ok(map);
    }
    Err(RunError::Usage(
        "no ingest-map.json found (looked in --out and --dataset); pass --ingest-map FILE mapping doc-key -> documentId".to_string(),
    ))
}

// ─── HTTP plumbing ───────────────────────────────────────────────────────────

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

fn bearer(key: &str) -> String {
    format!("Bearer {key}")
}

async fn send_json(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: reqwest::Url,
    key: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), RunError> {
    let mut request = client
        .request(method, url)
        .header(reqwest::header::AUTHORIZATION, bearer(key));
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let text = response.text().await?;
    if !(200..300).contains(&status) {
        let head: String = text.chars().take(300).collect();
        return Err(RunError::Http(format!(
            "HTTP {status}: {}",
            redact_secret(&head, key)
        )));
    }
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok((status, value))
}

#[derive(Debug, Deserialize)]
struct SearchAnswer {
    #[serde(default)]
    chunks: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    #[serde(rename = "documentId", default)]
    document_id: String,
    /// `null` when the server has no page for a chunk; omitted on old servers.
    #[serde(default)]
    page: Option<Option<u32>>,
}

async fn search_hits(
    client: &reqwest::Client,
    config: &DisclosureConfig,
    query: &str,
    latency: &mut Vec<f64>,
) -> Result<Vec<(String, Option<u32>)>, RunError> {
    let url = reqwest::Url::parse(&endpoint(&config.base_url, "/v1/search"))
        .map_err(|e| RunError::Usage(format!("--base-url is not a URL: {e}")))?;
    let body = json!({ "query": query, "maxChunks": config.top_k });
    let started = Instant::now();
    let (_, value) = send_json(
        client,
        reqwest::Method::POST,
        url,
        &config.api_key,
        Some(&body),
    )
    .await?;
    latency.push(started.elapsed().as_secs_f64() * 1000.0);
    let answer: SearchAnswer = serde_json::from_value(value)?;
    Ok(answer
        .chunks
        .into_iter()
        .map(|hit| (hit.document_id, hit.page.flatten()))
        .collect())
}

#[derive(Debug, Deserialize)]
struct StructureSection {
    #[serde(default)]
    id: String,
    #[serde(rename = "pageStart", default)]
    page_start: Option<u32>,
    #[serde(rename = "pageEnd", default)]
    page_end: Option<u32>,
    #[serde(rename = "chunkStart", default)]
    chunk_start: u32,
    #[serde(rename = "chunkEnd", default)]
    chunk_end: u32,
}

impl From<StructureSection> for SectionRef {
    fn from(section: StructureSection) -> Self {
        Self {
            id: section.id,
            page_start: section.page_start,
            page_end: section.page_end,
            chunk_start: section.chunk_start,
            chunk_end: section.chunk_end,
        }
    }
}

#[derive(Debug, Deserialize)]
struct StructureChunk {
    #[serde(rename = "chunkIndex", default)]
    chunk_index: u32,
    #[serde(default)]
    page: Option<u32>,
}

#[derive(Debug, Clone)]
struct Structure {
    sections: Vec<SectionRef>,
    chunks: Vec<(u32, Option<u32>)>,
}

impl Structure {
    /// The chunk range a page window covers, from the structure's chunk map.
    fn chunk_range_for_pages(&self, pages: (u32, u32)) -> Option<(u32, u32)> {
        let indices: Vec<u32> = self
            .chunks
            .iter()
            .filter_map(|(index, page)| {
                page.and_then(|p| (pages.0 <= p && p <= pages.1).then_some(*index))
            })
            .collect();
        indices
            .iter()
            .min()
            .copied()
            .zip(indices.iter().max().copied())
    }
}

async fn fetch_structure(
    client: &reqwest::Client,
    config: &DisclosureConfig,
    document_id: &str,
    latency: &mut Vec<f64>,
) -> Result<Structure, RunError> {
    let path = format!("/v1/documents/{document_id}/structure");
    let url = reqwest::Url::parse(&endpoint(&config.base_url, &path))
        .map_err(|e| RunError::Usage(format!("--base-url is not a URL: {e}")))?;
    let started = Instant::now();
    let (_, value) = send_json(client, reqwest::Method::GET, url, &config.api_key, None).await?;
    latency.push(started.elapsed().as_secs_f64() * 1000.0);
    #[derive(Debug, Deserialize)]
    struct Wire {
        #[serde(default)]
        sections: Vec<StructureSection>,
        #[serde(default)]
        chunks: Vec<StructureChunk>,
    }
    let wire: Wire = serde_json::from_value(value)?;
    Ok(Structure {
        sections: wire.sections.into_iter().map(Into::into).collect(),
        chunks: wire
            .chunks
            .into_iter()
            .map(|chunk| (chunk.chunk_index, chunk.page))
            .collect(),
    })
}

/// One `/content` response, as the reader uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentPage {
    pub text: String,
    pub chunk_range: (u32, u32),
    /// Inclusive 0-based page range the server reported, when it had one.
    pub page_range: (Option<u32>, Option<u32>),
}

#[derive(Debug, Deserialize)]
struct ContentAnswer {
    #[serde(default)]
    text: String,
    #[serde(default)]
    truncated: bool,
    #[serde(rename = "nextCursor", default)]
    next_cursor: Option<String>,
    #[serde(default, rename = "range")]
    range: ContentRangeAnswer,
    #[serde(default)]
    degrades: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ContentRangeAnswer {
    #[serde(rename = "chunkStart", default)]
    chunk_start: u32,
    #[serde(rename = "chunkEnd", default)]
    chunk_end: u32,
    #[serde(rename = "pageStart", default)]
    page_start: Option<u32>,
    #[serde(rename = "pageEnd", default)]
    page_end: Option<u32>,
}

/// Read a selector from `/content`, following `nextCursor` to the end.
///
/// `latency` is `None` for gold-page reads: those calibrate the budget and are
/// not the reader's work, so their time must not appear in the p95.
///
/// # Errors
///
/// Any non-2xx response or an unparseable answer. A cursor loop is broken with
/// a visible note rather than silently truncating the read.
async fn content_pages(
    client: &reqwest::Client,
    config: &DisclosureConfig,
    document_id: &str,
    selector: &ReadSelector,
    max_chars: usize,
    mut latency: Option<&mut Vec<f64>>,
    notes: &mut Vec<String>,
) -> Result<Vec<ContentPage>, RunError> {
    let path = format!("/v1/documents/{document_id}/content");
    let base = endpoint(&config.base_url, &path);
    let mut cursor: Option<String> = None;
    let max_chars = max_chars.clamp(CONTENT_MIN_CHARS, CONTENT_HARD_MAX);
    let mut pages = Vec::new();
    for _ in 0..MAX_CURSOR_PAGES {
        let mut url = reqwest::Url::parse(&base)
            .map_err(|e| RunError::Usage(format!("--base-url is not a URL: {e}")))?;
        {
            let mut pairs = url.query_pairs_mut();
            match cursor.as_deref() {
                Some(value) => {
                    pairs.append_pair("cursor", value);
                }
                None => match selector {
                    ReadSelector::Section(id) => {
                        pairs.append_pair("section", id);
                    }
                    ReadSelector::Pages(start, end) => {
                        pairs.append_pair("pageStart", &start.to_string());
                        pairs.append_pair("pageEnd", &end.to_string());
                    }
                    ReadSelector::Cursor(value) => {
                        pairs.append_pair("cursor", value);
                    }
                },
            }
            pairs.append_pair("maxChars", &max_chars.to_string());
        }
        let started = Instant::now();
        let (_, value) =
            send_json(client, reqwest::Method::GET, url, &config.api_key, None).await?;
        if let Some(samples) = latency.as_deref_mut() {
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let answer: ContentAnswer = serde_json::from_value(value)?;
        for degrade in &answer.degrades {
            notes.push(format!("{document_id}: {degrade}"));
        }
        pages.push(ContentPage {
            text: answer.text,
            chunk_range: (answer.range.chunk_start, answer.range.chunk_end),
            page_range: (answer.range.page_start, answer.range.page_end),
        });
        if !answer.truncated {
            return Ok(pages);
        }
        match answer.next_cursor {
            Some(next) => cursor = Some(next),
            None => {
                notes.push(format!(
                    "{document_id}: truncated content had no nextCursor"
                ));
                return Ok(pages);
            }
        }
    }
    notes.push(format!(
        "{document_id}: stopped after {MAX_CURSOR_PAGES} cursor pages"
    ));
    Ok(pages)
}

// ─── the run ─────────────────────────────────────────────────────────────────

#[derive(Default)]
struct RunState {
    structures: HashMap<String, Structure>,
    gold_texts: HashMap<String, String>,
    search_ms: Vec<f64>,
    structure_ms: Vec<f64>,
    content_ms: Vec<f64>,
}

/// Fetch and cache the text of one gold page. Not budgeted, not timed.
async fn gold_text(
    client: &reqwest::Client,
    config: &DisclosureConfig,
    document_id: &str,
    page: &GoldPage,
    state: &mut RunState,
    notes: &mut Vec<String>,
) -> Result<String, RunError> {
    if let Some(text) = state.gold_texts.get(&page.corpus_id) {
        return Ok(text.clone());
    }
    let page_0based = page.page_1based.saturating_sub(1);
    let pages = content_pages(
        client,
        config,
        document_id,
        &ReadSelector::Pages(page_0based, page_0based),
        CONTENT_HARD_MAX,
        None,
        notes,
    )
    .await?;
    let text: String = pages.into_iter().map(|page| page.text).collect();
    state
        .gold_texts
        .insert(page.corpus_id.clone(), text.clone());
    Ok(text)
}

/// Run the scripted reader for one question.
async fn score_question(
    client: &reqwest::Client,
    config: &DisclosureConfig,
    ingest_map: &HashMap<String, String>,
    query: &crate::dataset::Query,
    gold_pages: &[GoldPage],
    state: &mut RunState,
) -> QuestionResult {
    let mut errors: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut gold_texts: Vec<String> = Vec::new();
    let mut missing_doc = 0usize;
    let mut gold_chars = 0u64;
    // (document id, 0-based page) of every gold page we could resolve through
    // the ingest map. Used to tell a low-ranked gold hit from a reader miss.
    let gold_pages_set: HashSet<(String, u32)> = gold_pages
        .iter()
        .filter_map(|page| {
            let document = ingest_map.get(&page.doc_key)?;
            Some((document.clone(), page.page_1based.saturating_sub(1)))
        })
        .collect();

    // Gold first: the budget depends on it, and the reads are not the reader's.
    for page in gold_pages {
        match ingest_map.get(&page.doc_key) {
            None => {
                missing_doc += 1;
                gold_texts.push(String::new());
            }
            Some(document_id) => {
                match gold_text(client, config, document_id, page, state, &mut notes).await {
                    Ok(text) => {
                        gold_chars += text.chars().count() as u64;
                        gold_texts.push(text);
                    }
                    Err(err) => {
                        errors.push(format!("{}: gold read failed: {err}", page.corpus_id));
                        gold_texts.push(String::new());
                    }
                }
            }
        }
    }
    errors.append(&mut notes);

    let mut budget = budget_chars(gold_chars, config.budget_factor);
    let mut parts: Vec<String> = Vec::new();
    let mut chars_read = 0u64;
    let hits = match search_hits(client, config, &query.text, &mut state.search_ms).await {
        Ok(hits) => hits,
        Err(err) => {
            errors.push(format!("search failed: {err}"));
            Vec::new()
        }
    };
    let hit_records: Vec<HitRecord> = hits
        .iter()
        .enumerate()
        .map(|(index, (document, page))| HitRecord {
            rank: index + 1,
            document_id: document.clone(),
            page: *page,
            gold: page.is_some_and(|page| gold_pages_set.contains(&(document.clone(), page))),
        })
        .collect();
    let first_gold_rank = best_gold_rank(&hits, &gold_pages_set);
    let mut pages_read: Vec<PageReadRecord> = Vec::new();
    let mut ledger = ReadLedger::default();
    for (document_id, pages) in group_hits_by_document(&hits) {
        if budget == 0 {
            break;
        }
        let structure = if let Some(structure) = state.structures.get(&document_id) {
            structure.clone()
        } else {
            match fetch_structure(client, config, &document_id, &mut state.structure_ms).await {
                Ok(structure) => {
                    state
                        .structures
                        .insert(document_id.clone(), structure.clone());
                    structure
                }
                Err(err) => {
                    errors.push(format!("{document_id}: structure failed: {err}"));
                    continue;
                }
            }
        };
        for page in pages {
            if budget == 0 {
                break;
            }
            let Some(page_0based) = page else {
                continue;
            };
            let (selector, chunk_range) = match section_for_page(&structure.sections, page_0based) {
                Some(section) => (
                    ReadSelector::Section(section.id.clone()),
                    (section.chunk_start, section.chunk_end),
                ),
                None => {
                    let window = page_window(page_0based);
                    match structure.chunk_range_for_pages(window) {
                        Some(range) => (ReadSelector::Pages(window.0, window.1), range),
                        None => {
                            notes.push(format!(
                                "{document_id}: no chunks on pages {}-{}",
                                window.0, window.1
                            ));
                            continue;
                        }
                    }
                }
            };
            if !ledger.mark_selector(&document_id, &selector)
                || ledger.overlaps_read(&document_id, chunk_range)
            {
                continue;
            }
            let read = content_pages(
                client,
                config,
                &document_id,
                &selector,
                usize::try_from(budget)
                    .unwrap_or(usize::MAX)
                    .min(CONTENT_MAX_CHARS),
                Some(&mut state.content_ms),
                &mut notes,
            )
            .await;
            match read {
                Ok(read) => {
                    for page in read {
                        ledger.record_range(&document_id, page.chunk_range);
                        let kept = take_chars(&page.text, budget);
                        let used = kept.chars().count() as u64;
                        pages_read.push(PageReadRecord {
                            document_id: document_id.clone(),
                            selector: selector.label(),
                            chunk_start: page.chunk_range.0,
                            chunk_end: page.chunk_range.1,
                            page_start: page.page_range.0,
                            page_end: page.page_range.1,
                            chars: used,
                        });
                        chars_read += used;
                        budget = budget.saturating_sub(used);
                        parts.push(kept);
                        if budget == 0 {
                            break;
                        }
                    }
                }
                Err(err) => errors.push(format!("{document_id}: content failed: {err}")),
            }
        }
    }
    errors.extend(notes);

    let scored = coverage(&gold_texts, &parts);
    let answer_hit = answer_page_hit(&scored.flags);
    let covered_pages: Vec<String> = gold_pages
        .iter()
        .zip(scored.flags.iter())
        .filter(|&(_, &yes)| yes)
        .map(|(page, _)| page.corpus_id.clone())
        .collect();
    let missed_pages: Vec<String> = gold_pages
        .iter()
        .zip(scored.flags.iter())
        .filter(|&(_, &yes)| !yes)
        .map(|(page, _)| page.corpus_id.clone())
        .collect();
    QuestionResult {
        query_id: query.id.clone(),
        query: query.text.clone(),
        coverage: scored.fraction,
        covered: scored.covered,
        gold_pages: scored.total,
        missing_doc,
        gold_chars,
        chars_read,
        chars_ratio: if gold_chars == 0 {
            0.0
        } else {
            chars_read as f64 / gold_chars as f64
        },
        answer_page_hit: answer_hit,
        best_gold_rank: first_gold_rank,
        covered_pages,
        missed_pages,
        hits: hit_records,
        pages_read,
        errors,
    }
}

fn write_outputs(
    config: &DisclosureConfig,
    report: &DisclosureReport,
    questions: &[QuestionResult],
) -> Result<(), RunError> {
    fs::create_dir_all(&config.out_dir)?;
    fs::write(
        config.out_dir.join("results.json"),
        serde_json::to_string_pretty(report)?,
    )?;
    let mut jsonl = String::new();
    for question in questions {
        jsonl.push_str(&serde_json::to_string(question)?);
        jsonl.push('\n');
    }
    fs::write(config.out_dir.join("per_question.jsonl"), jsonl)?;
    fs::write(
        config.out_dir.join("report.md"),
        render_report(report, questions),
    )?;
    Ok(())
}

/// Run the whole harness and write `results.json`, `per_question.jsonl` and
/// `report.md` into `--out`.
///
/// # Errors
///
/// [`RunError::Usage`] for a missing dataset or ingest map; [`RunError::Http`]
/// cannot abort a question — endpoint failures are recorded in `errors` and
/// score as misses — but is returned for a connection that cannot be used at
/// all.
pub async fn run(config: DisclosureConfig) -> Result<DisclosureReport, RunError> {
    let files = dataset_files(&config)?;
    let queries = parse_queries(&fs::read_to_string(&files.queries)?).map_err(RunError::Usage)?;
    let qrels = parse_qrels(&fs::read_to_string(&files.qrels)?).map_err(RunError::Usage)?;
    let gold = gold_by_query(&qrels);
    let ingest_map = load_ingest_map(&config)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let mut state = RunState::default();
    let mut results = Vec::new();
    for query in queries.iter().take(config.limit.unwrap_or(queries.len())) {
        let empty: Vec<GoldPage> = Vec::new();
        let question_gold = gold.get(&query.id).unwrap_or(&empty);
        results.push(
            score_question(
                &client,
                &config,
                &ingest_map,
                query,
                question_gold,
                &mut state,
            )
            .await,
        );
    }
    let answer_values: Vec<f64> = results
        .iter()
        .map(|q| f64::from(q.answer_page_hit))
        .collect();
    let coverage_values: Vec<f64> = results.iter().map(|q| q.coverage).collect();
    let single_values: Vec<f64> = results
        .iter()
        .filter(|q| q.gold_pages == 1)
        .map(|q| q.coverage)
        .collect();
    let ratio_values: Vec<f64> = results.iter().map(|q| q.chars_ratio).collect();
    let answer_page_coverage = bootstrap_ci(&answer_values, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED);
    let coverage = bootstrap_ci(&coverage_values, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED);
    let single_gold_coverage = bootstrap_ci(&single_values, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED);
    let chars_ratio = bootstrap_ci(&ratio_values, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED);
    let best_gold_rank = rank_distribution(
        &results.iter().map(|q| q.best_gold_rank).collect::<Vec<_>>(),
        usize::try_from(config.top_k).unwrap_or(usize::MAX),
    );
    let latency_ms = LatencyByEndpoint {
        search: latency_report(state.search_ms),
        structure: latency_report(state.structure_ms),
        content: latency_report(state.content_ms),
    };
    let report = DisclosureReport {
        dataset: config.dataset.display().to_string(),
        top_k: config.top_k,
        budget_factor: config.budget_factor,
        bootstrap_resamples: BOOTSTRAP_RESAMPLES,
        bootstrap_seed: BOOTSTRAP_SEED,
        n_questions: results.len(),
        missing_doc: results.iter().map(|q| q.missing_doc).sum(),
        answer_page_coverage,
        coverage,
        single_gold_coverage,
        best_gold_rank,
        chars_ratio,
        chars_read_total: results.iter().map(|q| q.chars_read).sum(),
        gold_chars_total: results.iter().map(|q| q.gold_chars).sum(),
        targets: evaluate_targets(
            answer_page_coverage.mean,
            coverage.mean,
            chars_ratio.mean,
            latency_ms.content.p95,
        ),
        latency_ms,
    };
    write_outputs(&config, &report, &results)?;
    Ok(report)
}

// ─── command line ────────────────────────────────────────────────────────────

fn parse_config(args: &[String]) -> Result<DisclosureConfig, EvalError> {
    let mut flags: HashMap<&str, String> = HashMap::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let Some(name) = arg.strip_prefix("--") else {
            return Err(EvalError::Usage(format!("unexpected argument {arg:?}")));
        };
        let known = [
            "base-url",
            "key",
            "api-key",
            "dataset",
            "out",
            "top-k",
            "budget-factor",
            "limit",
            "ingest-map",
            "queries",
            "qrels",
        ];
        if !known.contains(&name) {
            return Err(EvalError::Usage(format!("unknown flag --{name}")));
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| EvalError::Usage(format!("--{name} needs a value")))?;
        flags.insert(name, value.clone());
        index += 2;
    }
    let base_url = flags
        .get("base-url")
        .cloned()
        .ok_or_else(|| EvalError::Usage("missing --base-url".to_string()))?;
    let dataset = flags
        .get("dataset")
        .cloned()
        .ok_or_else(|| EvalError::Usage("missing --dataset".to_string()))?;
    let api_key = flags
        .get("key")
        .or_else(|| flags.get("api-key"))
        .cloned()
        .or_else(|| std::env::var("KB_API_KEY").ok())
        .or_else(|| std::env::var("KB_EVAL_API_KEY").ok())
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| {
            EvalError::Usage("missing API key: pass --key or set KB_API_KEY".to_string())
        })?;
    let uint = |name: &str, default: u64| -> Result<u64, EvalError> {
        match flags.get(name) {
            None => Ok(default),
            Some(raw) => raw
                .parse::<u64>()
                .map_err(|_| EvalError::Usage(format!("--{name} must be a positive integer"))),
        }
    };
    let top_k = u32::try_from(uint("top-k", 5)?)
        .map_err(|_| EvalError::Usage("--top-k is too large".to_string()))?;
    if top_k == 0 {
        return Err(EvalError::Usage("--top-k must be at least 1".to_string()));
    }
    let budget_factor: f64 = match flags.get("budget-factor") {
        None => 1.5,
        Some(raw) => raw.parse().map_err(|_| {
            EvalError::Usage("--budget-factor must be a positive number".to_string())
        })?,
    };
    if !budget_factor.is_finite() || budget_factor <= 0.0 {
        return Err(EvalError::Usage(
            "--budget-factor must be a positive number".to_string(),
        ));
    }
    let limit = match flags.get("limit") {
        None => None,
        Some(raw) => {
            let value = raw
                .parse::<usize>()
                .map_err(|_| EvalError::Usage("--limit must be a positive integer".to_string()))?;
            if value == 0 {
                return Err(EvalError::Usage("--limit must be at least 1".to_string()));
            }
            Some(value)
        }
    };
    Ok(DisclosureConfig {
        base_url,
        api_key,
        dataset: PathBuf::from(dataset),
        out_dir: PathBuf::from(
            flags
                .get("out")
                .cloned()
                .unwrap_or_else(|| "eval-out/disclosure".to_string()),
        ),
        top_k,
        budget_factor,
        limit,
        ingest_map: flags.get("ingest-map").map(PathBuf::from),
        queries: flags.get("queries").map(PathBuf::from),
        qrels: flags.get("qrels").map(PathBuf::from),
    })
}

/// `kb-server eval disclosure …`.
///
/// # Errors
///
/// [`EvalError::Usage`] for bad flags or a missing dataset; [`EvalError::Failed`]
/// when the run cannot produce outputs.
pub async fn run_cli(args: &[String]) -> Result<(), EvalError> {
    let config = parse_config(args)?;
    let report = run(config)
        .await
        .map_err(|err| EvalError::Failed(err.to_string()))?;
    let verdict = |pass: bool| if pass { "PASS" } else { "MISS" };
    println!(
        "disclosure: {} questions, answer-page coverage {:.3} [{:.3}, {:.3}], all-pages coverage {:.3} [{:.3}, {:.3}], chars read/gold {:.3}, content p95 {:.1} ms",
        report.n_questions,
        report.answer_page_coverage.mean,
        report.answer_page_coverage.ci_low,
        report.answer_page_coverage.ci_high,
        report.coverage.mean,
        report.coverage.ci_low,
        report.coverage.ci_high,
        report.chars_ratio.mean,
        report.latency_ms.content.p95
    );
    println!(
        "targets: answer-page {} (primary) · chars {} · all-pages {} (secondary) · content p95 {}",
        verdict(report.targets.answer_page_coverage_pass),
        verdict(report.targets.chars_ratio_pass),
        verdict(report.targets.coverage_pass),
        verdict(report.targets.content_p95_pass)
    );
    println!("wrote results.json, per_question.jsonl, report.md");
    Ok(())
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    #[test]
    fn whitespace_normalisation_collapses_runs_and_trims() {
        assert_eq!(normalize_whitespace("  a\t b\n\n c  "), "a b c");
        assert_eq!(normalize_whitespace("\t\n"), "");
    }

    #[test]
    fn coverage_is_exact_substring_after_normalisation() {
        let gold = vec!["p1  text".to_string(), "page 2".to_string()];
        let read = vec!["p1 text more text".to_string(), "page 2".to_string()];
        let scored = coverage(&gold, &read);
        assert_eq!(scored.covered, 2);
        assert_eq!(scored.total, 2);
        assert!((scored.fraction - 1.0).abs() < 1e-12);
        assert_eq!(scored.flags, vec![true, true]);
    }

    #[test]
    fn a_partial_or_empty_gold_page_is_uncovered() {
        let scored = coverage(
            &[
                "alpha beta".to_string(),
                "".to_string(),
                "gamma".to_string(),
            ],
            &["alpha beta gamma".to_string()],
        );
        assert_eq!(scored.flags, vec![true, false, true]);
        assert!((scored.fraction - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(coverage(&[], &["text".to_string()]).fraction, 0.0);
    }

    #[test]
    fn an_extra_word_inside_the_gold_page_defeats_the_substring_match() {
        let scored = coverage(&["the cat sat".to_string()], &["the cats sat".to_string()]);
        assert!(!scored.flags[0]);
    }

    #[test]
    fn budget_is_bounded_and_visible_when_misconfigured() {
        assert_eq!(budget_chars(100, 1.5), 150);
        assert_eq!(budget_chars(7, 1.5), 10);
        assert_eq!(budget_chars(100, 0.0), 0);
        assert_eq!(budget_chars(100, f64::NAN), 0);
        assert_eq!(budget_chars(100, f64::INFINITY), 0);
    }

    #[test]
    fn budget_take_does_not_split_a_character() {
        assert_eq!(take_chars("é中x", 2), "é中");
        assert_eq!(take_chars("abc", 0), "");
        assert_eq!(take_chars("abc", u64::MAX), "abc");
    }

    #[test]
    fn hits_group_by_document_without_reordering() {
        let hits = vec![
            ("b".to_string(), Some(3)),
            ("a".to_string(), Some(1)),
            ("b".to_string(), Some(4)),
            ("a".to_string(), None),
        ];
        assert_eq!(
            group_hits_by_document(&hits),
            vec![
                ("b".to_string(), vec![Some(3), Some(4)]),
                ("a".to_string(), vec![Some(1), None]),
            ]
        );
    }

    #[test]
    fn section_selection_prefers_the_containing_section() {
        let sections = vec![
            SectionRef {
                id: "s1".to_string(),
                page_start: Some(0),
                page_end: Some(2),
                chunk_start: 0,
                chunk_end: 4,
            },
            SectionRef {
                id: "s2".to_string(),
                page_start: Some(3),
                page_end: Some(9),
                chunk_start: 5,
                chunk_end: 20,
            },
        ];
        assert_eq!(
            section_for_page(&sections, 0).map(|s| s.id.as_str()),
            Some("s1")
        );
        assert_eq!(
            section_for_page(&sections, 8).map(|s| s.id.as_str()),
            Some("s2")
        );
    }

    #[test]
    fn page_window_clamps_at_zero_and_keeps_both_neighbours() {
        assert_eq!(page_window(0), (0, 1));
        assert_eq!(page_window(7), (6, 8));
        assert_eq!(page_window(u32::MAX), (u32::MAX - 1, u32::MAX));
    }

    #[test]
    fn ledger_never_reads_the_same_chunk_range_twice() {
        let mut ledger = ReadLedger::default();
        let section = ReadSelector::Section("s1".to_string());
        assert!(ledger.mark_selector("doc-a", &section));
        assert!(!ledger.mark_selector("doc-a", &section));
        // The same section id and chunk range in another document is a new read.
        assert!(ledger.mark_selector("doc-b", &section));
        assert!(!ledger.overlaps_read("doc-a", (10, 20)));
        ledger.record_range("doc-a", (10, 20));
        assert!(ledger.overlaps_read("doc-a", (0, 11)));
        assert!(ledger.overlaps_read("doc-a", (20, 30)));
        assert!(!ledger.overlaps_read("doc-a", (21, 30)));
        assert!(!ledger.overlaps_read("doc-b", (10, 20)));
    }

    #[test]
    fn bootstrap_interval_has_2000_resamples_and_brackets_the_mean() {
        let values: Vec<f64> = (0..40).map(|i| i as f64 / 40.0).collect();
        let ci = bootstrap_ci(&values, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED);
        let again = bootstrap_ci(&values, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED);
        assert_eq!(BOOTSTRAP_RESAMPLES, 2000);
        assert_eq!(ci, again);
        assert!(ci.ci_low <= ci.mean && ci.mean <= ci.ci_high);
        assert!(ci.ci_low < ci.ci_high);
    }

    #[test]
    fn gold_by_query_keeps_only_positive_page_qrels() {
        let qrels = parse_qrels(
            "query-id\tcorpus-id\tscore\nq1\tdoc#p2\t1\nq1\tdoc#p3\t0\nq1\tdoc\t1\nq2\tdoc#p5\t2\n",
        )
        .unwrap();
        let gold = gold_by_query(&qrels);
        assert_eq!(gold["q1"].len(), 1);
        assert_eq!(gold["q1"][0].page_1based, 2);
        assert_eq!(gold["q1"][0].doc_key, "doc");
        assert_eq!(gold["q2"][0].corpus_id, "doc#p5");
    }

    #[test]
    fn targets_report_a_miss_instead_of_hiding_it() {
        assert_eq!(
            evaluate_targets(0.91, 0.89, 1.4, 49.0),
            Targets {
                answer_page_coverage_pass: true,
                coverage_pass: false,
                chars_ratio_pass: true,
                content_p95_pass: true
            }
        );
        let miss = evaluate_targets(0.79, 0.89, 1.6, 51.0);
        assert!(
            !miss.answer_page_coverage_pass
                && !miss.coverage_pass
                && !miss.chars_ratio_pass
                && !miss.content_p95_pass
        );
        let text = render_report(
            &DisclosureReport {
                dataset: "d".to_string(),
                top_k: 5,
                budget_factor: 1.5,
                bootstrap_resamples: BOOTSTRAP_RESAMPLES,
                bootstrap_seed: BOOTSTRAP_SEED,
                n_questions: 1,
                missing_doc: 0,
                answer_page_coverage: bootstrap_ci(&[0.79], BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED),
                coverage: bootstrap_ci(&[0.89], BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED),
                single_gold_coverage: bootstrap_ci(&[0.5], BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED),
                best_gold_rank: rank_distribution(&[None], 5),
                chars_ratio: bootstrap_ci(&[1.6], BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED),
                chars_read_total: 16,
                gold_chars_total: 10,
                latency_ms: LatencyByEndpoint {
                    search: latency_report(vec![1.0]),
                    structure: latency_report(vec![1.0]),
                    content: latency_report(vec![51.0]),
                },
                targets: miss,
            },
            &[],
        );
        assert!(text.contains("MISS"));
        assert!(text.contains("Target"));
        assert!(text.contains("answer-page coverage"));
        assert!(text.contains("all-gold-pages coverage"));
    }

    #[test]
    fn answer_page_hit_and_best_gold_rank_separate_search_from_reader() {
        assert_eq!(answer_page_hit(&[false, false]), 0);
        assert_eq!(answer_page_hit(&[false, true, false]), 1);

        let gold = HashSet::from([("doc-a".to_string(), 3u32), ("doc-b".to_string(), 0u32)]);
        let hits = vec![
            ("doc-x".to_string(), Some(0)),
            ("doc-a".to_string(), None),
            ("doc-a".to_string(), Some(3)),
            ("doc-b".to_string(), Some(0)),
        ];
        assert_eq!(best_gold_rank(&hits, &gold), Some(3));
        assert_eq!(best_gold_rank(&hits[..2], &gold), None);

        let distribution = rank_distribution(&[Some(1), Some(3), Some(3), None], 5);
        assert_eq!(
            distribution.counts,
            vec![(1, 1), (3, 2)],
            "rank histogram is sorted by rank"
        );
        assert_eq!(distribution.any, 3);
        assert_eq!(distribution.none, 1);
        assert!((distribution.mean - 7.0 / 3.0).abs() < 1e-12);
    }

    /// A three-page mock stack: two documents, labelled pages and an agent loop.
    #[derive(Debug)]
    struct MockState {
        search_calls: Mutex<usize>,
        structure_calls: Mutex<usize>,
        content_calls: Mutex<usize>,
    }

    async fn mock_search(
        State(state): State<Arc<MockState>>,
        Json(_body): Json<Value>,
    ) -> Json<Value> {
        *state.search_calls.lock().unwrap() += 1;
        Json(json!({"chunks": [
            {"documentId": "doc-a", "page": 0},
            {"documentId": "doc-b", "page": 0}
        ]}))
    }

    async fn mock_structure(
        State(state): State<Arc<MockState>>,
        axum::extract::Path(id): axum::extract::Path<String>,
    ) -> Json<Value> {
        *state.structure_calls.lock().unwrap() += 1;
        let sections = if id == "doc-a" {
            vec![json!({"id": "s1", "pageStart": 0, "pageEnd": 1, "chunkStart": 0, "chunkEnd": 1})]
        } else {
            vec![json!({"id": "s1", "pageStart": 0, "pageEnd": 0, "chunkStart": 0, "chunkEnd": 0})]
        };
        Json(json!({"documentId": id, "sections": sections, "chunks": []}))
    }

    async fn mock_content(
        State(state): State<Arc<MockState>>,
        axum::extract::Path(id): axum::extract::Path<String>,
        axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
    ) -> Json<Value> {
        *state.content_calls.lock().unwrap() += 1;
        let text = if let Some(section) = params.get("section") {
            if section != "s1" {
                return Json(json!({"error": "unknown section"}));
            }
            if id == "doc-a" {
                "A page one A page two"
            } else {
                "B page one"
            }
        } else {
            let page: u32 = params.get("pageStart").unwrap().parse().unwrap();
            match (id.as_str(), page) {
                ("doc-a", 0) => "A page one",
                ("doc-a", 1) => "A page two",
                ("doc-b", 0) => "B page one",
                _ => "",
            }
        };
        Json(json!({
            "documentId": id,
            "text": text,
            "range": {"chunkStart": 0, "chunkEnd": 0, "pageStart": 0, "pageEnd": 0},
            "truncated": false,
            "nextCursor": null,
            "refs": [],
            "degrades": []
        }))
    }

    #[tokio::test]
    async fn end_to_end_against_a_mock_http_server() {
        let dir = std::env::temp_dir().join(format!(
            "kb-disclosure-e2e-{}-{}",
            std::process::id(),
            BOOTSTRAP_SEED
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("qrels")).unwrap();
        fs::write(
            dir.join("queries.jsonl"),
            "{\"_id\":\"q1\",\"text\":\"what is on page one?\"}\n",
        )
        .unwrap();
        fs::write(
            dir.join("qrels/test.tsv"),
            "query-id\tcorpus-id\tscore\nq1\tdoc-a#p1\t1\nq1\tdoc-a#p2\t1\nq1\tdoc-b#p1\t1\n",
        )
        .unwrap();
        fs::write(
            dir.join("ingest-map.json"),
            json!({"doc-a": "doc-a", "doc-b": "doc-b"}).to_string(),
        )
        .unwrap();
        let out = dir.join("out");
        let state = Arc::new(MockState {
            search_calls: Mutex::new(0),
            structure_calls: Mutex::new(0),
            content_calls: Mutex::new(0),
        });
        let router = Router::new()
            .route("/v1/search", post(mock_search))
            .route("/v1/documents/:id/structure", get(mock_structure))
            .route("/v1/documents/:id/content", get(mock_content))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let report = run(DisclosureConfig {
            base_url: format!("http://{addr}"),
            api_key: "test-key".to_string(),
            dataset: dir.clone(),
            out_dir: out.clone(),
            top_k: 5,
            budget_factor: 1.5,
            limit: None,
            ingest_map: None,
            queries: None,
            qrels: None,
        })
        .await
        .unwrap();

        assert_eq!(report.n_questions, 1);
        assert_eq!(report.missing_doc, 0);
        assert!((report.coverage.mean - 1.0).abs() < 1e-12);
        assert!((report.answer_page_coverage.mean - 1.0).abs() < 1e-12);
        assert!((report.single_gold_coverage.mean).abs() < 1e-12); // no single-gold question
        assert_eq!(report.best_gold_rank.any, 1, "first hit is on a gold page");
        assert!((report.best_gold_rank.mean - 1.0).abs() < 1e-12);
        assert!(report.chars_ratio.mean <= 1.5);
        assert_eq!(*state.search_calls.lock().unwrap(), 1);
        assert_eq!(*state.structure_calls.lock().unwrap(), 2);
        // Three gold-page reads plus one read per hit document.
        assert_eq!(*state.content_calls.lock().unwrap(), 5);
        let results: Value =
            serde_json::from_str(&fs::read_to_string(out.join("results.json")).unwrap()).unwrap();
        assert_eq!(results["bootstrap_resamples"], 2000);
        assert_eq!(results["n_questions"], 1);
        let lines = fs::read_to_string(out.join("per_question.jsonl")).unwrap();
        assert_eq!(lines.lines().count(), 1);
        let line: Value = serde_json::from_str(lines.lines().next().unwrap()).unwrap();
        assert_eq!(line["answer_page_hit"], 1);
        assert_eq!(line["best_gold_rank"], 1);
        assert_eq!(line["hits"].as_array().unwrap().len(), 2);
        assert!(!line["pages_read"].as_array().unwrap().is_empty());
        let md = fs::read_to_string(out.join("report.md")).unwrap();
        assert!(md.contains("PASS") || md.contains("MISS"));
        server.abort();
        let _ = fs::remove_dir_all(&dir);
    }
}
