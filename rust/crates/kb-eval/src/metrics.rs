//! Pure information-retrieval metrics. No I/O, no clock, no randomness that is
//! not seeded by the caller, so a number produced by this module can be
//! reproduced exactly.
//!
//! The metrics are the standard ones:
//!
//! * **nDCG@k** with **linear** gain (`gain = rel`) and the `log2` discount,
//! * **Recall@k**, **MRR@k**, **Hit@1** and **Precision@k**.
//!
//! # Which nDCG
//!
//! Linear gain is the default because it is what the public leaderboards
//! report: `trec_eval`'s `ndcg_cut` and `ndcg` (unless `-m ndcg_burges`),
//! `pytrec_eval`, BEIR, MTEB and the ViDoRe pipeline leaderboard all compute
//! `rel / log2(i + 2)`. A number that is only comparable with our own past runs
//! is worth nothing, so linear is the default and the Burges exponential gain
//! `2^rel - 1` sits behind [`Gain::Exponential`] for anyone reproducing the
//! 2005 paper.
//!
//! The two differ only when grades are graded and the order is imperfect, and
//! they differ *a lot*: for qrels `a 1`, `b 3` with the run `a, b`, linear
//! gives `0.797` and exponential `0.710`. Both are "nDCG@10" on different
//! dashboards, which is exactly why the gain in force is recorded in every
//! output.
//!
//! "Relevant" means a positive grade; grades above one change only nDCG. A
//! query with no positive grade at a level is not evaluable there and is
//! excluded from that level's mean (as in BEIR), rather than counted as a zero.

use std::collections::HashMap;

use serde::Serialize;

/// Graded relevance for one query: key → grade. A grade `<= 0` is not relevant.
pub type Gold = HashMap<String, i32>;

/// How a relevance grade turns into a gain inside nDCG.
///
/// The variant is part of the reported metric, never an implicit default at the
/// call site: two runs that disagree about the gain are not comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Gain {
    /// `gain = rel`. `trec_eval`, `pytrec_eval`, BEIR, MTEB, ViDoRe.
    #[default]
    Linear,
    /// `gain = 2^rel - 1`. Burges et al. 2005, `ranx ndcg_burges`,
    /// `trec_eval -m ndcg_burges`.
    Exponential,
}

impl Gain {
    /// The gain of a grade under this variant.
    #[must_use]
    pub fn of(self, rel: i32) -> f64 {
        match self {
            Self::Linear => rel as f64,
            Self::Exponential => 2.0_f64.powi(rel) - 1.0,
        }
    }

    /// Parse the `--gain` flag value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "linear" => Some(Self::Linear),
            "exponential" | "burges" => Some(Self::Exponential),
            _ => None,
        }
    }

    /// The name written into `metrics.json` and `report.md`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Exponential => "exponential",
        }
    }
}

/// The number of bootstrap resamples used for every confidence interval.
pub const BOOTSTRAP_RESAMPLES: usize = 1000;

/// Fixed seed for the bootstrap, so two runs of the same data agree bit for bit.
pub const BOOTSTRAP_SEED: u64 = 0x5EED_2026_0B00_0001;

/// Metrics for one query at one cutoff.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryMetrics {
    pub ndcg: f64,
    pub recall: f64,
    pub mrr: f64,
    pub hit1: f64,
    pub precision: f64,
}

impl QueryMetrics {
    /// The score of a query the server could not answer.
    ///
    /// Zero on every metric, at every level where the query was evaluable. It
    /// is what a failed query costs the headline means: excluding it instead
    /// would hand a backend that times out on exactly the hard queries a *better*
    /// score than one that answers them badly.
    pub const ZERO: Self = Self {
        ndcg: 0.0,
        recall: 0.0,
        mrr: 0.0,
        hit1: 0.0,
        precision: 0.0,
    };
}

/// Collapse a ranked sequence of keys to unique keys, keeping first occurrence.
///
/// A chunk store returns many chunks per document; the document-level ranking is
/// the order in which documents first appear.
#[must_use]
pub fn unique_first_occurrence<I: IntoIterator<Item = String>>(items: I) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|k| seen.insert(k.clone()))
        .collect()
}

/// Discount at a 0-based position: `log2(position + 2)`.
fn discount(position: usize) -> f64 {
    (position as f64 + 2.0).log2()
}

/// DCG over the top `k` of `ranked`.
///
/// Folded from an explicit `0.0` rather than `sum()`: `Sum<f64>` folds from
/// `-0.0`, so an empty ranking would produce `-0.0` and a report would print
/// `-0.000` for a query that simply found nothing.
fn dcg(ranked: &[String], gold: &Gold, k: usize, gain: Gain) -> f64 {
    ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, key)| {
            let rel = gold.get(key).copied().unwrap_or(0);
            if rel > 0 {
                gain.of(rel) / discount(i)
            } else {
                0.0
            }
        })
        .fold(0.0, |acc, value| acc + value)
}

fn ideal_dcg(gold: &Gold, k: usize, gain: Gain) -> f64 {
    let mut grades: Vec<i32> = gold.values().copied().filter(|&s| s > 0).collect();
    grades.sort_unstable_by(|a, b| b.cmp(a));
    grades
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, &rel)| gain.of(rel) / discount(i))
        .sum()
}

/// Whether a gold set has anything to score: one positive grade or more.
///
/// This is the "evaluable at this level" rule the report and the failure
/// accounting share.
#[must_use]
pub fn has_positive_gold(gold: &Gold) -> bool {
    gold.values().any(|&rel| rel > 0)
}

/// Evaluate one query at cutoff `k` under `gain`.
///
/// `ranked` must already be unique keys in rank order (see
/// [`unique_first_occurrence`]). Returns `None` when the gold set has no
/// positive grade, i.e. the query is not evaluable at this level.
#[must_use]
pub fn evaluate_query(
    ranked: &[String],
    gold: &Gold,
    k: usize,
    gain: Gain,
) -> Option<QueryMetrics> {
    let k = k.max(1);
    let idcg = ideal_dcg(gold, k, gain);
    if idcg <= 0.0 {
        return None;
    }
    let relevant_total = gold.values().filter(|&&s| s > 0).count();
    let top: Vec<&String> = ranked.iter().take(k).collect();
    let relevant_in_top = top
        .iter()
        .filter(|key| gold.get(key.as_str()).copied().unwrap_or(0) > 0)
        .count();
    let mrr = ranked
        .iter()
        .take(k)
        .position(|key| gold.get(key).copied().unwrap_or(0) > 0)
        .map_or(0.0, |i| 1.0 / (i as f64 + 1.0));
    let hit1 = ranked
        .first()
        .is_some_and(|key| gold.get(key).copied().unwrap_or(0) > 0);
    Some(QueryMetrics {
        ndcg: dcg(ranked, gold, k, gain) / idcg,
        recall: relevant_in_top as f64 / relevant_total as f64,
        mrr,
        hit1: if hit1 { 1.0 } else { 0.0 },
        precision: relevant_in_top as f64 / k as f64,
    })
}

/// A mean with a bootstrap confidence interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Aggregate {
    pub mean: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub n: usize,
}

impl Aggregate {
    fn empty() -> Self {
        Self {
            mean: 0.0,
            ci_low: 0.0,
            ci_high: 0.0,
            n: 0,
        }
    }
}

/// The linear-interpolation percentile of an already-sorted slice, `p` in 0..=1.
#[must_use]
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = p.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let frac = rank - lower as f64;
        sorted[lower] * (1.0 - frac) + sorted[upper] * frac
    }
}

/// Small deterministic PRNG (SplitMix64). Dependency-free and reproducible, so
/// a confidence interval can be checked by re-running it.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform index in `0..n` (modulo bias is negligible at these sizes).
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

/// Mean and the 95% percentile-bootstrap interval over `values`.
///
/// `resamples` re-samples of the same size are drawn with replacement, seeded by
/// `seed`; the interval is the 2.5th/97.5th percentile of the resampled means.
#[must_use]
pub fn bootstrap_ci(values: &[f64], resamples: usize, seed: u64) -> Aggregate {
    if values.is_empty() {
        return Aggregate::empty();
    }
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    if n == 1 {
        return Aggregate {
            mean,
            ci_low: mean,
            ci_high: mean,
            n,
        };
    }
    let mut rng = SplitMix64::new(seed);
    let mut means = Vec::with_capacity(resamples);
    for _ in 0..resamples.max(1) {
        let mut sum = 0.0;
        for _ in 0..n {
            sum += values[rng.below(n)];
        }
        means.push(sum / n as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Aggregate {
        mean,
        ci_low: percentile(&means, 0.025),
        ci_high: percentile(&means, 0.975),
        n,
    }
}

/// One metric aggregated over a set of per-query values, with its interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Means {
    pub ndcg: Aggregate,
    pub recall: Aggregate,
    pub mrr: Aggregate,
    pub hit1: Aggregate,
    pub precision: Aggregate,
    /// Queries behind this mean — the evaluable count, not the asked count.
    pub n_queries: usize,
}

/// One metric aggregated over the evaluable queries at a level.
///
/// The five headline fields are **strict**: every evaluable query is in them,
/// including the ones the server failed to answer (which enter as
/// [`QueryMetrics::ZERO`]). `answered` is the same arithmetic over answered
/// queries only and is published as a secondary diagnostic, because that is the
/// number that separates "the backend is worse" from "the backend is down".
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LevelReport {
    pub ndcg: Aggregate,
    pub recall: Aggregate,
    pub mrr: Aggregate,
    pub hit1: Aggregate,
    pub precision: Aggregate,
    pub n_queries: usize,
    /// Secondary: answered queries only.
    pub answered: Means,
}

fn means(per_query: &[QueryMetrics]) -> Means {
    let col = |f: fn(&QueryMetrics) -> f64| -> Aggregate {
        let values: Vec<f64> = per_query.iter().map(f).collect();
        bootstrap_ci(&values, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED)
    };
    Means {
        ndcg: col(|m| m.ndcg),
        recall: col(|m| m.recall),
        mrr: col(|m| m.mrr),
        hit1: col(|m| m.hit1),
        precision: col(|m| m.precision),
        n_queries: per_query.len(),
    }
}

/// Aggregate per-query metrics into level means with confidence intervals.
///
/// `strict` is the headline population (evaluable queries, failed ones included
/// as zeros); `answered` is the secondary one (evaluable queries the server
/// actually answered).
#[must_use]
pub fn aggregate_level(strict: &[QueryMetrics], answered: &[QueryMetrics]) -> LevelReport {
    let head = means(strict);
    LevelReport {
        ndcg: head.ndcg,
        recall: head.recall,
        mrr: head.mrr,
        hit1: head.hit1,
        precision: head.precision,
        n_queries: head.n_queries,
        answered: means(answered),
    }
}

/// Client-side latency summary in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LatencyReport {
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub n: usize,
}

/// Summarise a set of per-query latencies (milliseconds).
#[must_use]
pub fn latency_report(mut samples: Vec<f64>) -> LatencyReport {
    if samples.is_empty() {
        return LatencyReport {
            mean: 0.0,
            p50: 0.0,
            p95: 0.0,
            p99: 0.0,
            n: 0,
        };
    }
    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n as f64;
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    LatencyReport {
        mean,
        p50: percentile(&samples, 0.50),
        p95: percentile(&samples, 0.95),
        p99: percentile(&samples, 0.99),
        n,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    fn gold(pairs: &[(&str, i32)]) -> Gold {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn perfect_ranking_is_one() {
        // gold: d1 grade 2, d2 grade 1, linear gain.
        // DCG  = 2/log2(2) + 1/log2(3) = 2 + 0.6309298 = 2.6309298.
        // IDCG = the same, so nDCG = 1.
        let m = evaluate_query(
            &keys(&["d1", "d2"]),
            &gold(&[("d1", 2), ("d2", 1)]),
            10,
            Gain::Linear,
        )
        .expect("evaluable");
        assert!(close(m.ndcg, 1.0), "ndcg {}", m.ndcg);
        assert!(close(m.recall, 1.0));
        assert!(close(m.mrr, 1.0));
        assert!(close(m.hit1, 1.0));
        assert!(close(m.precision, 2.0 / 10.0));
    }

    #[test]
    fn reversed_ranking_keeps_metric_below_one() {
        // ranked [d2, d1], gold {d1:2}. Only d1 is relevant.
        // DCG  = 0 + 2/log2(3) = 1.2618595.
        // IDCG = 2/log2(2) = 2. nDCG = 0.6309298...
        // MRR  = 1/2 = 0.5 (d1 is second). Hit@1 = 0 (d2 is not relevant).
        let m = evaluate_query(&keys(&["d2", "d1"]), &gold(&[("d1", 2)]), 10, Gain::Linear)
            .expect("evaluable");
        // DCG/IDCG = (2/log2(3)) / 2 = 1/log2(3).
        assert!(close(m.ndcg, 1.0 / 3.0_f64.log2()), "ndcg {}", m.ndcg);
        assert!(close(m.recall, 1.0));
        assert!(close(m.mrr, 0.5));
        assert!(close(m.hit1, 0.0));
    }

    #[test]
    fn no_relevant_gold_is_not_evaluable() {
        assert!(evaluate_query(&keys(&["a", "b"]), &gold(&[("a", 0)]), 10, Gain::Linear).is_none());
        assert!(evaluate_query(&keys(&["a"]), &Gold::new(), 10, Gain::Linear).is_none());
    }

    #[test]
    fn all_retrieved_irrelevant_scores_zero() {
        // gold {a:1}; ranked [x, y] has no relevant item.
        let m = evaluate_query(&keys(&["x", "y"]), &gold(&[("a", 1)]), 10, Gain::Linear)
            .expect("evaluable");
        assert!(close(m.ndcg, 0.0));
        assert!(close(m.recall, 0.0));
        assert!(close(m.mrr, 0.0));
        assert!(close(m.hit1, 0.0));
        assert!(close(m.precision, 0.0));
    }

    #[test]
    fn an_empty_ranking_is_plus_zero_not_negative_zero() {
        // `Sum<f64>` folds from -0.0, which made a query that retrieved nothing
        // render as "-0.000" in report.md.
        let m = evaluate_query(&[], &gold(&[("a", 1)]), 10, Gain::Linear).expect("evaluable");
        for (name, value) in [
            ("ndcg", m.ndcg),
            ("recall", m.recall),
            ("mrr", m.mrr),
            ("precision", m.precision),
        ] {
            assert_eq!(value, 0.0, "{name} was {value}");
            assert!(value.is_sign_positive(), "{name} was {value:?}");
        }
        let level = aggregate_level(&[m], &[m]);
        assert!(level.ndcg.mean.is_sign_positive());
        assert!(level.ndcg.ci_low.is_sign_positive());
        assert!(level.ndcg.ci_high.is_sign_positive());
    }

    // The case that decided the gain: qrels `a 1`, `b 3`, run `a` then `b`.
    //
    // `ranx` reports `ndcg@10 = 0.7967` for this and `ndcg_burges@10 = 0.7097`;
    // `trec_eval -m ndcg` agrees with the first. We must produce the first by
    // default, or our nDCG is not comparable with any public leaderboard.
    #[test]
    fn graded_relevance_uses_linear_gain_by_default() {
        let g = gold(&[("a", 1), ("b", 3)]);
        let ranked = keys(&["a", "b"]);

        // DCG  = 1/log2(2) + 3/log2(3) = 1 + 1.8927893 = 2.8927893
        // IDCG = 3/log2(2) + 1/log2(3) = 3 + 0.6309298 = 3.6309298
        // nDCG = 2.8927893 / 3.6309298 = 0.7967076...
        let log2_3 = 3.0_f64.log2();
        let expected = (1.0 / 1.0 + 3.0 / log2_3) / (3.0 / 1.0 + 1.0 / log2_3);
        assert!((expected - 0.7967).abs() < 0.0001, "expected ≈ 0.797");

        let linear = evaluate_query(&ranked, &g, 10, Gain::Linear).expect("evaluable");
        assert!(
            (linear.ndcg - expected).abs() < 1e-12,
            "ndcg {}",
            linear.ndcg
        );
        assert!(
            (linear.ndcg - 0.797).abs() < 0.001,
            "linear nDCG must be ≈ 0.797, was {}",
            linear.ndcg
        );

        // The same data under Burges' exponential gain:
        // DCG  = (2^1-1)/log2(2) + (2^3-1)/log2(3) = 1 + 4.4165088 = 5.4165088
        // IDCG = (2^3-1)/log2(2) + (2^1-1)/log2(3) = 7 + 0.6309298 = 7.6309298
        // nDCG = 5.4165088 / 7.6309298 = 0.709711...
        let burges = evaluate_query(&ranked, &g, 10, Gain::Exponential).expect("evaluable");
        let expected_burges = (1.0 + 7.0 / log2_3) / (7.0 + 1.0 / log2_3);
        assert!(
            (burges.ndcg - expected_burges).abs() < 1e-12,
            "ndcg {}",
            burges.ndcg
        );
        assert!(
            (burges.ndcg - 0.710).abs() < 0.001,
            "exponential nDCG must be ≈ 0.710, was {}",
            burges.ndcg
        );
        // The two are genuinely different numbers, not a relabelling.
        assert!(linear.ndcg - burges.ndcg > 0.08);
        // Recall/MRR/Hit@1/P@k never depend on the gain.
        assert!((linear.recall - burges.recall).abs() < 1e-12);
        assert!((linear.mrr - burges.mrr).abs() < 1e-12);
    }

    #[test]
    fn graded_relevance_under_linear_gain_is_below_one_when_reversed() {
        // gold {a:3, b:2, c:1}; ranked [c,b,a], linear gain:
        // DCG  = 1/log2(2) + 2/log2(3) + 3/log2(4)
        //      = 1 + 1.2618595 + 1.5 = 3.7618595.
        // IDCG = 3/log2(2) + 2/log2(3) + 1/log2(4)
        //      = 3 + 1.2618595 + 0.5 = 4.7618595.
        // nDCG = 3.7618595 / 4.7618595 = 0.7899980...
        let g = gold(&[("a", 3), ("b", 2), ("c", 1)]);
        let perfect =
            evaluate_query(&keys(&["a", "b", "c"]), &g, 10, Gain::Linear).expect("evaluable");
        assert!(close(perfect.ndcg, 1.0));

        let worst =
            evaluate_query(&keys(&["c", "b", "a"]), &g, 10, Gain::Linear).expect("evaluable");
        let log2_3 = 3.0_f64.log2();
        let expected = (1.0 + 2.0 / log2_3 + 3.0 / 2.0) / (3.0 + 2.0 / log2_3 + 1.0 / 2.0);
        assert!(close(worst.ndcg, expected), "ndcg {}", worst.ndcg);
        assert!(close(worst.mrr, 1.0)); // c is relevant, first.
        // Exponential gain over the same data disagrees, which is why it is a
        // flag and not the default.
        let burges =
            evaluate_query(&keys(&["c", "b", "a"]), &g, 10, Gain::Exponential).expect("evaluable");
        // 0.6806 under exponential gain, 0.7900 under linear: not a relabelling.
        assert!((burges.ndcg - worst.ndcg).abs() > 0.10);
    }

    #[test]
    fn the_exponential_gain_is_kept_behind_an_explicit_option() {
        // gold {a:3, b:2, c:1}; ranked [c,b,a]:
        // DCG  = (2^1-1)/log2(2) + (2^2-1)/log2(3) + (2^3-1)/log2(4)
        //      = 1 + 1.8927893 + 3.5 = 6.3927893.
        // IDCG = 7 + 3/1.5849625 + 1/2 = 9.3927893.
        // nDCG = 0.680606...
        let g = gold(&[("a", 3), ("b", 2), ("c", 1)]);
        let worst =
            evaluate_query(&keys(&["c", "b", "a"]), &g, 10, Gain::Exponential).expect("evaluable");
        let log2_3 = 3.0_f64.log2();
        let expected = (1.0 + 3.0 / log2_3 + 7.0 / 2.0) / (7.0 + 3.0 / log2_3 + 1.0 / 2.0);
        assert!(close(worst.ndcg, expected), "ndcg {}", worst.ndcg);
    }

    #[test]
    fn gain_parses_and_names_itself() {
        assert_eq!(Gain::parse("linear"), Some(Gain::Linear));
        assert_eq!(Gain::parse("  Exponential "), Some(Gain::Exponential));
        // `ranx` and trec_eval both spell the Burges variant two ways.
        assert_eq!(Gain::parse("burges"), Some(Gain::Exponential));
        assert_eq!(Gain::parse("ndcg_burges"), None);
        assert_eq!(Gain::parse("quadratic"), None);
        assert_eq!(Gain::default(), Gain::Linear);
        assert_eq!(Gain::Linear.as_str(), "linear");
        assert_eq!(Gain::Exponential.as_str(), "exponential");
        assert!(close(Gain::Linear.of(0), 0.0));
        assert!(close(Gain::Linear.of(3), 3.0));
        assert!(close(Gain::Exponential.of(0), 0.0));
        assert!(close(Gain::Exponential.of(3), 7.0));
    }

    #[test]
    fn cutoff_k_stops_counting() {
        // Gold has two relevant docs; with k=1 only the first rank counts.
        // ranked [x, a], gold {a:1}: nDCG@1 = 0 (x not relevant), recall@1 = 0.
        let m = evaluate_query(&keys(&["x", "a"]), &gold(&[("a", 1)]), 1, Gain::Linear)
            .expect("evaluable");
        assert!(close(m.ndcg, 0.0));
        assert!(close(m.recall, 0.0));
        assert!(close(m.mrr, 0.0));
        assert!(close(m.hit1, 0.0));
        assert!(close(m.precision, 0.0));

        // Same ranking at k=2: a is found at rank 2.
        let m = evaluate_query(&keys(&["x", "a"]), &gold(&[("a", 1)]), 2, Gain::Linear)
            .expect("evaluable");
        // DCG = 1/log2(3); IDCG = 1/log2(2) = 1. nDCG = 1/log2(3).
        assert!(close(m.ndcg, 1.0 / 3.0_f64.log2()));
        assert!(close(m.recall, 1.0));
        assert!(close(m.mrr, 0.5));
        assert!(close(m.hit1, 0.0));
        assert!(close(m.precision, 0.5));
    }

    #[test]
    fn duplicates_collapse_to_first_occurrence() {
        let ranked = unique_first_occurrence(keys(&["d1", "d1", "d2", "d1", "d3"]));
        assert_eq!(ranked, keys(&["d1", "d2", "d3"]));
        // And the collapsed ranking is what the metric sees: d2 is rank 2.
        let m = evaluate_query(&ranked, &gold(&[("d2", 1)]), 10, Gain::Linear).expect("evaluable");
        assert!(close(m.mrr, 0.5));
    }

    #[test]
    fn bootstrap_is_reproducible_and_brackets_the_mean() {
        let values = vec![0.0, 0.0, 1.0, 0.5, 1.0];
        let a = bootstrap_ci(&values, 1000, BOOTSTRAP_SEED);
        let b = bootstrap_ci(&values, 1000, BOOTSTRAP_SEED);
        assert_eq!(a, b, "same seed must give the same interval");
        let mean = 2.5 / 5.0;
        assert!(close(a.mean, mean));
        assert!(a.ci_low <= mean && mean <= a.ci_high);
        assert!(a.ci_low <= a.ci_high);
        assert_eq!(a.n, 5);
    }

    #[test]
    fn bootstrap_of_a_constant_is_a_point() {
        let a = bootstrap_ci(&[0.25, 0.25, 0.25], 1000, BOOTSTRAP_SEED);
        assert!(close(a.mean, 0.25));
        assert!(close(a.ci_low, 0.25));
        assert!(close(a.ci_high, 0.25));
    }

    #[test]
    fn percentile_interpolates() {
        let s = [0.0, 1.0, 2.0, 3.0, 4.0];
        assert!(close(percentile(&s, 0.0), 0.0));
        assert!(close(percentile(&s, 1.0), 4.0));
        assert!(close(percentile(&s, 0.5), 2.0));
        assert!(close(percentile(&s, 0.25), 1.0));
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn latency_percentiles() {
        let r = latency_report((1..=100).map(f64::from).collect());
        assert_eq!(r.n, 100);
        assert!(close(r.mean, 50.5));
        // nearest-by-interpolation p95 of 1..=100: rank = 0.95*99 = 94.05 → 95.05
        assert!((r.p95 - 95.05).abs() < 1e-9, "p95 {}", r.p95);
        assert!(r.p50 < r.p95 && r.p95 <= r.p99);
    }

    #[test]
    fn aggregate_level_keeps_only_evaluable_queries() {
        let q1 = evaluate_query(&keys(&["a"]), &gold(&[("a", 1)]), 10, Gain::Linear).unwrap();
        let q2 = evaluate_query(&keys(&["x"]), &gold(&[("a", 1)]), 10, Gain::Linear).unwrap();
        let level = aggregate_level(&[q1, q2], &[q1, q2]);
        assert_eq!(level.n_queries, 2);
        assert!(close(level.ndcg.mean, 0.5));
        assert!(close(level.mrr.mean, 0.5));
        assert_eq!(level.answered.n_queries, 2);
        assert!(close(level.answered.ndcg.mean, 0.5));
    }

    #[test]
    fn a_failed_query_is_zero_in_the_headline_and_absent_from_the_secondary() {
        // The regression this pins: a backend that fails the hard queries used
        // to drop them, which raised its score. Strict means must not.
        let q1 = evaluate_query(&keys(&["a"]), &gold(&[("a", 1)]), 10, Gain::Linear).unwrap();
        let level = aggregate_level(&[q1, QueryMetrics::ZERO], &[q1]);
        assert_eq!(level.n_queries, 2, "the failure is in the headline count");
        assert!(
            close(level.ndcg.mean, 0.5),
            "strict nDCG {}",
            level.ndcg.mean
        );
        assert!(close(level.recall.mean, 0.5));
        assert!(close(level.mrr.mean, 0.5));
        assert!(close(level.hit1.mean, 0.5));
        assert!(
            close(level.precision.mean, 0.05),
            "P@10 of one hit in two queries"
        );

        assert_eq!(level.answered.n_queries, 1);
        assert!(close(level.answered.ndcg.mean, 1.0));
        assert!(close(level.answered.mrr.mean, 1.0));
    }

    #[test]
    fn the_zero_score_is_plus_zero() {
        let z = QueryMetrics::ZERO;
        for (name, value) in [
            ("ndcg", z.ndcg),
            ("recall", z.recall),
            ("mrr", z.mrr),
            ("hit1", z.hit1),
            ("precision", z.precision),
        ] {
            assert_eq!(value, 0.0, "{name}");
            assert!(value.is_sign_positive(), "{name} was {value:?}");
        }
    }
}
