//! `kb-server eval calibrate-confidence` — fit RET-18 bands over HTTP.
//!
//! This crate is a black-box client: it links no engine, core or store, so it
//! reads the score and score kind from the server's additive `confidence`
//! object instead of recomputing them locally. It splits both query sets into
//! a fit half and a seeded hold-out half, fits `low`/`medium`/`high`, and then
//! reports the hold-out error rates against the RET-18 targets (≥ 90% of
//! off-topic queries band `none`, ≤ 5% of answerable ones band `none`).
//!
//! The result is a `calibration.json` in the same array format
//! `KB_CONFIDENCE_CALIBRATION` accepts, plus a human-readable `report.md` and
//! the per-query evidence in `scores.jsonl`. A fit is evidence, not a default:
//! nothing here changes a running server.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::EvalError;
use crate::dataset::parse_queries;

/// The off-topic rejection target (`RET-18`).
pub const OFFTOPIC_TARGET: f64 = 0.90;
/// The maximum answerable `none` rate (`RET-18`).
pub const ANSWERABLE_TARGET: f64 = 0.05;

/// Arguments of one calibration run.
#[derive(Debug, Clone)]
pub struct CalibrateConfig {
    pub base_url: String,
    pub api_key: String,
    pub answerable: PathBuf,
    pub offtopic: PathBuf,
    pub out_dir: PathBuf,
    pub holdout: f64,
    pub seed: u64,
    /// Explicit model id. When absent the tool resolves `KB_RERANK_MODEL`
    /// (rerank answers) or `KB_EMBEDDING_MODEL` (dense/fused) from its own
    /// environment, so the same deployment env file drives server and tool.
    pub model_id: Option<String>,
}

/// One row of `scores.jsonl`: what the server actually answered for a query.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryScore {
    pub id: String,
    pub set: String,
    pub split: String,
    pub top_score: Option<f32>,
    pub score_kind: String,
    pub model_id: String,
    pub band: String,
}

/// The fitted row, in the exact shape `KB_CONFIDENCE_CALIBRATION` reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationEntry {
    pub model_id: String,
    pub score_kind: String,
    pub high: f32,
    pub medium: f32,
    pub low: f32,
}

/// Fit the `low` threshold: the smallest observed off-topic top that leaves at
/// least `target` of the off-topic tops strictly below it (the 90th-percentile
/// off-topic score). Choosing the *smallest* qualifying threshold matters: a
/// larger one would reject more off-topic traffic but also more answerable
/// questions, and RET-18's other target is to keep answerable `none` ≤ 5%.
///
/// Candidates are observed off-topic tops, so the fit needs no tuning grid.
/// Returns `None` when the set is empty or no score clears the target.
#[must_use]
pub fn fit_low(offtopic: &[f32], target: f64) -> Option<f32> {
    let mut sorted = offtopic.to_vec();
    sorted.sort_by(f32::total_cmp);
    if sorted.is_empty() {
        return None;
    }
    // ceil(target * n)-1 is the last index whose value can have at least
    // `target` of the set strictly below it. Clamp for n=1 and target=1.
    let index = ((target * sorted.len() as f64).ceil() as usize).min(sorted.len() - 1);
    let threshold = sorted[index];
    let below = sorted.iter().filter(|&&score| score < threshold).count();
    let rate = below as f64 / sorted.len() as f64;
    (rate >= target).then_some(threshold)
}

/// Fit the `high` threshold: the smallest answerable top at which precision
/// (answerable tops ≥ t among all tops ≥ t) reaches `target`. A small `high`
/// keeps the top band honest without pushing borderline answers into it.
#[must_use]
pub fn fit_high(answerable: &[f32], offtopic: &[f32], target: f64) -> Option<f32> {
    let mut candidates = answerable.to_vec();
    candidates.sort_by(f32::total_cmp);
    candidates.dedup();
    for threshold in candidates {
        let positives = answerable
            .iter()
            .filter(|&&score| score >= threshold)
            .count();
        let negatives = offtopic.iter().filter(|&&score| score >= threshold).count();
        if positives == 0 {
            continue;
        }
        let precision = positives as f64 / (positives + negatives) as f64;
        if precision >= target {
            return Some(threshold);
        }
    }
    None
}

/// The band for a fitted entry, mirroring `kb-core::confidence` (which this
/// black-box crate is forbidden from linking).
#[must_use]
pub fn band_for(top: Option<f32>, entry: &CalibrationEntry) -> &'static str {
    match top {
        Some(score) if score >= entry.high => "high",
        Some(score) if score >= entry.medium => "medium",
        Some(score) if score >= entry.low => "low",
        _ => "none",
    }
}

/// A deterministic fit/hold-out assignment: shuffle the indices with a seeded
/// xorshift, then take the first `holdout` fraction as the hold-out half.
#[must_use]
pub fn holdout_mask(n: usize, holdout: f64, seed: u64) -> Vec<bool> {
    let mut order: Vec<usize> = (0..n).collect();
    // Mix the seed before forcing it odd: `seed | 1` would make 42 and 43
    // the same stream, so neighbouring runs would silently share a split.
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for i in (1..n).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
    let count = ((n as f64) * holdout).round() as usize;
    let mut mask = vec![false; n];
    for index in order.into_iter().take(count) {
        mask[index] = true;
    }
    mask
}

/// Parse and run `kb-server eval calibrate-confidence …`.
///
/// # Errors
///
/// [`EvalError::Usage`] for a missing flag or malformed number;
/// [`EvalError::Failed`] for an unreadable query file, an inconsistent server
/// answer, or a failed HTTP request.
pub async fn run(args: &[String]) -> Result<(), EvalError> {
    let config = parse_args(args)?;
    let answerable = read_queries("answerable", &config.answerable)?;
    let offtopic = read_queries("off-topic", &config.offtopic)?;
    if answerable.is_empty() || offtopic.is_empty() {
        return Err(EvalError::Failed(
            "both query files must contain at least one query".to_string(),
        ));
    }

    let client = reqwest::Client::new();
    let mut scores = Vec::with_capacity(answerable.len() + offtopic.len());
    for (set, queries) in [
        ("answerable", answerable.as_slice()),
        ("offtopic", offtopic.as_slice()),
    ] {
        let mask = holdout_mask(queries.len(), config.holdout, config.seed);
        for (query, is_holdout) in queries.iter().zip(mask) {
            scores.push(query_score(&client, &config, set, query, is_holdout).await?);
        }
    }

    let kinds: Vec<&str> = scores
        .iter()
        .map(|score| score.score_kind.as_str())
        .collect();
    let score_kind = kinds
        .first()
        .copied()
        .ok_or_else(|| EvalError::Failed("no query responses recorded".to_string()))?;
    if kinds.iter().any(|kind| *kind != score_kind) {
        return Err(EvalError::Failed(
            "the answers used more than one score kind; calibrate one scoring mode at a time \
             (set KB_RERANK_ENABLED consistently across the run)"
                .to_string(),
        ));
    }
    let model_ids: Vec<&str> = scores.iter().map(|score| score.model_id.as_str()).collect();
    let model_id = model_ids
        .first()
        .copied()
        .ok_or_else(|| EvalError::Failed("no model id recorded".to_string()))?;
    if model_ids.iter().any(|id| *id != model_id) {
        return Err(EvalError::Failed(
            "the answers named more than one model id; pass one --model-id".to_string(),
        ));
    }

    let fit_off: Vec<f32> = scores
        .iter()
        .filter(|score| score.set == "offtopic" && score.split == "fit")
        .filter_map(|score| score.top_score)
        .collect();
    let fit_answerable: Vec<f32> = scores
        .iter()
        .filter(|score| score.set == "answerable" && score.split == "fit")
        .filter_map(|score| score.top_score)
        .collect();
    let low = fit_low(&fit_off, OFFTOPIC_TARGET).ok_or_else(|| {
        EvalError::Failed(
            "no low threshold reached the off-topic target on the fit half; widen the off-topic \
             set or improve the score instead of raising the floor"
                .to_string(),
        )
    })?;
    let high = fit_high(&fit_answerable, &fit_off, OFFTOPIC_TARGET).ok_or_else(|| {
        EvalError::Failed(
            "no high threshold reached the answerable precision target on the fit half".to_string(),
        )
    })?;
    let entry = CalibrationEntry {
        model_id: model_id.to_string(),
        score_kind: score_kind.to_string(),
        high,
        medium: (low + high) / 2.0,
        low,
    };

    let mut off_holdout = 0usize;
    let mut off_none = 0usize;
    let mut answerable_holdout = 0usize;
    let mut answerable_none = 0usize;
    for score in &scores {
        if score.split != "holdout" {
            continue;
        }
        let band = band_for(score.top_score, &entry);
        if score.set == "offtopic" {
            off_holdout += 1;
            if band == "none" {
                off_none += 1;
            }
        } else {
            answerable_holdout += 1;
            if band == "none" {
                answerable_none += 1;
            }
        }
    }
    let off_rate = ratio(off_none, off_holdout);
    let answerable_rate = ratio(answerable_none, answerable_holdout);

    write_outputs(&config, &scores, &entry, off_rate, answerable_rate)?;

    println!(
        "calibrate-confidence: {score_kind} / {model_id}: low {low:.4}, medium {:.4}, high {high:.4}",
        entry.medium
    );
    println!(
        "hold-out: off-topic none {:.1}% ({} / {}, target ≥ 90%), answerable none {:.1}% \
         ({} / {}, target ≤ 5%)",
        off_rate * 100.0,
        off_none,
        off_holdout,
        answerable_rate * 100.0,
        answerable_none,
        answerable_holdout
    );
    println!(
        "wrote calibration.json, report.md and scores.jsonl to {}",
        config.out_dir.display()
    );
    Ok(())
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn read_queries(label: &str, path: &Path) -> Result<Vec<(String, String)>, EvalError> {
    let text = std::fs::read_to_string(path).map_err(|err| {
        EvalError::Failed(format!("read {label} queries {}: {err}", path.display()))
    })?;
    let queries = parse_queries(&text)
        .map_err(|err| EvalError::Failed(format!("{}: {err}", path.display())))?;
    Ok(queries
        .into_iter()
        .map(|query| (query.id, query.text))
        .collect())
}

async fn query_score(
    client: &reqwest::Client,
    config: &CalibrateConfig,
    set: &str,
    query: &(String, String),
    is_holdout: bool,
) -> Result<QueryScore, EvalError> {
    let url = format!("{}/v1/search", config.base_url.trim_end_matches('/'));
    let response = client
        .post(&url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", config.api_key),
        )
        .json(&serde_json::json!({"query": query.1.as_str(), "maxChunks": 10}))
        .send()
        .await
        .map_err(|err| EvalError::Failed(format!("{}: POST /v1/search: {err}", query.0)))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|err| EvalError::Failed(format!("{}: search body: {err}", query.0)))?;
    if !status.is_success() {
        return Err(EvalError::Failed(format!(
            "{}: /v1/search answered {status}: {}",
            query.0,
            body.get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error")
        )));
    }
    // A failed or degraded search is not an empty result: calibrating on one
    // would make a band look better than it is.
    let degrades = body.get("degrades").and_then(serde_json::Value::as_array);
    if degrades.is_some_and(|values| !values.is_empty()) {
        return Err(EvalError::Failed(format!(
            "{}: degraded search; refusing to calibrate on it",
            query.0
        )));
    }
    let confidence = body
        .get("confidence")
        .and_then(|value| value.as_object())
        .ok_or_else(|| {
            EvalError::Failed(format!(
                "{}: no confidence object; the server must be built with RET-18",
                query.0
            ))
        })?;
    let top_score = confidence
        .get("topScore")
        .and_then(serde_json::Value::as_f64)
        .map(|value| value as f32);
    let score_kind = confidence
        .get("scoreKind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let model_id = match (config.model_id.as_deref(), score_kind.as_str()) {
        (Some(id), _) => id.to_string(),
        (None, "rerank") => env_model("KB_RERANK_MODEL", &query.0)?,
        (None, _) => env_model("KB_EMBEDDING_MODEL", &query.0)?,
    };
    Ok(QueryScore {
        id: query.0.clone(),
        set: set.to_string(),
        split: if is_holdout { "holdout" } else { "fit" }.to_string(),
        top_score,
        score_kind,
        model_id,
        band: confidence
            .get("band")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    })
}

fn env_model(variable: &str, query_id: &str) -> Result<String, EvalError> {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            EvalError::Failed(format!(
                "{query_id}: set --model-id or {variable} so the fit can name its model"
            ))
        })
}

fn write_outputs(
    config: &CalibrateConfig,
    scores: &[QueryScore],
    entry: &CalibrationEntry,
    off_rate: f64,
    answerable_rate: f64,
) -> Result<(), EvalError> {
    std::fs::create_dir_all(&config.out_dir)
        .map_err(|err| EvalError::Failed(format!("create {}: {err}", config.out_dir.display())))?;
    let mut lines = String::new();
    for score in scores {
        let line = serde_json::to_string(score)
            .map_err(|err| EvalError::Failed(format!("serialise score: {err}")))?;
        lines.push_str(&line);
        lines.push('\n');
    }
    let scores_path = config.out_dir.join("scores.jsonl");
    std::fs::write(&scores_path, lines)
        .map_err(|err| EvalError::Failed(format!("write {}: {err}", scores_path.display())))?;
    let calibration_path = config.out_dir.join("calibration.json");
    let calibration = serde_json::to_string_pretty(&vec![entry.clone()])
        .map_err(|err| EvalError::Failed(format!("serialise calibration: {err}")))?;
    std::fs::write(&calibration_path, format!("{calibration}\n"))
        .map_err(|err| EvalError::Failed(format!("write {}: {err}", calibration_path.display())))?;

    let off_target = if off_rate >= OFFTOPIC_TARGET {
        "met"
    } else {
        "MISSED"
    };
    let answerable_target = if answerable_rate <= ANSWERABLE_TARGET {
        "met"
    } else {
        "MISSED"
    };
    let report = format!(
        "# Confidence calibration — {} / {}\n\n\
         Fitted from `/v1/search` responses on one server build. `low` is the smallest observed\n\
         off-topic top that leaves ≥ 90% of the fit half strictly below it (the 90th-percentile\n\
         off-topic score); `high` is the smallest answerable top with ≥ 90% precision among all\n\
         tops at or above it; `medium` is their midpoint.\n\n\
         | Threshold | Value |\n|---|---:|\n\
         | `low` | {:.6} |\n| `medium` | {:.6} |\n| `high` | {:.6} |\n\n\
         Hold-out ({:.0}%, seed {}):\n\n\
         | Set | Queries | band=`none` | Rate | Target | Result |\n|---|---:|---:|---:|---|---|\n\
         | off-topic | {} | {} | {:.1}% | ≥ 90% | {} |\n\
         | answerable | {} | {} | {:.1}% | ≤ 5% | {} |\n\n\
         If a target is MISSED, do not ship this table: widen the query sets or change the score.\n\
         Install an accepted table with `KB_CONFIDENCE_CALIBRATION=<out>/calibration.json`.\n\n\
         Per-query evidence: `scores.jsonl`. Fit inputs: {} answerable, {} off-topic tops.\n",
        entry.score_kind,
        entry.model_id,
        entry.low,
        entry.medium,
        entry.high,
        config.holdout * 100.0,
        config.seed,
        scores
            .iter()
            .filter(|s| s.set == "offtopic" && s.split == "holdout")
            .count(),
        (off_rate
            * scores
                .iter()
                .filter(|s| s.set == "offtopic" && s.split == "holdout")
                .count() as f64)
            .round() as usize,
        off_rate * 100.0,
        off_target,
        scores
            .iter()
            .filter(|s| s.set == "answerable" && s.split == "holdout")
            .count(),
        (answerable_rate
            * scores
                .iter()
                .filter(|s| s.set == "answerable" && s.split == "holdout")
                .count() as f64)
            .round() as usize,
        answerable_rate * 100.0,
        answerable_target,
        scores
            .iter()
            .filter(|s| s.set == "answerable" && s.split == "fit")
            .count(),
        scores
            .iter()
            .filter(|s| s.set == "offtopic" && s.split == "fit")
            .count(),
    );
    let report_path = config.out_dir.join("report.md");
    std::fs::write(&report_path, report)
        .map_err(|err| EvalError::Failed(format!("write {}: {err}", report_path.display())))?;
    Ok(())
}

fn parse_args(args: &[String]) -> Result<CalibrateConfig, EvalError> {
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let required = |name: &str| -> Result<String, EvalError> {
        flag(name)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| EvalError::Usage(format!("calibrate-confidence: missing {name}")))
    };
    let base_url = required("--base-url")?;
    let api_key = flag("--api-key")
        .or_else(|| std::env::var("KB_EVAL_API_KEY").ok())
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| {
            EvalError::Usage(
                "calibrate-confidence: pass --api-key or set KB_EVAL_API_KEY".to_string(),
            )
        })?;
    let holdout = match flag("--holdout") {
        None => 0.5,
        Some(raw) => raw.parse::<f64>().map_err(|_| {
            EvalError::Usage(format!(
                "calibrate-confidence: --holdout must be a number, got {raw:?}"
            ))
        })?,
    };
    if !(0.0..=1.0).contains(&holdout) {
        return Err(EvalError::Usage(
            "calibrate-confidence: --holdout must be between 0 and 1".to_string(),
        ));
    }
    let seed = match flag("--seed") {
        None => 42,
        Some(raw) => raw.parse::<u64>().map_err(|_| {
            EvalError::Usage(format!(
                "calibrate-confidence: --seed must be an integer, got {raw:?}"
            ))
        })?,
    };
    Ok(CalibrateConfig {
        base_url,
        api_key,
        answerable: PathBuf::from(required("--answerable")?),
        offtopic: PathBuf::from(required("--offtopic")?),
        out_dir: PathBuf::from(flag("--out").unwrap_or_else(|| "eval-out/confidence".to_string())),
        holdout,
        seed,
        model_id: flag("--model-id").filter(|id| !id.trim().is_empty()),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn fit_low_takes_the_90th_percentile_off_topic_top() {
        let off: Vec<f32> = (1..=20).map(|i| i as f32 / 20.0).collect();
        // ceil(0.9 * 20) = 18 -> the 19th score (0-based) has 18/20 below it.
        let low = fit_low(&off, 0.90).expect("a fit exists");
        assert!((low - 0.95).abs() < 1e-6, "{low}");
        assert_eq!(fit_low(&[], 0.90), None);
        assert_eq!(fit_low(&[0.4], 0.90), None);
    }

    #[test]
    fn fit_high_is_the_smallest_threshold_with_90_percent_precision() {
        let answerable = [0.80, 0.85, 0.90, 0.95];
        let off = [0.10, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90];
        let high = fit_high(&answerable, &off, 0.90).expect("a fit exists");
        assert!((high - 0.95).abs() < 1e-6, "{high}");
        assert_eq!(fit_high(&[0.9], &[0.95], 0.90), None);
    }

    #[test]
    fn medium_is_the_midpoint_and_bands_follow_the_fit() {
        let entry = CalibrationEntry {
            model_id: "m".to_string(),
            score_kind: "rerank".to_string(),
            high: 0.86,
            medium: 0.64,
            low: 0.42,
        };
        assert_eq!(band_for(Some(0.9), &entry), "high");
        assert_eq!(band_for(Some(0.7), &entry), "medium");
        assert_eq!(band_for(Some(0.5), &entry), "low");
        assert_eq!(band_for(Some(0.1), &entry), "none");
        assert_eq!(band_for(None, &entry), "none");
    }

    #[test]
    fn holdout_split_is_deterministic_covers_every_query_and_respects_the_fraction() {
        let first = holdout_mask(20, 0.5, 42);
        assert_eq!(first, holdout_mask(20, 0.5, 42));
        assert_eq!(first.iter().filter(|&&flag| flag).count(), 10);
        assert_ne!(first, holdout_mask(20, 0.5, 43));
        assert_eq!(holdout_mask(0, 0.5, 42).len(), 0);
    }
}
