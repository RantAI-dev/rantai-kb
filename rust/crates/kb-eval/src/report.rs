//! Run outputs: `metrics.json`, `report.md`, `run.trec`, and the offline
//! scorer.
//!
//! The TREC files are the important interoperability surface: `trec_eval` and
//! `ranx` can re-score them, which is how this harness is checked against an
//! outside tool. Because the metrics rank documents by the first chunk that
//! mentions them, the TREC score column encodes rank (`1 / rank`) rather than
//! the cosine similarity, so a re-scoring tool reproduces the same order.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::dataset::{Qrel, gold_for_documents, gold_for_pages};
use crate::metrics::{
    Gold, LatencyReport, LevelReport, QueryMetrics, aggregate_level, evaluate_query,
    unique_first_occurrence,
};

/// The top-level contents of `metrics.json`.
///
/// The counts are deliberately more than "how many documents did we find":
/// `n_queries` is what we asked, `n_failed` is what the server could not
/// answer, and `n_degraded` is what it answered while an arm was down. A run
/// that quietly scored a broken server as zero relevance would be a lie, so the
/// denominators are all published.
#[derive(Debug, Serialize)]
pub struct MetricsFile {
    pub label: String,
    pub dataset: String,
    pub timestamp: String,
    pub git_commit: String,
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
    pub document: LevelReport,
    pub page: LevelReport,
    pub latency_ms: LatencyReport,
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
    pub k: usize,
    pub max_chunks: u32,
    pub n_queries: usize,
    pub n_failed: usize,
    pub n_degraded: usize,
    pub n_noted: usize,
    pub n_documents: usize,
    pub n_ingest_failed: usize,
    pub degrades: Vec<String>,
    pub document: Vec<QueryMetrics>,
    pub page: Vec<QueryMetrics>,
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
        git_commit: git_commit(),
        k: parts.k,
        max_chunks: parts.max_chunks,
        n_queries: parts.n_queries,
        n_failed: parts.n_failed,
        n_degraded: parts.n_degraded,
        n_noted: parts.n_noted,
        n_documents: parts.n_documents,
        n_ingest_failed: parts.n_ingest_failed,
        degrades: parts.degrades,
        document: aggregate_level(&parts.document),
        page: aggregate_level(&parts.page),
        latency_ms: parts.latency,
    };
    RunReport {
        metrics,
        document_run: parts.document_run,
        page_run: parts.page_run,
    }
}

/// The short git commit of the working tree, or `unknown`.
#[must_use]
pub fn git_commit() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "unknown".to_string(),
    }
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
    out.push_str(&format!("- git commit: `{}`\n", m.git_commit));
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
    out.push_str("| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    out.push_str(&level_row("document", &m.document));
    out.push_str(&level_row("page", &m.page));
    out.push('\n');
    out.push_str(&format!(
        "Latency over {} successful queries (ms): mean {:.1}, p50 {:.1}, p95 {:.1}, p99 {:.1}\n",
        m.latency_ms.n, m.latency_ms.mean, m.latency_ms.p50, m.latency_ms.p95, m.latency_ms.p99,
    ));
    if m.n_failed > 0 {
        out.push_str(&format!(
            "\n> {} of {} queries failed and are excluded from the means above: a failed\n\
             > search is a failure, not a query that found nothing.\n",
            m.n_failed, m.n_queries
        ));
    }
    out.push_str(
        "\n`n` is the number of queries evaluable at that level: a query with no judged\n\
         document, or no judged page, is left out rather than scored as zero.\n",
    );
    out
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
pub fn score_offline(qrels: &[Qrel], rankings: &[TrecRanking], k: usize) -> String {
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
        if let Some(m) = evaluate_query(&doc_ranked, &doc_gold, k) {
            doc_metrics.push(m);
        }
        // A run file of document-level keys carries no pages, so the page arm
        // is empty here; scoring `run.page.trec` is what fills it.
        let page_ranked: Vec<String> = doc_ranked
            .into_iter()
            .filter(|key| crate::dataset::page_of(key).is_some())
            .collect();
        if let Some(m) = evaluate_query(&page_ranked, &page_gold, k) {
            page_metrics.push(m);
        }
    }
    let metrics = MetricsFile {
        label: "score".to_string(),
        dataset: "offline".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        git_commit: git_commit(),
        k,
        max_chunks: 0,
        n_queries,
        n_failed: 0,
        n_degraded: 0,
        n_noted: 0,
        n_documents: documents.len(),
        n_ingest_failed: 0,
        degrades: Vec::new(),
        document: aggregate_level(&doc_metrics),
        page: aggregate_level(&page_metrics),
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
        let report = score_offline(&qrels, &rankings, 10);
        assert!(report.contains("1.000"), "{report}");
    }

    #[test]
    fn n_queries_is_what_was_asked_not_what_was_scored() {
        let scored = evaluate_query(
            &["a".to_string()],
            &HashMap::from([("a".to_string(), 1)]),
            10,
        )
        .expect("evaluable");
        let report = build_report(ReportParts {
            label: "run",
            dataset: "tiny",
            k: 10,
            max_chunks: 50,
            n_queries: 5,
            n_failed: 2,
            n_degraded: 1,
            n_noted: 1,
            n_documents: 6,
            n_ingest_failed: 1,
            degrades: vec!["vector arm unavailable".to_string()],
            document: vec![scored],
            page: Vec::new(),
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
        // Five queries were attempted; one of them was evaluable.
        assert_eq!(report.metrics.n_queries, 5);
        assert_eq!(report.metrics.document.n_queries, 1);
        assert_eq!(report.metrics.n_failed, 2);

        let markdown = render_markdown(&report.metrics);
        assert!(
            markdown.contains("queries: 5 (failed: 2, degraded: 1, noted: 1)"),
            "{markdown}"
        );
        assert!(markdown.contains("vector arm unavailable"), "{markdown}");
        assert!(markdown.contains("corpus files that failed to ingest: 1"));
        assert!(markdown.contains("2 of 5 queries failed"), "{markdown}");

        let json = serde_json::to_string(&report.metrics).expect("json");
        assert!(json.contains("\"n_failed\":2"), "{json}");
        assert!(json.contains("\"n_ingest_failed\":1"), "{json}");
        assert!(json.contains("\"git_commit\""), "{json}");
    }
}
