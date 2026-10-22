//! Reciprocal Rank Fusion and the shared "context builder" helpers (source
//! dedupe, source numbering, context string).

use std::collections::HashMap;

/// The RRF constant Qdrant uses server-side.
///
/// Qdrant's RRF is `sum over arms of 1 / (2 + rank)`, with `rank` 0-based: a
/// candidate at the top of one arm contributes `0.5`, one at rank 2
/// contributes `0.25`. **This is the port's two-arm fusion** — there is no
/// second constant to choose, because a store that fuses server-side (Qdrant)
/// has its own and a store that cannot (the in-memory reference index,
/// SurrealDB) has to reproduce it exactly or the two disagree about the same
/// candidates.
///
/// The original RRF paper's `60` is *not* this, and the difference is not
/// cosmetic: with `k = 60` the gap between rank 1 and rank 3 is a few percent
/// of the score, so a passage both arms like barely beats one a single arm
/// likes. Measured on `miracl-id-200` (`docs/kf-vplane-eval.md`), re-sorting
/// Qdrant's fused list with `k = 60` cost **0.01 nDCG@10 and 0.017 MRR@10**,
/// in either engine, and accounted for the whole significant MRR gap against
/// moco-kf's V-plane.
pub const RRF_K: f32 = 2.0;

/// One arm's contribution to a candidate's RRF score: `1 / (RRF_K + rank)`,
/// with `rank` 0-based inside that arm's own list.
///
/// Qdrant computes exactly this server-side, in `f32`. Doing the same
/// arithmetic in `f32` here is what lets an adapter that fuses in process and
/// the store that fuses server-side produce the same numbers, not merely the
/// same order.
#[must_use]
pub fn rrf_contribution(rank: u32) -> f32 {
    1.0 / (RRF_K + rank as f32)
}

/// RRF options.
#[derive(Debug, Clone, Copy)]
pub struct RrfOptions {
    /// RRF constant — larger values flatten rank differences. Default 60 per the
    /// original paper.
    pub k: u32,
    /// Max results returned after fusion. `None` = no cap.
    pub limit: Option<u32>,
}

impl Default for RrfOptions {
    fn default() -> Self {
        Self { k: 60, limit: None }
    }
}

/// One entry the fusion code can rank by id. Items must own their id.
pub trait RrfItem {
    fn id(&self) -> &str;
}

/// Helper wrapper so callers can fuse lists of types defined in sibling
/// crates (`kb_ports::SearchHit`, etc.) without needing to add an impl of
/// `RrfItem` for those types here. Construct as `IdRef(&item)`.
pub struct IdRef<'a, T>(pub &'a T);

impl<'a, T> RrfItem for IdRef<'a, T> {
    fn id(&self) -> &str {
        // Forward to a sibling-crate-defined `rrf_id` method when present.
        // We can't know that statically, so the engine must pass a closure.
        // See `reciprocal_rank_fusion_with` for the closure-based API.
        ""
    }
}

/// Reciprocal Rank Fusion driven by a per-item id extractor. Same algorithm
/// as [`reciprocal_rank_fusion`], but takes a closure instead of relying on
/// the `RrfItem` trait. Use this when fusing types from sibling crates.
pub fn reciprocal_rank_fusion_with<T, L, I, F>(
    lists: I,
    opts: RrfOptions,
    id_of: F,
) -> Vec<RrfResult<T>>
where
    T: Clone,
    L: IntoIterator<Item = T>,
    I: IntoIterator<Item = L>,
    F: Fn(&T) -> &str,
{
    let k = opts.k.max(1) as f64;

    struct Entry<T> {
        score: f64,
        sources: Vec<usize>,
        first: T,
        insert_index: usize,
    }

    let mut scores: HashMap<String, Entry<T>> = HashMap::new();
    let mut next_insert: usize = 0;

    for (list_idx, list) in lists.into_iter().enumerate() {
        for (rank, item) in list.into_iter().enumerate() {
            let contribution = 1.0 / (k + (rank as f64) + 1.0);
            let id = id_of(&item).to_string();
            scores
                .entry(id)
                .and_modify(|e| {
                    e.score += contribution;
                    e.sources.push(list_idx);
                })
                .or_insert_with(|| Entry {
                    score: contribution,
                    sources: vec![list_idx],
                    first: item,
                    insert_index: {
                        let n = next_insert;
                        next_insert += 1;
                        n
                    },
                });
        }
    }

    let mut merged: Vec<(String, f64, Vec<usize>, T, usize)> = scores
        .into_iter()
        .map(|(id, e)| (id, e.score, e.sources, e.first, e.insert_index))
        .collect();
    merged.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.4.cmp(&b.4))
    });

    let mut out: Vec<RrfResult<T>> = merged
        .into_iter()
        .map(|(id, score, sources, first, _idx)| RrfResult {
            id,
            rrf_score: score,
            sources,
            first,
        })
        .collect();
    if let Some(limit) = opts.limit {
        out.truncate(limit as usize);
    }
    out
}

/// One fused result: the id, the accumulated RRF score, the input lists that
/// contributed it, and the original item preserved from whichever input was
/// seen first. The insert-index tie-breaker is consumed by the sort and not
/// surfaced.
pub struct RrfResult<T> {
    pub id: String,
    pub rrf_score: f64,
    /// Indices of the input lists that contributed this item.
    pub sources: Vec<usize>,
    /// The first-seen source item, preserved verbatim.
    pub first: T,
}

/// Reciprocal Rank Fusion — merges N ranked lists of items with shared string
/// ids. Score for item i across lists L_1..L_n is sum over lists of
/// 1 / (k + rank(i, L_j)). Higher is better. Order-stable for ties (preserves
/// first-seen insertion order).
pub fn reciprocal_rank_fusion<T, L, I>(lists: I, opts: RrfOptions) -> Vec<RrfResult<T>>
where
    T: RrfItem + Clone,
    L: IntoIterator<Item = T>,
    I: IntoIterator<Item = L>,
{
    let k = opts.k.max(1) as f64;

    struct Entry<T> {
        score: f64,
        sources: Vec<usize>,
        first: T,
        insert_index: usize,
    }

    let mut scores: HashMap<String, Entry<T>> = HashMap::new();
    let mut next_insert: usize = 0;

    for (list_idx, list) in lists.into_iter().enumerate() {
        for (rank, item) in list.into_iter().enumerate() {
            // +1 because rank is 0-based but the canonical RRF formula uses
            // 1-based ranks: rank 1 → 1/(k+1).
            let contribution = 1.0 / (k + (rank as f64) + 1.0);
            let id = item.id().to_string();
            scores
                .entry(id)
                .and_modify(|e| {
                    e.score += contribution;
                    e.sources.push(list_idx);
                })
                .or_insert_with(|| {
                    let e = Entry {
                        score: contribution,
                        sources: vec![list_idx],
                        first: item,
                        insert_index: next_insert,
                    };
                    next_insert += 1;
                    e
                });
        }
    }

    let mut merged: Vec<(String, f64, Vec<usize>, T, usize)> = scores
        .into_iter()
        .map(|(id, e)| (id, e.score, e.sources, e.first, e.insert_index))
        .collect();
    merged.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.4.cmp(&b.4))
    });

    let mut out: Vec<RrfResult<T>> = merged
        .into_iter()
        .map(|(id, score, sources, first, _idx)| RrfResult {
            id,
            rrf_score: score,
            sources,
            first,
        })
        .collect();
    if let Some(limit) = opts.limit {
        out.truncate(limit as usize);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Item {
        id: String,
    }
    impl RrfItem for Item {
        fn id(&self) -> &str {
            &self.id
        }
    }

    #[test]
    fn qdrants_rrf_contribution_is_one_over_two_plus_the_zero_based_rank() {
        // The numbers the whole port now agrees on: rank 0 → 0.5, rank 2 → 0.25.
        assert!((rrf_contribution(0) - 0.5).abs() < 1e-6);
        assert!((rrf_contribution(1) - 1.0 / 3.0).abs() < 1e-6);
        assert!((rrf_contribution(2) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn merges_two_lists_by_rrf_score() {
        let a = vec![
            Item { id: "a".into() },
            Item { id: "b".into() },
            Item { id: "c".into() },
        ];
        let b = vec![
            Item { id: "b".into() },
            Item { id: "a".into() },
            Item { id: "d".into() },
        ];
        let out = reciprocal_rank_fusion([a, b], RrfOptions::default());
        // a: 1/61 + 1/62; b: 1/62 + 1/61 — tie, stable on insertion order.
        // a is seen first (rank 0 of list 0) so it wins the tie.
        assert_eq!(
            out.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c", "d"]
        );
    }

    #[test]
    fn single_list_preserves_order() {
        let list = vec![
            Item { id: "a".into() },
            Item { id: "b".into() },
            Item { id: "c".into() },
        ];
        let out = reciprocal_rank_fusion([list], RrfOptions::default());
        assert_eq!(
            out.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn empty_input_produces_empty_output() {
        let out = reciprocal_rank_fusion::<Item, Vec<Item>, _>(
            std::iter::empty::<Vec<Item>>(),
            RrfOptions::default(),
        );
        assert!(out.is_empty());
        let out: Vec<RrfResult<Item>> = reciprocal_rank_fusion(
            [Vec::<Item>::new(), Vec::<Item>::new()],
            RrfOptions::default(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn limit_caps_results() {
        let list = (0..9)
            .map(|i| Item {
                id: format!("a{i}"),
            })
            .collect::<Vec<_>>();
        let out = reciprocal_rank_fusion(
            [list],
            RrfOptions {
                limit: Some(3),
                ..RrfOptions::default()
            },
        );
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn k_constant_does_not_crash() {
        let a = vec![Item { id: "x".into() }, Item { id: "y".into() }];
        let b = vec![Item { id: "y".into() }, Item { id: "x".into() }];
        let tight = reciprocal_rank_fusion(
            [a.clone(), b.clone()],
            RrfOptions {
                k: 1,
                ..RrfOptions::default()
            },
        );
        let loose = reciprocal_rank_fusion(
            [a, b],
            RrfOptions {
                k: 1000,
                ..RrfOptions::default()
            },
        );
        let mut tight_ids: Vec<&str> = tight.iter().map(|r| r.id.as_str()).collect();
        let mut loose_ids: Vec<&str> = loose.iter().map(|r| r.id.as_str()).collect();
        tight_ids.sort_unstable();
        loose_ids.sort_unstable();
        assert_eq!(tight_ids, vec!["x", "y"]);
        assert_eq!(loose_ids, vec!["x", "y"]);
    }
}
