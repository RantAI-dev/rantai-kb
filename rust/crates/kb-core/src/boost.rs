//! Recency and authority boosts (`RET-4`) — pure math.
//!
//! A caller may ask search to favour recent or authoritative documents. The
//! decision is a per-request option, never per-tenant configuration, and it is
//! **off unless asked for**: no boost means the pipeline never computes a
//! factor or re-sorts, so the ranking is exactly the one earlier releases
//! produced.
//!
//! # The formula
//!
//! Every boosted hit starts from a base relevance in `[0, 1]`:
//!
//! * the rerank score itself when every rerank score of the request is already a
//!   probability in `[0, 1]` (Cohere/Voyage, TEI), else `sigmoid(rerank_score)`
//!   for logits — chosen once per request so the base keeps the reranker's order;
//! * otherwise `1 / (1 + rank)` on the hit's post-fusion/post-rerank rank
//!   (`rank` is 0-based, so the first hit has base `1.0` and the tenth `~0.09`).
//!
//! A boost is a weighted interpolation between "no change" and its decay or
//! normalised value:
//!
//! ```text
//! recency_decay    = 0.5 ^ (age_days / half_life_days)
//! recency_factor   = 1 - w + w * recency_decay
//! authority_factor = 1 - w + w * authority_normalised
//! final            = base * recency_factor * authority_factor
//! ```
//!
//! `w` is the request's `weight` in `(0, 1]`. A missing field — no date, no
//! numeric authority — contributes no factor at all (multiplied by `1.0`), and
//! is never an error: a document without the field simply ranks as it did.
//!
//! Age is clamped at zero: a date in the future is "as new as now" rather than
//! a boost above `1.0`.
//!
//! # Authority normalisation
//!
//! Authority is min-max normalised **per request over the candidate documents
//! that carry the value**: `(v - min) / (max - min)`. When every present value
//! is equal (or only one is), they all normalise to `1.0`; that request's
//! authority boost then changes no order, which is the honest answer — there is
//! no relative authority to express.
//!
//! # Dates
//!
//! A metadata date is ISO-8601: a date (`2024-03-01`) or a date-time with a
//! timezone (`2024-03-01T09:30:00Z`, `2024-03-01T09:30:00+07:00`). A local
//! date-time without an offset is read as UTC, the same assumption the Postgres
//! `createdAt`/`updatedAt` columns make here. Anything else is an absent field.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};

/// The longest metadata key accepted by the `RET-3` filter and the `RET-4`
/// boost. One rule for both, so a key that can be filtered on can be boosted
/// on.
pub const MAX_METADATA_KEY_LEN: usize = 64;

/// Whether `key` is a valid top-level metadata key: `[A-Za-z0-9_.-]{1,64}`.
#[must_use]
pub fn valid_metadata_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_METADATA_KEY_LEN
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Where a recency boost reads its date from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecencyField {
    /// `Document.createdAt` (server clock, ingest time).
    CreatedAt,
    /// `Document.updatedAt` (server clock; a metadata patch moves it).
    UpdatedAt,
    /// The top-level JSONB `Document.metadata` key, e.g. `effectiveDate`.
    Metadata(String),
}

impl RecencyField {
    /// The wire spelling: `createdAt`, `updatedAt`, or `metadata.<key>`.
    #[must_use]
    pub fn as_str(&self) -> String {
        match self {
            Self::CreatedAt => "createdAt".to_string(),
            Self::UpdatedAt => "updatedAt".to_string(),
            Self::Metadata(key) => format!("metadata.{key}"),
        }
    }
}

/// A recency boost: dates decay with a half-life.
#[derive(Debug, Clone, PartialEq)]
pub struct RecencyBoost {
    pub field: RecencyField,
    /// Days after which a document's recency weight is halved. Must be `> 0`.
    pub half_life_days: f64,
    /// How strongly recency counts, in `(0, 1]`. `1` is pure decay; a smaller
    /// value keeps more of the base relevance.
    pub weight: f64,
}

/// An authority boost: a numeric metadata field, normalised per request.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorityBoost {
    /// A top-level metadata key holding a JSON number.
    pub field: String,
    /// How strongly authority counts, in `(0, 1]`.
    pub weight: f64,
}

/// The boosts a request asked for. `None`/empty is the off state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoostConfig {
    pub recency: Option<RecencyBoost>,
    pub authority: Option<AuthorityBoost>,
}

impl BoostConfig {
    /// Whether this request asked for no boost at all.
    #[must_use]
    pub fn is_off(&self) -> bool {
        self.recency.is_none() && self.authority.is_none()
    }
}

/// The base relevance of a hit a reranker scored, in `[0, 1]`.
///
/// Logistic rather than a clamp: a cross-encoder's raw score is an unbounded
/// logit, and clamping would flatten every confident hit to `1.0` and make the
/// boost the only ranking signal. The logistic is strictly monotone, so with
/// the boost off (or missing) the reranker's order survives unchanged.
#[must_use]
pub fn base_from_rerank(score: f32) -> f64 {
    1.0 / (1.0 + (-f64::from(score)).exp())
}

/// The base relevance of a reranked hit, choosing the mapping once per request.
///
/// Most rerankers here (Cohere/Voyage `relevance_score`, TEI's default) already
/// return a probability in `[0, 1]`; a sigmoid on top squeezes those into
/// `[0.5, 0.73]` and lets even a weak boost outweigh relevance. So when every
/// rerank score of the request is in `[0, 1]` they are used as they are;
/// otherwise (logits) every score goes through [`base_from_rerank`]. Deciding per
/// request keeps the mapping monotone across the request's hits.
#[must_use]
pub fn base_from_rerank_in(score: f32, all_scores_are_probabilities: bool) -> f64 {
    if all_scores_are_probabilities {
        f64::from(score)
    } else {
        base_from_rerank(score)
    }
}

/// True when every given rerank score lies in `[0, 1]`.
#[must_use]
pub fn rerank_scores_are_probabilities<I: IntoIterator<Item = f32>>(scores: I) -> bool {
    scores.into_iter().all(|s| (0.0..=1.0).contains(&s))
}

/// The base relevance of a hit with no comparable score: `1 / (1 + rank)`.
#[must_use]
pub fn base_from_rank(rank: u32) -> f64 {
    1.0 / (1.0 + f64::from(rank))
}

/// The recency decay for `age_days` at `half_life_days`: `0.5 ^ (age/half)`.
///
/// A negative age (a future date) is clamped to zero: the future is not
/// evidence of extra relevance.
#[must_use]
pub fn recency_decay(age_days: f64, half_life_days: f64) -> f64 {
    let age = age_days.max(0.0);
    0.5_f64.powf(age / half_life_days)
}

/// `1 - w + w * decay`, the factor applied to the base relevance.
#[must_use]
pub fn weighted(decay: f64, weight: f64) -> f64 {
    1.0 - weight + weight * decay
}

/// The recency factor for a document dated `date`, evaluated at `now`.
#[must_use]
pub fn recency_factor(
    date: DateTime<Utc>,
    now: DateTime<Utc>,
    half_life_days: f64,
    weight: f64,
) -> f64 {
    let age_days = (now - date).num_seconds() as f64 / 86_400.0;
    weighted(recency_decay(age_days, half_life_days), weight)
}

/// The authority factor for an already-normalised value in `[0, 1]`.
#[must_use]
pub fn authority_factor(normalised: f64, weight: f64) -> f64 {
    weighted(normalised.clamp(0.0, 1.0), weight)
}

/// Min-max normalise the present values of one request's candidate documents.
///
/// `None` stays `None` (the document carries no value). With fewer than two
/// distinct present values every present value normalises to `1.0`: there is no
/// relative authority to act on.
#[must_use]
pub fn normalise_authority(values: &[Option<f64>]) -> Vec<Option<f64>> {
    let present: Vec<f64> = values.iter().filter_map(|v| *v).collect();
    let (Some(min), Some(max)) = (
        present.iter().copied().reduce(f64::min),
        present.iter().copied().reduce(f64::max),
    ) else {
        return vec![None; values.len()];
    };
    let span = max - min;
    values
        .iter()
        .map(|value| {
            value.map(|v| {
                if span > 0.0 {
                    ((v - min) / span).clamp(0.0, 1.0)
                } else {
                    1.0
                }
            })
        })
        .collect()
}

/// A JSON metadata value as an authority number. JSON numbers only; a string
/// such as `"0.9"` is not a number and the document gets no authority boost.
#[must_use]
pub fn authority_value(value: &serde_json::Value) -> Option<f64> {
    value.as_f64()
}

/// A JSON metadata date: an ISO-8601 date or date-time string.
#[must_use]
pub fn date_value(value: &serde_json::Value) -> Option<DateTime<Utc>> {
    value.as_str().and_then(parse_date)
}

/// Parse an ISO-8601 date or date-time into UTC.
///
/// Accepted: RFC 3339 (`2024-03-01T09:30:00Z`, with an offset, with fractional
/// seconds), a bare date (`2024-03-01`), and a local date-time without an
/// offset (`2024-03-01T09:30:00`, read as UTC).
#[must_use]
pub fn parse_date(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
        return Some(Utc.from_utc_datetime(&naive));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
        return Some(Utc.from_utc_datetime(&naive));
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn utc(value: &str) -> DateTime<Utc> {
        parse_date(value).expect("a valid date")
    }

    #[test]
    fn metadata_keys_match_the_filter_rule() {
        assert!(valid_metadata_key("effectiveDate"));
        assert!(valid_metadata_key("source.authority"));
        assert!(valid_metadata_key("a-b_1.2"));
        assert!(!valid_metadata_key(""));
        assert!(!valid_metadata_key("has space"));
        assert!(!valid_metadata_key("has/slash"));
        assert!(!valid_metadata_key(&"x".repeat(MAX_METADATA_KEY_LEN + 1)));
        assert!(valid_metadata_key(&"x".repeat(MAX_METADATA_KEY_LEN)));
    }

    #[test]
    fn dates_parse_in_the_documented_forms() {
        let noon = utc("2024-03-01T09:30:00Z");
        assert_eq!(utc("2024-03-01"), utc("2024-03-01T00:00:00Z"));
        assert_eq!(noon, utc("2024-03-01T16:30:00+07:00"));
        assert_eq!(utc("2024-03-01T09:30:00"), noon);
        assert_eq!(utc("2024-03-01 09:30:00"), noon);
        assert_eq!(
            utc("2024-03-01T09:30:00.500Z").timestamp(),
            noon.timestamp()
        );
        assert!(parse_date("March 1, 2024").is_none());
        assert!(parse_date("2024-13-01").is_none());
        assert!(parse_date("").is_none());
        assert!(date_value(&serde_json::json!(1234)).is_none());
    }

    #[test]
    fn decay_halves_at_the_half_life() {
        assert!((recency_decay(0.0, 30.0) - 1.0).abs() < 1e-12);
        assert!((recency_decay(30.0, 30.0) - 0.5).abs() < 1e-12);
        assert!((recency_decay(60.0, 30.0) - 0.25).abs() < 1e-12);
        // A future date is as new as now, never fresher than 1.0.
        assert!((recency_decay(-10.0, 30.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn recency_factor_interpolates_between_one_and_decay() {
        let now = utc("2025-01-01T00:00:00Z");
        let today = recency_factor(now, now, 365.0, 0.5);
        assert!((today - 1.0).abs() < 1e-12);
        // One half-life: 1 - 0.5 + 0.5*0.5 = 0.75.
        let half = recency_factor(utc("2024-01-02T00:00:00Z"), now, 365.0, 0.5);
        assert!((half - 0.75).abs() < 0.002);
        // Weight 1 is pure decay.
        let pure = recency_factor(utc("2024-01-02T00:00:00Z"), now, 365.0, 1.0);
        assert!((pure - 0.5).abs() < 0.002);
        // Missing dates are handled by the caller as `None`, not by this function.
    }

    #[test]
    fn authority_is_min_max_normalised_over_present_values() {
        assert_eq!(
            normalise_authority(&[Some(10.0), Some(20.0), Some(30.0)]),
            vec![Some(0.0), Some(0.5), Some(1.0)]
        );
        assert_eq!(
            normalise_authority(&[None, Some(4.0), Some(2.0)]),
            vec![None, Some(1.0), Some(0.0)]
        );
        // No relative authority: every present value normalises to 1.
        assert_eq!(
            normalise_authority(&[Some(7.0), Some(7.0)]),
            vec![Some(1.0), Some(1.0)]
        );
        assert_eq!(
            normalise_authority(&[None, Some(7.0)]),
            vec![None, Some(1.0)]
        );
        assert_eq!(normalise_authority(&[None, None]), vec![None, None]);
    }

    #[test]
    fn authority_factor_is_weighted() {
        assert!((authority_factor(1.0, 0.5) - 1.0).abs() < 1e-12);
        assert!((authority_factor(0.0, 0.5) - 0.5).abs() < 1e-12);
        assert!((authority_factor(0.5, 1.0) - 0.5).abs() < 1e-12);
        // Out-of-range input is clamped, never allowed to exceed 1.
        assert!((authority_factor(2.0, 1.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn probability_rerank_scores_are_used_as_they_are() {
        assert!(rerank_scores_are_probabilities([0.1, 0.9, 1.0, 0.0]));
        assert!(!rerank_scores_are_probabilities([0.4, 2.5]));
        assert!((base_from_rerank_in(0.9, true) - 0.9).abs() < 1e-6);
        assert!((base_from_rerank_in(0.0, false) - 0.5).abs() < 1e-12);
        // Logit mode stays monotone across the in-range/out-of-range boundary.
        assert!(base_from_rerank_in(1.1, false) > base_from_rerank_in(0.9, false));
    }

    #[test]
    fn base_relevance_is_bounded_and_monotone() {
        assert!((base_from_rerank(0.0) - 0.5).abs() < 1e-12);
        assert!(base_from_rerank(2.0) > base_from_rerank(1.0));
        assert!(base_from_rerank(0.0) < 1.0 && base_from_rerank(0.0) > 0.0);
        assert!((base_from_rank(0) - 1.0).abs() < 1e-12);
        assert!((base_from_rank(1) - 0.5).abs() < 1e-12);
        assert!((base_from_rank(9) - 0.1).abs() < 1e-12);
    }

    #[test]
    fn authority_values_are_json_numbers_only() {
        assert_eq!(authority_value(&serde_json::json!(0.9)), Some(0.9));
        assert_eq!(authority_value(&serde_json::json!(3)), Some(3.0));
        assert_eq!(authority_value(&serde_json::json!("0.9")), None);
        assert_eq!(authority_value(&serde_json::json!(true)), None);
        assert_eq!(authority_value(&serde_json::Value::Null), None);
    }
}
