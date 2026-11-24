//! Calibrated answer confidence (RET-18).
//!
//! A raw retrieval score is meaningless until it is tied to the model that
//! produced it: a reranker probability, a dense cosine and a fused RRF sum are
//! three different scales. This module is the pure half of the feature: it
//! turns one comparable score into a band a calling app can act on, using a
//! table fitted per scoring model. It does no I/O — the operator's override
//! file is read by `kb-server` at startup and handed here as already-parsed
//! data.
//!
//! The caller is expected to answer when the band is `high` or `medium` and
//! say "I don't know" otherwise. When `calibrated` is false, the band is a
//! hint only: see [`confidence`].

use serde::{Deserialize, Serialize};

/// Which comparable score an answer's top hit carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScoreKind {
    /// Cross-encoder reranker score, comparable across queries after fitting.
    Rerank,
    /// Dense-arm cosine similarity.
    Dense,
    /// Fused (rank-only) score: a last resort, never calibrated here.
    #[default]
    Fused,
}

impl ScoreKind {
    /// The wire spelling, matching `confidence.scoreKind`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rerank => "rerank",
            Self::Dense => "dense",
            Self::Fused => "fused",
        }
    }
}

/// The band a caller can act on. `none` means "say I don't know"; `unknown`
/// means "there is no configured evidence either way".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    High,
    Medium,
    Low,
    None,
    Unknown,
}

impl Band {
    /// The wire spelling, matching `confidence.band`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }
}

/// One fitted row of the calibration table, keyed by model id and score kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationEntry {
    /// The active reranker model id when `score_kind` is `rerank`, else the
    /// embedding model id. Matched exactly; no prefix magic.
    pub model_id: String,
    pub score_kind: ScoreKind,
    /// Top score at or above this → `high`.
    pub high: f32,
    /// Top score at or above this → `medium`.
    pub medium: f32,
    /// Top score at or above this → `low`; below → `none`.
    pub low: f32,
}

/// The table in force for one request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalibrationTable {
    pub entries: Vec<CalibrationEntry>,
    /// The effective relevance floor (`minScore`, request or server default).
    /// It is the fallback band for a model with no fitted entry.
    pub min_score: Option<f32>,
}

/// Built-in entries.
///
/// Intentionally **empty**. The only committed number for a scoring model in
/// this repository is the RET-2 floor `0.42` for `voyageai/rerank-2.5` in
/// `docs/eval-ab/retrieval-gaps/README.md` (and its sweep in
/// `docs/eval-ab/retrieval-gaps/floor-calibration.txt`). That is a single
/// off-topic rejection threshold, not the three fitted points `high`,
/// `medium` and `low` need, so shipping it as a calibration would be inventing
/// two of the three numbers. Run `kb-server eval calibrate-confidence` on the
/// GPU box and install its `calibration.json` through
/// `KB_CONFIDENCE_CALIBRATION` instead.
pub const CALIBRATIONS: &[CalibrationEntry] = &[];

impl CalibrationTable {
    /// The shipped table, before an operator override and before a request's
    /// `minScore` is attached.
    #[must_use]
    pub fn built_in() -> Self {
        Self {
            entries: CALIBRATIONS.to_vec(),
            min_score: None,
        }
    }

    /// Parse `KB_CONFIDENCE_CALIBRATION`: a JSON array of entries in the same
    /// format `kb-server eval calibrate-confidence` writes.
    ///
    /// A malformed file, a non-finite threshold or an inverted band order is
    /// rejected here so the server refuses to start rather than report bands
    /// fitted on nonsense.
    ///
    /// # Errors
    ///
    /// A message naming the offending entry or JSON error.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let entries: Vec<CalibrationEntry> =
            serde_json::from_str(text).map_err(|err| format!("invalid calibration JSON: {err}"))?;
        for (index, entry) in entries.iter().enumerate() {
            let where_ = format!(
                "entry {index} ({} / {})",
                entry.model_id,
                entry.score_kind.as_str()
            );
            if entry.model_id.trim().is_empty() {
                return Err(format!("{where_}: modelId must not be empty"));
            }
            if !entry.high.is_finite() || !entry.medium.is_finite() || !entry.low.is_finite() {
                return Err(format!("{where_}: thresholds must be finite numbers"));
            }
            if entry.high < entry.medium || entry.medium < entry.low {
                return Err(format!(
                    "{where_}: thresholds must be ordered high >= medium >= low"
                ));
            }
        }
        Ok(Self {
            entries,
            min_score: None,
        })
    }

    /// Attach the effective relevance floor used for the uncalibrated fallback.
    #[must_use]
    pub fn with_min_score(mut self, min_score: Option<f32>) -> Self {
        self.min_score = min_score;
        self
    }

    /// The entry for exactly this model and score kind, if one was fitted.
    #[must_use]
    pub fn find(&self, model_id: &str, score_kind: ScoreKind) -> Option<&CalibrationEntry> {
        self.entries
            .iter()
            .find(|entry| entry.model_id == model_id && entry.score_kind == score_kind)
    }
}

/// The two top comparable scores of one answer, reduced to what a band needs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScoreEvidence {
    pub top: Option<f32>,
    pub second: Option<f32>,
    pub score_kind: ScoreKind,
}

impl ScoreEvidence {
    /// Take the highest and second-highest of `scores`, in that order. A
    /// single score leaves `second` as `None` (serialised as `null`), because
    /// a margin against a hit that does not exist would be a made-up number.
    #[must_use]
    pub fn from_scores<I>(scores: I, score_kind: ScoreKind) -> Self
    where
        I: IntoIterator<Item = f32>,
    {
        let mut sorted: Vec<f32> = scores.into_iter().collect();
        sorted.sort_by(|a, b| b.total_cmp(a));
        Self {
            top: sorted.first().copied(),
            second: sorted.get(1).copied(),
            score_kind,
        }
    }
}

/// The confidence object returned on a standard `/v1/search` answer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Confidence {
    pub top_score: Option<f32>,
    pub score_kind: ScoreKind,
    /// `top - second`, or `None` when fewer than two hits were scored.
    pub margin: Option<f32>,
    pub band: Band,
    /// `true` only when the active model has a fitted entry.
    pub calibrated: bool,
}

/// Reduce one answer to an actionable band.
///
/// With a fitted entry, the band is `high` at `high`, `medium` at `medium`,
/// `low` at `low`, else `none`. No hits is `none` even when calibrated: the
/// table says how to read a score, not how to invent one.
///
/// Without an entry, `calibrated` is false. If a `minScore` is configured the
/// band falls back to `low` at or above it and `none` below it; with no floor
/// at all it is `unknown`. That is a hint, not a promise: an unfitted score's
/// absolute value carries no cross-query meaning.
#[must_use]
pub fn confidence(
    top: Option<f32>,
    second: Option<f32>,
    score_kind: ScoreKind,
    model_id: &str,
    table: &CalibrationTable,
) -> Confidence {
    let margin = match (top, second) {
        (Some(top), Some(second)) => Some(top - second),
        _ => None,
    };
    let band = match table.find(model_id, score_kind) {
        Some(entry) => match top {
            Some(score) if score >= entry.high => Band::High,
            Some(score) if score >= entry.medium => Band::Medium,
            Some(score) if score >= entry.low => Band::Low,
            _ => Band::None,
        },
        None => match (top, table.min_score) {
            (None, _) => Band::None,
            (Some(_), None) => Band::Unknown,
            (Some(score), Some(floor)) if score >= floor => Band::Low,
            (Some(_), Some(_)) => Band::None,
        },
    };
    Confidence {
        top_score: top,
        score_kind,
        margin,
        band,
        calibrated: table.find(model_id, score_kind).is_some(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn table(high: f32, medium: f32, low: f32) -> CalibrationTable {
        CalibrationTable {
            entries: vec![CalibrationEntry {
                model_id: "rerank-2.5".to_string(),
                score_kind: ScoreKind::Rerank,
                high,
                medium,
                low,
            }],
            min_score: Some(0.2),
        }
    }

    #[test]
    fn calibrated_bands_use_all_four_thresholds() {
        let t = table(0.8, 0.6, 0.4);
        for (score, band) in [
            (0.95, Band::High),
            (0.8, Band::High),
            (0.79, Band::Medium),
            (0.6, Band::Medium),
            (0.59, Band::Low),
            (0.4, Band::Low),
            (0.39, Band::None),
        ] {
            let got = confidence(Some(score), None, ScoreKind::Rerank, "rerank-2.5", &t);
            assert_eq!(got.band, band, "score {score}");
            assert!(got.calibrated, "score {score}");
        }
    }

    #[test]
    fn no_hits_are_none_even_when_calibrated() {
        let got = confidence(
            None,
            None,
            ScoreKind::Rerank,
            "rerank-2.5",
            &table(0.8, 0.6, 0.4),
        );
        assert_eq!(got.band, Band::None);
        assert!(got.calibrated);
        assert_eq!(got.top_score, None);
        assert_eq!(got.margin, None);
    }

    #[test]
    fn margin_is_top_minus_second_or_null_for_one_hit() {
        let t = table(0.8, 0.6, 0.4);
        let two = confidence(Some(0.82), Some(0.71), ScoreKind::Rerank, "rerank-2.5", &t);
        assert!(
            (two.margin.unwrap_or(f32::NAN) - 0.11).abs() < 1e-6,
            "{two:?}"
        );
        let one = confidence(Some(0.82), None, ScoreKind::Rerank, "rerank-2.5", &t);
        assert_eq!(one.margin, None);
    }

    #[test]
    fn uncalibrated_uses_min_score_as_a_low_band_fallback() {
        let t = CalibrationTable {
            min_score: Some(0.42),
            ..CalibrationTable::default()
        };
        let above = confidence(Some(0.5), None, ScoreKind::Dense, "stub", &t);
        assert_eq!(above.band, Band::Low);
        assert!(!above.calibrated);
        let below = confidence(Some(0.1), None, ScoreKind::Dense, "stub", &t);
        assert_eq!(below.band, Band::None);
        assert!(!below.calibrated);
    }

    #[test]
    fn uncalibrated_without_a_floor_is_unknown_but_no_hits_are_none() {
        let t = CalibrationTable::default();
        let got = confidence(Some(0.9), None, ScoreKind::Fused, "stub", &t);
        assert_eq!(got.band, Band::Unknown);
        assert!(!got.calibrated);
        let empty = confidence(None, None, ScoreKind::Fused, "stub", &t);
        assert_eq!(empty.band, Band::None);
        assert!(!empty.calibrated);
    }

    #[test]
    fn a_model_or_score_kind_mismatch_is_uncalibrated() {
        let t = table(0.8, 0.6, 0.4);
        let other_model = confidence(Some(0.9), None, ScoreKind::Rerank, "other", &t);
        assert!(!other_model.calibrated);
        assert_eq!(other_model.band, Band::Low, "the floor still applies");
        let other_kind = confidence(Some(0.9), None, ScoreKind::Dense, "rerank-2.5", &t);
        assert!(!other_kind.calibrated);
    }

    #[test]
    fn built_in_table_is_empty_because_no_three_point_fit_is_committed() {
        assert!(CALIBRATIONS.is_empty());
        assert!(CalibrationTable::built_in().entries.is_empty());
    }

    #[test]
    fn score_evidence_takes_the_top_two_in_descending_order() {
        let e = ScoreEvidence::from_scores([0.3, 0.9, 0.7], ScoreKind::Rerank);
        assert_eq!(e.top, Some(0.9));
        assert_eq!(e.second, Some(0.7));
        assert_eq!(e.score_kind, ScoreKind::Rerank);
        let one = ScoreEvidence::from_scores([0.3], ScoreKind::Dense);
        assert_eq!(one.top, Some(0.3));
        assert_eq!(one.second, None);
    }

    #[test]
    fn override_json_parses_and_rejects_bad_tables() {
        let raw = r#"[{"modelId":"m","scoreKind":"rerank","high":0.8,"medium":0.6,"low":0.4}]"#;
        let parsed = CalibrationTable::from_json(raw).expect("valid calibration");
        assert_eq!(parsed.entries.len(), 1);
        assert!(confidence(Some(0.85), None, ScoreKind::Rerank, "m", &parsed).calibrated);
        assert!(CalibrationTable::from_json("{}").is_err());
        assert!(
            CalibrationTable::from_json(
                r#"[{"modelId":"m","scoreKind":"dense","high":0.3,"medium":0.6,"low":0.4}]"#
            )
            .is_err()
        );
        assert!(
            CalibrationTable::from_json(
                r#"[{"modelId":"","scoreKind":"dense","high":0.9,"medium":0.6,"low":0.4}]"#
            )
            .is_err()
        );
    }

    #[test]
    fn wire_spellings_match_the_api() {
        assert_eq!(ScoreKind::Rerank.as_str(), "rerank");
        assert_eq!(ScoreKind::Dense.as_str(), "dense");
        assert_eq!(ScoreKind::Fused.as_str(), "fused");
        assert_eq!(Band::High.as_str(), "high");
        assert_eq!(Band::Medium.as_str(), "medium");
        assert_eq!(Band::Low.as_str(), "low");
        assert_eq!(Band::None.as_str(), "none");
        assert_eq!(Band::Unknown.as_str(), "unknown");
    }
}
