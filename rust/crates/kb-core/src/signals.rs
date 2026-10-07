//! Pure heuristic predicates for deciding whether a text-layer PDF extraction is
//! "good enough" to skip the layout-parser / vision-OCR chain. Cheap-first so
//! early false-returns avoid unnecessary work.
//!
//! Thresholds were chosen empirically against the 40-doc all-approaches bench;
//! see `docs/superpowers/specs/2026-04-22-smart-router-extractor-design.md` in
//! the source repo.

use fancy_regex::Regex;
use std::sync::LazyLock;

/// Tunable thresholds for `is_unpdf_sufficient`.
#[derive(Debug, Clone, Copy)]
pub struct RouterOpts {
    /// Min chars per PDF page that unpdf must produce to be trusted. Below this
    /// signals a scanned / image-only PDF whose text layer is empty or junk.
    pub min_chars_per_page: u32,
    /// Max lines that look columnar (multi-cell tabular data flattened by
    /// unpdf). If the text-layer has more than this, the doc probably has
    /// tables that need vision OCR to preserve structure.
    pub max_columnar_lines: u32,
    /// Max `$X,XXX` currency patterns. Financial tables contain many; prose
    /// rarely exceeds this. Over the threshold → route to OCR.
    pub max_currency_matches: u32,
}

impl Default for RouterOpts {
    fn default() -> Self {
        Self {
            min_chars_per_page: 300,
            max_columnar_lines: 5,
            max_currency_matches: 10,
        }
    }
}

// `\S\s{3,}\S` — runs of 3+ whitespace chars between visible tokens. Tables
// flatten to these when extracted as text layer.
static COLUMNAR_RUN: LazyLock<Regex> = LazyLock::new(|| match Regex::new(r"\S\s{3,}\S") {
    Ok(r) => r,
    Err(_) => std::process::abort(),
});

// `$X,XXX(.XX)?` — optional space after `$`, optional fractional part.
static CURRENCY: LazyLock<Regex> = LazyLock::new(|| match Regex::new(r"\$\s?[\d,]+(?:\.\d+)?") {
    Ok(r) => r,
    Err(_) => std::process::abort(),
});

/// `true` when the text contains more than `threshold` lines that look columnar
/// (2+ runs of 3+ whitespace chars between visible tokens). Short lines are
/// ignored because the regex fires on incidental formatting noise too.
pub fn has_columnar_lines(text: &str, threshold: u32) -> bool {
    let mut count: u32 = 0;
    for raw in text.split('\n') {
        let line = raw.trim();
        if line.len() < 10 {
            continue;
        }
        let matches = COLUMNAR_RUN.find_iter(line).count();
        if matches >= 2 {
            count += 1;
            if count > threshold {
                return true;
            }
        }
    }
    false
}

/// `true` when the text contains more than `threshold` currency-shaped
/// patterns. Financial tables contain many; plain prose rarely does.
pub fn has_dense_currency(text: &str, threshold: u32) -> bool {
    let count = CURRENCY.find_iter(text).count();
    count > threshold as usize
}

/// Combined gate: returns `true` only when the text is plausibly a clean
/// text-layer extraction (volume OK, not columnar, no dense currency).
pub fn is_unpdf_sufficient(text: &str, page_count: u32, opts: RouterOpts) -> bool {
    if text.is_empty() {
        return false;
    }
    let min_total = opts.min_chars_per_page.saturating_mul(page_count.max(1));
    if (text.chars().count() as u64) < u64::from(min_total) {
        return false;
    }
    if has_columnar_lines(text, opts.max_columnar_lines) {
        return false;
    }
    if has_dense_currency(text, opts.max_currency_matches) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULTS: RouterOpts = RouterOpts {
        min_chars_per_page: 300,
        max_columnar_lines: 5,
        max_currency_matches: 10,
    };

    #[test]
    fn rejects_empty() {
        assert!(!is_unpdf_sufficient("", 1, DEFAULTS));
    }

    #[test]
    fn rejects_below_volume_threshold() {
        let text = "x".repeat(200);
        assert!(!is_unpdf_sufficient(&text, 1, DEFAULTS));
    }

    #[test]
    fn accepts_at_volume_threshold_when_clean() {
        let text = "prose ".repeat(60); // 360 chars
        assert!(is_unpdf_sufficient(&text, 1, DEFAULTS));
    }

    #[test]
    fn scales_volume_with_page_count() {
        let text = "x".repeat(400); // enough for 1, not for 2
        assert!(is_unpdf_sufficient(&text, 1, DEFAULTS));
        assert!(!is_unpdf_sufficient(&text, 2, DEFAULTS));
    }

    #[test]
    fn columnar_returns_false_on_plain_prose() {
        let text = [
            "This is a paragraph about running.",
            "Another paragraph describing cats.",
            "A third line with ordinary prose content.",
        ]
        .join("\n");
        assert!(!has_columnar_lines(&text, 5));
    }

    #[test]
    fn columnar_true_above_threshold() {
        let tabular = std::iter::repeat("Cash   $29,943   $29,965")
            .take(6)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(has_columnar_lines(&tabular, 5));
    }

    #[test]
    fn columnar_ignores_short_lines() {
        let text = std::iter::repeat("A   B   C")
            .take(10)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!has_columnar_lines(&text, 5));
    }

    #[test]
    fn columnar_threshold_is_exclusive() {
        let text = std::iter::repeat("Long row   $1,000   $2,000")
            .take(5)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!has_columnar_lines(&text, 5));
    }

    #[test]
    fn columnar_gate_rejects_even_with_volume() {
        let tabular = std::iter::repeat("Cash   $29,943   $29,965")
            .take(6)
            .collect::<Vec<_>>()
            .join("\n");
        let padding = "prose text ".repeat(40);
        assert!(!is_unpdf_sufficient(
            &format!("{tabular}\n{padding}"),
            1,
            DEFAULTS
        ));
    }

    #[test]
    fn currency_false_on_plain_prose() {
        assert!(!has_dense_currency("just some words here", 10));
    }

    #[test]
    fn currency_false_below_threshold() {
        let text = std::iter::repeat("$1,234")
            .take(5)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!has_dense_currency(&text, 10));
    }

    #[test]
    fn currency_true_above_threshold() {
        let text = std::iter::repeat("$1,234")
            .take(11)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(has_dense_currency(&text, 10));
    }

    #[test]
    fn currency_matches_space_and_decimals() {
        let text = std::iter::repeat("$ 29,943.50")
            .take(11)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(has_dense_currency(&text, 10));
    }

    #[test]
    fn currency_gate_rejects_even_with_volume() {
        let padded = "description text ".repeat(40);
        let currency = std::iter::repeat("$29,943")
            .take(11)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!is_unpdf_sufficient(
            &format!("{padded} {currency}"),
            1,
            DEFAULTS
        ));
    }

    #[test]
    fn clean_prose_passes_all_gates() {
        let prose = "The quick brown fox jumps over the lazy dog. ".repeat(20);
        assert!(is_unpdf_sufficient(&prose, 1, DEFAULTS));
    }

    #[test]
    fn static_regexes_compile() {
        // If either fixed literal in this crate drifts, this test catches it
        // before `LazyLock::new` ever fires.
        let _ = &*COLUMNAR_RUN;
        let _ = &*CURRENCY;
    }
}
