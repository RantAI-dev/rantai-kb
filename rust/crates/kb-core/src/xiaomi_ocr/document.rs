//! Whole-page postprocessing: math delimiters, OTSL tables, truncated repeats.
//!
//! The model answers a page with markdown that mixes prose, LaTeX and OTSL
//! table markup. Before the text is stored it is normalised: LaTeX
//! delimiters become markdown math delimiters, OTSL blocks become HTML
//! tables, and a tail that repeats itself because the answer was cut off is
//! cut back to one period.
//!
//! **Ported from** `postprocess/document.py` of `xiaomi-research/Xiaomi-OCR-0`
//! (Apache-2.0). See `docs/model-licences.md`.

use std::sync::LazyLock;

use fancy_regex::Regex;

use super::otsl;
use super::pytext;
use super::{compile, group, substitute};

/// The OTSL tags: `<fcel>`, `<ecel>`, `<nl>`, `<lcel>`, `<ucel>`, `<xcel>`.
static RE_OTSL_TAG: LazyLock<Regex> =
    LazyLock::new(|| compile(r"<(?:fcel|ecel|nl|lcel|ucel|xcel)>"));

/// `$…\[…\]…$` — a dollar-wrapped LaTeX display block.
static RE_MATH_DOLLAR_BRACKET: LazyLock<Regex> = LazyLock::new(|| {
    compile(concat!(
        r"(?s)\$\$?",
        py_space!(),
        r"*\\\[",
        py_space!(),
        r"*(.*?)",
        py_space!(),
        r"*\\]",
        py_space!(),
        r"*\$\$?"
    ))
});

/// `$…\(…\)…$` — a dollar-wrapped LaTeX inline block.
static RE_MATH_DOLLAR_PAREN: LazyLock<Regex> = LazyLock::new(|| {
    compile(concat!(
        r"(?s)\$",
        py_space!(),
        r"*\\\(",
        py_space!(),
        r"*(.*?)",
        py_space!(),
        r"*\\\)",
        py_space!(),
        r"*\$"
    ))
});

/// `\[…\]` — a bare LaTeX display block.
static RE_MATH_BRACKET: LazyLock<Regex> = LazyLock::new(|| {
    compile(concat!(
        r"(?s)\\\[",
        py_space!(),
        r"*(.*?)",
        py_space!(),
        r"*\\]"
    ))
});

/// `\(…\)` — a bare LaTeX inline block.
static RE_MATH_PAREN: LazyLock<Regex> = LazyLock::new(|| {
    compile(concat!(
        r"(?s)\\\(",
        py_space!(),
        r"*(.*?)",
        py_space!(),
        r"*\\\)"
    ))
});

/// The blank line between two markdown blocks, kept as the separator.
static RE_BLOCK_SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| compile(concat!(r"(\n", py_space!(), r"*\n)")));

/// Convert LaTeX delimiters in whole-page markdown to markdown math delimiters.
///
/// `\[x\]` and `\(x\)` become `$$ x $$` and `$ x $`; the content keeps its
/// own text, the delimiters are the only thing that changes.
#[must_use]
pub fn postprocess_math_delimiters(markdown: &str) -> String {
    if markdown.is_empty() {
        return markdown.to_string();
    }
    let mut text = markdown.replace("\\[\\[", "\\[").replace("\\]\\]", "\\]");
    text = substitute(&text, &RE_MATH_DOLLAR_BRACKET, |captures| {
        format!(" $$ {} $$ ", group(captures, 1))
    });
    text = substitute(&text, &RE_MATH_DOLLAR_PAREN, |captures| {
        format!(" $ {} $", group(captures, 1))
    });
    text = substitute(&text, &RE_MATH_BRACKET, |captures| {
        format!(" $$ {} $$ ", group(captures, 1))
    });
    substitute(&text, &RE_MATH_PAREN, |captures| {
        format!(" $ {} $", group(captures, 1))
    })
}

/// Split markdown into blocks, keeping the blank line that separated them.
///
/// The separator is a newline, a run of whitespace and a newline: the block
/// boundary is the blank line, and the whitespace around it belongs to the
/// separator, not to either block.
fn split_markdown_blocks(markdown: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut last = 0usize;
    for matched in RE_BLOCK_SEPARATOR.find_iter(markdown) {
        let Ok(matched) = matched else { break };
        blocks.push((
            markdown[last..matched.start()].to_string(),
            matched.as_str().to_string(),
        ));
        last = matched.end();
    }
    blocks.push((markdown[last..].to_string(), String::new()));
    blocks
}

/// Convert OTSL-looking markdown blocks to HTML tables; keep the original on failure.
///
/// A block inside a fenced code block is left alone: the fence is a code
/// sample, not a table the page is showing.
#[must_use]
pub fn postprocess_otsl_tables(markdown: &str) -> String {
    if markdown.is_empty() || !RE_OTSL_TAG.is_match(markdown).unwrap_or(false) {
        return markdown.to_string();
    }

    let blocks = split_markdown_blocks(markdown);
    let mut in_fence = false;
    let mut converted = String::with_capacity(markdown.len());
    for (text, separator) in blocks {
        let stripped = pytext::strip(&text).to_string();
        let fence_count = stripped.matches("```").count();
        let mut should_skip = in_fence;
        if fence_count % 2 == 1 {
            in_fence = !in_fence;
            should_skip = true;
        }

        let mut text = text;
        if !should_skip && RE_OTSL_TAG.is_match(&stripped).unwrap_or(false) {
            let html = otsl::otsl_to_html(&stripped);
            if !html.is_empty() {
                text = html;
            }
        }
        converted.push_str(&text);
        converted.push_str(&separator);
    }
    converted
}

/// Postprocess a whole-page answer.
///
/// `convert_otsl` turns the model's OTSL tables into HTML tables; the vendor's
/// e2e recipe asks for it, the region recipe does not (a region that is a
/// table is converted on its own).
#[must_use]
pub fn postprocess_e2e_markdown(result: &str, convert_otsl: bool) -> String {
    let text = postprocess_math_delimiters(result);
    if convert_otsl {
        postprocess_otsl_tables(&text)
    } else {
        text
    }
}

/// The defaults the vendor's recipe uses.
pub const DEFAULT_MIN_TEXT_LEN: usize = 8000;
/// Default: the longest period the tail scan looks at.
pub const DEFAULT_MAX_PERIOD: usize = 200;
/// Default: the shortest period the tail scan looks at.
pub const DEFAULT_MIN_PERIOD: usize = 1;
/// Default: how many characters the repeated tail must cover.
pub const DEFAULT_MIN_REPEAT_CHARS: usize = 100;
/// Default: how many times the period must repeat.
pub const DEFAULT_MIN_REPEAT_TIMES: usize = 5;

/// Cut a truncated repeated tail back to one period plus the remainder.
///
/// The model was cut off mid-period and kept repeating itself; the answer
/// keeps one full period and the partial one, and the rest is dropped.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn clean_truncated_repeats(
    text: &str,
    min_text_len: usize,
    max_period: usize,
    min_period: usize,
    min_repeat_chars: usize,
    min_repeat_times: usize,
) -> String {
    let chars: Vec<char> = text.chars().collect();
    let length = chars.len();
    if length < min_text_len {
        return text.to_string();
    }

    let max_period = max_period.min(length.saturating_sub(1));
    for unit_length in min_period..=max_period {
        if length < unit_length {
            break;
        }
        if chars[length - 1] != chars[length - 1 - unit_length] {
            continue;
        }

        let mut match_length = 1usize;
        let mut index = length as isize - 2;
        while index >= unit_length as isize
            && chars[index as usize] == chars[(index - unit_length as isize) as usize]
        {
            match_length += 1;
            index -= 1;
        }

        let total_length = match_length + unit_length;
        let repeat_times = total_length / unit_length;
        let tail_length = total_length % unit_length;

        if repeat_times >= min_repeat_times && total_length >= min_repeat_chars {
            let head: String = chars[..length - total_length + unit_length]
                .iter()
                .collect();
            let tail: String = chars[length - tail_length..].iter().collect();
            return format!("{head}{tail}");
        }
    }
    text.to_string()
}

/// [`clean_truncated_repeats`] with the vendor's defaults.
#[must_use]
pub fn clean_truncated_repeats_default(text: &str) -> String {
    clean_truncated_repeats(
        text,
        DEFAULT_MIN_TEXT_LEN,
        DEFAULT_MAX_PERIOD,
        DEFAULT_MIN_PERIOD,
        DEFAULT_MIN_REPEAT_CHARS,
        DEFAULT_MIN_REPEAT_TIMES,
    )
}

/// The vendor's final step: cut the repeated tail, then postprocess.
#[must_use]
pub fn finalize_document(text: &str) -> String {
    postprocess_e2e_markdown(&clean_truncated_repeats_default(text), true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_display_block_becomes_a_math_block() {
        assert_eq!(
            postprocess_math_delimiters("before \\[x = 1\\] after"),
            "before  $$ x = 1 $$  after"
        );
    }

    #[test]
    fn an_inline_block_becomes_inline_math() {
        assert_eq!(postprocess_math_delimiters("a \\(y\\) b"), "a  $ y $ b");
    }

    #[test]
    fn a_dollar_wrapped_block_becomes_a_math_block() {
        assert_eq!(postprocess_math_delimiters("$$\\[z\\]$$"), " $$ z $$ ");
    }

    #[test]
    fn a_double_bracket_is_unwrapped_first() {
        assert_eq!(postprocess_math_delimiters("\\[\\[x\\]\\]"), " $$ x $$ ");
    }

    #[test]
    fn an_otsl_block_becomes_an_html_table() {
        let markdown = "intro\n\n<fcel>A<fcel>B<nl>\n\ntail";
        assert_eq!(
            postprocess_otsl_tables(markdown),
            "intro\n\n<table><tr><td>A</td><td>B</td></tr></table>\n\ntail"
        );
    }

    #[test]
    fn an_otsl_block_inside_a_fence_is_left_alone() {
        let markdown = "```\n\n<fcel>A<fcel>B<nl>\n\n```";
        assert_eq!(postprocess_otsl_tables(markdown), markdown);
    }

    #[test]
    fn a_long_repeated_tail_is_cut_back_to_one_period() {
        let text = "ab".repeat(5000);
        assert_eq!(clean_truncated_repeats_default(&text), "ab");
    }

    #[test]
    fn a_repeated_tail_is_cut_back_to_one_period() {
        let mut text = "x".repeat(9000);
        text.push_str(&"y".repeat(1000));
        let cleaned = clean_truncated_repeats_default(&text);
        assert!(cleaned.len() < text.len());
        assert!(cleaned.starts_with(&"x".repeat(9000)));
    }
}
