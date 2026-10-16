//! Pure information-retrieval metrics. No I/O, no clock, no randomness that is
//! not seeded by the caller, so a number produced by this module can be
//! reproduced exactly.
//!
//! The metrics are the standard ones:
//!
//! * **nDCG@k** with exponential gains (`2^rel - 1`) and the `log2` discount,
//! * **Recall@k**, **MRR@k**, **Hit@1** and **Precision@k**.
//!
//! "Relevant" means a positive grade; grades above one change only nDCG. A
//! query with no positive grade at a level is not evaluable there and is
//! excluded from that level's mean (as in BEIR), rather than counted as a zero.

use std::collections::HashMap;

use serde::Serialize;

/// Graded relevance for one query: key → grade. A grade `<= 0` is not relevant.
pub type Gold = HashMap<String, i32>;

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

/// Gain of a grade: `2^rel - 1`.
fn gain(rel: i32) -> f64 {
    2.0_f64.powi(rel) - 1.0
}

/// Discount at a 0-based position: `log2(position + 2)`.
fn discount(position: usize) -> f64 {
    (position as f64 + 2.0).log2()
}

fn dcg(ranked: &[String], gold: &Gold, k: usize) -> f64 {
    ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, key)| {
            let rel = gold.get(key).copied().unwrap_or(0);
            if rel > 0 {
                gain(rel) / discount(i)
            } else {
                0.0
            }
        })
        .sum()
}

fn ideal_dcg(gold: &Gold, k: usize) -> f64 {
    let mut grades: Vec<i32> = gold.values().copied().filter(|&s| s > 0).collect();
    grades.sort_unstable_by(|a, b| b.cmp(a));
    grades
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, &rel)| gain(rel) / discount(i))
        .sum()
}

/// Evaluate one query at cutoff `k`.
///
/// `ranked` must already be unique keys in rank order (see
/// [`unique_first_occurrence`]). Returns `None` when the gold set has no
/// positive grade, i.e. the query is not evaluable at this level.
#[must_use]
pub fn evaluate_query(ranked: &[String], gold: &Gold, k: usize) -> Option<QueryMetrics> {
    let k = k.max(1);
    let idcg = ideal_dcg(gold, k);
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
        ndcg: dcg(ranked, gold, k) / idcg,
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

/// One metric aggregated over the evaluable queries at a level.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LevelReport {
    pub ndcg: Aggregate,
    pub recall: Aggregate,
    pub mrr: Aggregate,
    pub hit1: Aggregate,
    pub precision: Aggregate,
    pub n_queries: usize,
}

/// Aggregate per-query metrics into level means with confidence intervals.
#[must_use]
pub fn aggregate_level(per_query: &[QueryMetrics]) -> LevelReport {
    let col = |f: fn(&QueryMetrics) -> f64| -> Aggregate {
        let values: Vec<f64> = per_query.iter().map(f).collect();
        bootstrap_ci(&values, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED)
    };
    LevelReport {
        ndcg: col(|m| m.ndcg),
        recall: col(|m| m.recall),
        mrr: col(|m| m.mrr),
        hit1: col(|m| m.hit1),
        precision: col(|m| m.precision),
        n_queries: per_query.len(),
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
        // gold: d1 grade 2, d2 grade 1.
        // DCG  = (2^2-1)/log2(2) + (2^1-1)/log2(3)
        //      = 3/1 + 1/1.5849625 = 3 + 0.6309298 = 3.6309298.
        // IDCG = the same, so nDCG = 1.
        let m = evaluate_query(&keys(&["d1", "d2"]), &gold(&[("d1", 2), ("d2", 1)]), 10)
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
        // DCG  = 0 + 3/log2(3) = 1.8927893.
        // IDCG = 3/log2(2) = 3. nDCG = 0.6309298...
        // MRR  = 1/2 = 0.5 (d1 is second). Hit@1 = 0 (d2 is not relevant).
        let m = evaluate_query(&keys(&["d2", "d1"]), &gold(&[("d1", 2)]), 10).expect("evaluable");
        // DCG/IDCG = (3/log2(3)) / 3 = 1/log2(3).
        assert!(close(m.ndcg, 1.0 / 3.0_f64.log2()), "ndcg {}", m.ndcg);
        assert!(close(m.recall, 1.0));
        assert!(close(m.mrr, 0.5));
        assert!(close(m.hit1, 0.0));
    }

    #[test]
    fn no_relevant_gold_is_not_evaluable() {
        assert!(evaluate_query(&keys(&["a", "b"]), &gold(&[("a", 0)]), 10).is_none());
        assert!(evaluate_query(&keys(&["a"]), &Gold::new(), 10).is_none());
    }

    #[test]
    fn all_retrieved_irrelevant_scores_zero() {
        // gold {a:1}; ranked [x, y] has no relevant item.
        let m = evaluate_query(&keys(&["x", "y"]), &gold(&[("a", 1)]), 10).expect("evaluable");
        assert!(close(m.ndcg, 0.0));
        assert!(close(m.recall, 0.0));
        assert!(close(m.mrr, 0.0));
        assert!(close(m.hit1, 0.0));
        assert!(close(m.precision, 0.0));
    }

    #[test]
    fn graded_relevance_uses_exponential_gains() {
        // gold {a:3, b:2, c:1}; ranked [a,b,c]:
        // DCG  = 7/log2(2) + 3/log2(3) + 1/log2(4)
        //      = 7 + 1.8927893 + 0.5 = 9.3927893. IDCG is identical → 1.
        let perfect = evaluate_query(
            &keys(&["a", "b", "c"]),
            &gold(&[("a", 3), ("b", 2), ("c", 1)]),
            10,
        )
        .expect("evaluable");
        assert!(close(perfect.ndcg, 1.0));

        // ranked [c,b,a]:
        // DCG  = 1 + 3/1.5849625 + 7/2 = 1 + 1.8927893 + 3.5 = 6.3927893.
        // IDCG = 7 + 3/1.5849625 + 1/2 = 9.3927893.
        // nDCG = 0.680606...
        let worst = evaluate_query(
            &keys(&["c", "b", "a"]),
            &gold(&[("a", 3), ("b", 2), ("c", 1)]),
            10,
        )
        .expect("evaluable");
        let log2_3 = 3.0_f64.log2();
        let expected = (1.0 + 3.0 / log2_3 + 7.0 / 2.0) / (7.0 + 3.0 / log2_3 + 1.0 / 2.0);
        assert!(close(worst.ndcg, expected), "ndcg {}", worst.ndcg);
        assert!(close(worst.mrr, 1.0)); // c is relevant, first.
    }

    #[test]
    fn cutoff_k_stops_counting() {
        // Gold has two relevant docs; with k=1 only the first rank counts.
        // ranked [x, a], gold {a:1}: nDCG@1 = 0 (x not relevant), recall@1 = 0.
        let m = evaluate_query(&keys(&["x", "a"]), &gold(&[("a", 1)]), 1).expect("evaluable");
        assert!(close(m.ndcg, 0.0));
        assert!(close(m.recall, 0.0));
        assert!(close(m.mrr, 0.0));
        assert!(close(m.hit1, 0.0));
        assert!(close(m.precision, 0.0));

        // Same ranking at k=2: a is found at rank 2.
        let m = evaluate_query(&keys(&["x", "a"]), &gold(&[("a", 1)]), 2).expect("evaluable");
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
        let m = evaluate_query(&ranked, &gold(&[("d2", 1)]), 10).expect("evaluable");
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
        let q1 = evaluate_query(&keys(&["a"]), &gold(&[("a", 1)]), 10).unwrap();
        let q2 = evaluate_query(&keys(&["x"]), &gold(&[("a", 1)]), 10).unwrap();
        let level = aggregate_level(&[q1, q2]);
        assert_eq!(level.n_queries, 2);
        assert!(close(level.ndcg.mean, 0.5));
        assert!(close(level.mrr.mean, 0.5));
    }
}
