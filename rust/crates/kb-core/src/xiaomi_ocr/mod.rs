//! Xiaomi-OCR-0 support: the table markup, the postprocessing and the merge.
//!
//! Xiaomi-OCR-0 is a small vision-language model that reads a page and answers
//! with markdown: prose, LaTeX, and OTSL — a compact table markup — for tables.
//! This module is the pure part of that pipeline: it turns the model's answer
//! into text worth indexing, and it merges the per-region answers of a page
//! into one document.
//!
//! Nothing here talks to a model or to a file. The pieces are:
//!
//! - [`otsl`]: the OTSL grid, rendered as HTML and as a markdown pipe table.
//! - [`pytext`]: Python's string rules, which the ported code depends on.
//! - [`repetition`]: is this answer the model looping?
//! - [`document`]: whole-page postprocessing (math delimiters, tables, repeats).
//! - [`region`]: per-region answers merged into one page of markdown.
//!
//! **Ported from** the postprocessing of `xiaomi-research/Xiaomi-OCR-0`
//! (Apache-2.0). See `docs/model-licences.md`.

use fancy_regex::{Captures, Regex};

/// Python's `\s` for `str` patterns.
///
/// Python matches Unicode whitespace and the ASCII file separators; Rust's
/// `\s` is ASCII-only, so the class is spelled out wherever the ported
/// patterns need it.
macro_rules! py_space {
    () => {
        r"[\t-\r \x{1c}-\x{1f}\x{85}\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}]"
    };
}

pub mod document;
pub mod otsl;
pub mod pytext;
pub mod region;
pub mod repetition;

/// Compile a pattern that is known to be valid, aborting if it is not.
///
/// The patterns are compile-time constants; a failure here is a bug in this
/// crate, not a runtime condition to recover from.
pub(crate) fn compile(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(_) => std::process::abort(),
    }
}

/// Replace every match of `pattern` with what `replacement` makes of it.
pub(crate) fn substitute(
    text: &str,
    pattern: &Regex,
    replacement: impl Fn(&Captures<'_>) -> String,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for captures in pattern.captures_iter(text) {
        let Ok(captures) = captures else { break };
        let Some(whole) = captures.get(0) else { break };
        out.push_str(&text[last..whole.start()]);
        out.push_str(&replacement(&captures));
        last = whole.end();
    }
    out.push_str(&text[last..]);
    out
}

/// The text of a capture group, or the empty string when the group is absent.
pub(crate) fn group(captures: &Captures<'_>, index: usize) -> String {
    captures
        .get(index)
        .map_or_else(String::new, |matched| matched.as_str().to_string())
}
