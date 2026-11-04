//! Page furniture: running headers, footers and page numbers.
//!
//! Report PDFs repeat a page number and a running title ("Joint Employment
//! Report 2025", "European Commission") at the top or bottom of nearly every
//! page. In the ViDoRe v3 HR corpus that is 1,953 of 54,741 lines across 13 of
//! 14 reports. Left in, every chunk of a document carries the same words:
//! keyword search scores them, the embedder spends its budget on them, and the
//! reranker reads them.
//!
//! The rule is deliberately narrow, so it never removes content:
//!
//! - Only the first and last [`FurnitureOptions::zone_lines`] non-empty lines
//!   of a page are candidates (fewer on a short page: at most a quarter of its
//!   lines). A repeated sentence in the body stays.
//! - A candidate is furniture when its *shape* (whitespace collapsed, digits
//!   replaced by `#`, lower-cased) appears in the zone of at least
//!   `max(min_pages, min_share × pages)` pages. So "Page 3" and "Page 47" are
//!   one shape, and a heading that happens to repeat on two pages is not.
//! - Long lines are never furniture ([`FurnitureOptions::max_line_chars`]).
//! - Short documents (fewer than `min_pages + 1` pages) are left alone.
//!
//! Removed lines are dropped whole; every kept line is byte-identical.

use std::collections::{HashMap, HashSet};

/// Tunables for [`strip_page_furniture`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FurnitureOptions {
    /// Non-empty lines at the top and at the bottom of a page that may be
    /// furniture.
    pub zone_lines: usize,
    /// A shape must repeat on at least this many pages...
    pub min_pages: usize,
    /// ...and on at least this share of the document's pages.
    pub min_share: f32,
    /// Lines longer than this (in characters, after trimming) are never removed.
    pub max_line_chars: usize,
}

impl Default for FurnitureOptions {
    fn default() -> Self {
        Self {
            zone_lines: 3,
            min_pages: 3,
            min_share: 0.2,
            max_line_chars: 120,
        }
    }
}

/// What [`strip_page_furniture`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FurnitureReport {
    pub lines_removed: usize,
    /// The shapes judged to be furniture, most frequent first.
    pub shapes: Vec<(String, usize)>,
}

/// The shape a line is compared by: whitespace collapsed, digits as `#`,
/// lower-case.
fn shape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last_space = false;
    let mut last_digit = false;
    for c in line.trim().chars() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
            last_digit = false;
        } else if c.is_ascii_digit() {
            if !last_digit {
                out.push('#');
            }
            last_digit = true;
            last_space = false;
        } else {
            out.extend(c.to_lowercase());
            last_space = false;
            last_digit = false;
        }
    }
    out
}

/// Indices (into `lines`) of the zone: the first and last `k` non-empty lines,
/// with `k` capped at a quarter of the page's non-empty lines (at least 1), so
/// on a short page the zone never swallows the body.
fn zone(lines: &[&str], k: usize) -> Vec<usize> {
    let non_empty: Vec<usize> = (0..lines.len())
        .filter(|&i| !lines[i].trim().is_empty())
        .collect();
    let k = k.min((non_empty.len() / 4).max(1));
    let mut idx: Vec<usize> = non_empty.iter().take(k).copied().collect();
    idx.extend(non_empty.iter().rev().take(k).copied());
    idx.sort_unstable();
    idx.dedup();
    idx
}

/// Remove running headers, footers and page numbers from `(page, text)` pairs.
///
/// Returns the cleaned pages (same order, same page numbers) and a report.
#[must_use]
pub fn strip_page_furniture(
    pages: &[(u32, String)],
    opts: FurnitureOptions,
) -> (Vec<(u32, String)>, FurnitureReport) {
    if pages.len() <= opts.min_pages {
        return (pages.to_vec(), FurnitureReport::default());
    }
    let eligible = |l: &str| {
        let t = l.trim();
        !t.is_empty() && t.chars().count() <= opts.max_line_chars
    };

    // Count, per shape, how many pages have it in their zone.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_, text) in pages {
        let lines: Vec<&str> = text.split('\n').collect();
        let shapes: HashSet<String> = zone(&lines, opts.zone_lines)
            .into_iter()
            .filter(|&i| eligible(lines[i]))
            .map(|i| shape(lines[i]))
            .collect();
        for s in shapes {
            *counts.entry(s).or_default() += 1;
        }
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let by_share = (opts.min_share * pages.len() as f32).ceil() as usize;
    let threshold = opts.min_pages.max(by_share);
    let furniture: HashSet<&String> = counts
        .iter()
        .filter(|(_, c)| **c >= threshold)
        .map(|(s, _)| s)
        .collect();
    if furniture.is_empty() {
        return (pages.to_vec(), FurnitureReport::default());
    }

    let mut removed = 0usize;
    let cleaned = pages
        .iter()
        .map(|(page, text)| {
            let lines: Vec<&str> = text.split('\n').collect();
            let drop: HashSet<usize> = zone(&lines, opts.zone_lines)
                .into_iter()
                .filter(|&i| eligible(lines[i]) && furniture.contains(&shape(lines[i])))
                .collect();
            if drop.is_empty() {
                return (*page, text.clone());
            }
            removed += drop.len();
            let kept: Vec<&str> = lines
                .iter()
                .enumerate()
                .filter(|(i, _)| !drop.contains(i))
                .map(|(_, l)| *l)
                .collect();
            (*page, kept.join("\n").trim_matches('\n').to_string())
        })
        .collect();

    let mut shapes: Vec<(String, usize)> = furniture
        .into_iter()
        .map(|s| (s.clone(), counts[s]))
        .collect();
    shapes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    (
        cleaned,
        FurnitureReport {
            lines_removed: removed,
            shapes,
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn doc(n: u32, body: impl Fn(u32) -> String) -> Vec<(u32, String)> {
        (0..n).map(|p| (p, body(p))).collect()
    }

    #[test]
    fn running_title_and_page_numbers_are_removed() {
        let pages = doc(10, |p| {
            format!(
                "Joint Employment Report 2025\n\nBody paragraph {p} about wages.\nMore text.\n\n{}",
                p + 1
            )
        });
        let (out, report) = strip_page_furniture(&pages, FurnitureOptions::default());
        assert_eq!(report.lines_removed, 20);
        assert_eq!(out[3].1, "Body paragraph 3 about wages.\nMore text.");
        assert_eq!(out[3].0, 3);
    }

    /// A distinct word per page, so body lines differ even after digits fold.
    fn word(p: u32) -> &'static str {
        [
            "oak", "elm", "ash", "yew", "fir", "bay", "box", "fig", "lime", "pine",
        ][p as usize % 10]
    }

    #[test]
    fn a_repeated_line_in_the_body_stays() {
        let pages = doc(10, |p| {
            let w = word(p);
            format!(
                "Chapter {p}\nintro {w}\nmore {w}\nSee the annex for details.\nmid {w}\nmid2 {w}\nmid3 {w}\nend {p}"
            )
        });
        let (out, report) = strip_page_furniture(&pages, FurnitureOptions::default());
        assert!(out[0].1.contains("See the annex for details."));
        // 8 lines: a zone of 2 at each end. "Chapter #" and "end #" go.
        assert_eq!(report.lines_removed, 20);
        assert!(out[4].1.starts_with("intro fir"));
    }

    #[test]
    fn short_documents_and_rare_lines_are_untouched() {
        let pages = doc(3, |p| format!("Title\nbody {p}\n{p}"));
        let (out, report) = strip_page_furniture(&pages, FurnitureOptions::default());
        assert_eq!(out, pages);
        assert_eq!(report.lines_removed, 0);

        // On 20 pages, a line on 3 of them is under the 20% share (4 pages).
        let pages = doc(20, |p| {
            let head = if p < 3 { "Chapter summary" } else { "x" };
            format!("{head}\nunique body text about {}", word(p))
        });
        let (out, _) = strip_page_furniture(&pages, FurnitureOptions::default());
        assert!(out[0].1.starts_with("Chapter summary"));
    }

    #[test]
    fn long_lines_are_never_furniture() {
        let long = "a".repeat(200);
        let pages = doc(10, |p| format!("{long}\nbody {}", word(p)));
        let (out, report) = strip_page_furniture(&pages, FurnitureOptions::default());
        assert_eq!(report.lines_removed, 0);
        assert_eq!(out, pages);
    }

    #[test]
    fn shape_folds_numbers_case_and_spaces() {
        assert_eq!(shape("  Page 12   of 40 "), "page # of #");
        assert_eq!(shape("PAGE 3 of 4"), "page # of #");
        assert_eq!(shape("2024-25"), "#-#");
    }
}
