//! The server's per-stage timings, read back out of a search response.
//!
//! `GET /v1/search` answers with `stats.stageTimingsMs`: a partition of the
//! request (`scopeBuild`, `arms`, `fusion`, …, `total`) and a `detail` object
//! of measurements that overlap other stages (the query embed, the two arms,
//! the keyword arm's Postgres check) and so must never be summed into a total.
//!
//! This module flattens that object into `detail.queryEmbed` style names and
//! turns a run of them into percentiles per stage. Two things it deliberately
//! does *not* do:
//!
//! * It does not assume the server has stage timings. A server that reports
//!   none yields `None` and the report says so, rather than a table of zeros
//!   that reads like a very fast pipeline.
//! * It does not judge. Whether `arms` dominating is good news depends on
//!   whether `detail.queryEmbed` was a cache hit, so the cache counts travel
//!   with the numbers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::metrics::{LatencyReport, latency_report};

/// One response's stage timings, flattened.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StageSample {
    /// Milliseconds per stage. Names are the JSON paths: `arms`, `detail.queryEmbed`.
    pub stages: BTreeMap<String, f64>,
    /// `detail.queryEmbedCached` — whether the query embedding came from cache.
    pub query_embed_cached: Option<bool>,
}

/// The stage timings of a whole run, per stage, in percentiles.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StageLatencyReport {
    /// Answered queries whose response carried a `stageTimingsMs` object.
    pub reports: usize,
    /// Answered queries in the run, whether they reported timings or not.
    pub answers: usize,
    /// Answers that used a cached query embedding, and those that called the
    /// provider. Counted, not averaged: one number for the two states would be
    /// neither.
    pub query_embed_cache_hits: usize,
    pub query_embed_cache_misses: usize,
    /// Per stage: mean, p50, p95, p99 and how many responses had it.
    pub stages: BTreeMap<String, LatencyReport>,
    /// The server's own `total`, i.e. route entry to just before serialisation.
    pub server_total_ms: LatencyReport,
}

/// Read `stats.stageTimingsMs` out of a search response body.
///
/// Returns `None` when the response carries no timings — an older server, the
/// `documentIds` branch of an old build, or a body that is not JSON at all.
/// Not an error: the run continues and reports the gap.
#[must_use]
pub fn parse_stage_timings(body: &serde_json::Value) -> Option<StageSample> {
    // Accepts a whole response body or the `stats` object on its own, so a
    // caller that already has one of them does not have to rebuild the other.
    let stages = body
        .get("stats")
        .and_then(|stats| stats.get("stageTimingsMs"))
        .or_else(|| body.get("stageTimingsMs"))?;
    let mut sample = StageSample::default();
    flatten(stages, "", &mut sample);
    if sample.stages.is_empty() {
        return None;
    }
    Some(sample)
}

/// Walk an object of numbers, prefixing nested names with their path.
fn flatten(value: &serde_json::Value, prefix: &str, out: &mut StageSample) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (key, child) in object {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match child {
            serde_json::Value::Number(n) => {
                if let Some(ms) = n.as_f64() {
                    if path == "detail.queryEmbedCached" {
                        out.query_embed_cached = Some(ms != 0.0);
                    } else {
                        out.stages.insert(path, ms);
                    }
                }
            }
            serde_json::Value::Bool(b) => {
                if path == "detail.queryEmbedCached" {
                    out.query_embed_cached = Some(*b);
                }
            }
            serde_json::Value::Object(_) => flatten(child, &path, out),
            _ => {}
        }
    }
}

/// Collects per-stage samples across a run.
#[derive(Debug, Default)]
pub struct StageCollector {
    samples: BTreeMap<String, Vec<f64>>,
    totals: Vec<f64>,
    reports: usize,
    answers: usize,
    cache_hits: usize,
    cache_misses: usize,
}

impl StageCollector {
    /// Record one answered query's response.
    pub fn record(&mut self, body: &serde_json::Value) {
        self.answers += 1;
        let Some(sample) = parse_stage_timings(body) else {
            return;
        };
        self.reports += 1;
        match sample.query_embed_cached {
            Some(true) => self.cache_hits += 1,
            _ => self.cache_misses += 1,
        }
        for (stage, ms) in &sample.stages {
            self.samples.entry(stage.clone()).or_default().push(*ms);
        }
        if let Some(total) = sample.stages.get("total") {
            self.totals.push(*total);
        }
    }

    /// The percentiles, or `None` when no response reported timings.
    #[must_use]
    pub fn finish(self) -> Option<StageLatencyReport> {
        if self.reports == 0 {
            return None;
        }
        let stages = self
            .samples
            .into_iter()
            .map(|(stage, values)| (stage, latency_report(values)))
            .collect();
        Some(StageLatencyReport {
            reports: self.reports,
            answers: self.answers,
            query_embed_cache_hits: self.cache_hits,
            query_embed_cache_misses: self.cache_misses,
            stages,
            server_total_ms: latency_report(self.totals),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> serde_json::Value {
        json!({
            "chunks": [{"id": "c1"}],
            "stats": {
                "totalTimeMs": 2310.5,
                "embeddingTimeMs": 1900.0,
                "stageTimingsMs": {
                    "scopeBuild": 2.0,
                    "expand": 0.0,
                    "arms": 2100.0,
                    "enrich": 3.5,
                    "fusion": 0.6,
                    "rerank": 0.0,
                    "neighbours": 1.2,
                    "figures": 0.0,
                    "context": 0.8,
                    "serialise": 4.1,
                    "total": 2312.2,
                    "detail": {
                        "engineTotal": 2300.0,
                        "queryEmbed": 1980.0,
                        "queryEmbedCached": false,
                        "vectorArm": 2050.0,
                        "keywordArm": 40.0,
                        "entityArm": 0.0,
                        "scopeCheck": 12.0
                    }
                }
            }
        })
    }

    #[test]
    fn every_stage_is_flattened_with_its_path() {
        let sample = parse_stage_timings(&body()).expect("timings present");
        assert_eq!(sample.stages["arms"], 2100.0);
        assert_eq!(sample.stages["scopeBuild"], 2.0);
        assert_eq!(sample.stages["serialise"], 4.1);
        assert_eq!(sample.stages["detail.queryEmbed"], 1980.0);
        assert_eq!(sample.stages["detail.vectorArm"], 2050.0);
        assert_eq!(sample.stages["detail.scopeCheck"], 12.0);
        assert_eq!(sample.stages["total"], 2312.2);
    }

    #[test]
    fn the_cache_flag_is_a_flag_not_a_millisecond() {
        let sample = parse_stage_timings(&body()).expect("timings present");
        assert_eq!(sample.query_embed_cached, Some(false));
        assert!(
            !sample.stages.contains_key("detail.queryEmbedCached"),
            "a boolean is not a stage in milliseconds"
        );
    }

    #[test]
    fn the_stats_object_is_accepted_on_its_own() {
        let stats = body()["stats"].clone();
        let sample = parse_stage_timings(&stats).expect("timings present");
        assert_eq!(sample.stages["arms"], 2100.0);
    }

    #[test]
    fn a_response_without_timings_yields_nothing_rather_than_zeros() {
        assert!(parse_stage_timings(&json!({"chunks": []})).is_none());
        assert!(parse_stage_timings(&json!({"stats": {"totalTimeMs": 3}})).is_none());
        assert!(parse_stage_timings(&json!({"stats": {"stageTimingsMs": {}}})).is_none());
    }

    #[test]
    fn percentiles_are_taken_per_stage_across_the_run() {
        let mut collector = StageCollector::default();
        for arms in [2100.0, 100.0, 300.0] {
            let mut value = body();
            value["stats"]["stageTimingsMs"]["arms"] = json!(arms);
            collector.record(&value);
        }
        let report = collector.finish().expect("a report");
        assert_eq!(report.reports, 3);
        assert_eq!(report.answers, 3);
        assert_eq!(report.stages["arms"].n, 3);
        // mean of 2100, 100, 300
        assert!((report.stages["arms"].mean - 833.333).abs() < 0.01);
        // p50 of [100, 300, 2100], by the same rule as the rest of the harness.
        assert!((report.stages["arms"].p50 - 300.0).abs() < 0.01);
        assert!((report.stages["arms"].p95 - 1_920.0).abs() < 0.01);
    }

    #[test]
    fn the_server_total_is_reported_separately_from_the_client_latency() {
        let mut collector = StageCollector::default();
        collector.record(&body());
        let report = collector.finish().expect("a report");
        assert_eq!(report.server_total_ms.n, 1);
        assert!((report.server_total_ms.mean - 2312.2).abs() < 0.001);
        assert_eq!(report.query_embed_cache_hits, 0);
        assert_eq!(report.query_embed_cache_misses, 1);
    }

    #[test]
    fn answers_without_timings_are_counted_but_do_not_enter_the_percentiles() {
        let mut collector = StageCollector::default();
        collector.record(&body());
        collector.record(&json!({"chunks": []}));
        let report = collector.finish().expect("a report");
        assert_eq!(report.reports, 1);
        assert_eq!(report.answers, 2);
        assert_eq!(report.stages["arms"].n, 1);
    }

    #[test]
    fn a_run_where_nothing_reported_timings_has_no_report_at_all() {
        let mut collector = StageCollector::default();
        collector.record(&json!({"chunks": []}));
        assert!(collector.finish().is_none());
    }

    #[test]
    fn a_cache_hit_is_counted_as_a_hit() {
        let mut value = body();
        value["stats"]["stageTimingsMs"]["detail"]["queryEmbedCached"] = json!(true);
        let mut collector = StageCollector::default();
        collector.record(&value);
        let report = collector.finish().expect("a report");
        assert_eq!(report.query_embed_cache_hits, 1);
        assert_eq!(report.query_embed_cache_misses, 0);
    }
}
