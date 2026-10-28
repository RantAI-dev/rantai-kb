//! Fusing the text ranking with the page-image ranking, at page level.
//!
//! Weighted RRF: a page at 0-based rank `r` in a list scores `w / (k + r + 1)`,
//! with weight `1 - image_weight` for the text list and `image_weight` for the
//! image list. Measured on ViDoRe v3 HR with 2-fold cross-validation: both
//! folds chose `image_weight` 0.50–0.55 and `k = 5`, and lifted held-out page
//! nDCG@10 from 0.561 to 0.629 (`docs/eval-ab/vidore3-hr-en-image`). With
//! k = 60, the constant the text arms use, the fused list lost table questions
//! to images alone: the two lists disagree most at the top, and a small `k` is
//! what lets each list's first answers win.

use std::collections::HashMap;
use std::hash::Hash;

/// Fuse two rankings of keys. Ties keep first appearance: text order first,
/// then image order, so the result is deterministic.
#[must_use]
pub fn fuse_page_rankings<K: Clone + Eq + Hash>(
    text: &[K],
    image: &[K],
    image_weight: f32,
    k: u32,
) -> Vec<K> {
    let w = image_weight.clamp(0.0, 1.0);
    let k = f64::from(k);
    let mut score: HashMap<K, f64> = HashMap::new();
    let mut order: Vec<K> = Vec::new();
    for (list, weight) in [(text, 1.0 - f64::from(w)), (image, f64::from(w))] {
        for (r, key) in list.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let add = weight / (k + r as f64 + 1.0);
            match score.get_mut(key) {
                Some(s) => *s += add,
                None => {
                    score.insert(key.clone(), add);
                    order.push(key.clone());
                }
            }
        }
    }
    let mut ranked: Vec<(usize, K)> = order.into_iter().enumerate().collect();
    ranked.sort_by(|(ia, a), (ib, b)| {
        score[b]
            .partial_cmp(&score[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(ia.cmp(ib))
    });
    ranked.into_iter().map(|(_, key)| key).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_both_lists_agree_on_wins() {
        let text = ["a", "b", "c"];
        let image = ["x", "b", "y"];
        let fused = fuse_page_rankings(&text, &image, 0.5, 5);
        assert_eq!(fused[0], "b");
        assert_eq!(fused.len(), 5);
    }

    #[test]
    fn weight_zero_is_the_text_order_and_one_is_the_image_order() {
        let text = ["a", "b", "c"];
        let image = ["c", "b", "a"];
        assert_eq!(
            fuse_page_rankings(&text, &image, 0.0, 5),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            fuse_page_rankings(&text, &image, 1.0, 5),
            vec!["c", "b", "a"]
        );
    }

    #[test]
    fn equal_scores_keep_text_order_first() {
        let fused = fuse_page_rankings(&["t"], &["i"], 0.5, 5);
        assert_eq!(fused, vec!["t", "i"]);
    }

    /// Matches `fuse_cv.py`: rank r scores w/(k + r + 1).
    #[test]
    fn small_k_lets_each_lists_top_answer_lead() {
        // With k=5, image #1 (0.5/6 = 0.083) beats text #2 (0.5/7 = 0.071).
        let fused = fuse_page_rankings(&["t1", "t2"], &["i1", "i2"], 0.5, 5);
        assert_eq!(fused, vec!["t1", "i1", "t2", "i2"]);
    }
}
