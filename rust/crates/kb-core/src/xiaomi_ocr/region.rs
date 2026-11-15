//! Region assembly: turn a page's layout regions into one markdown document.
//!
//! A page is read in two steps by the vendor's pipeline: a layout detector
//! says which parts of the page are text, tables and formulas, and the model
//! is asked to read each region. This module is the second half — it formats
//! every region, merges a formula with its number, joins a word split across
//! two blocks, and lays the blocks out in reading order.
//!
//! **Ported from** `postprocess/region.py` of `xiaomi-research/Xiaomi-OCR-0`
//! (Apache-2.0). See `docs/model-licences.md`.

use std::collections::HashSet;
use std::sync::LazyLock;

use fancy_regex::Regex;
use serde::{Deserialize, Serialize};

use super::pytext;
use super::{compile, group, substitute};

/// A region of a page: where it is, and what the OCR read in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutRegion {
    /// The region's order on the page.
    #[serde(default)]
    pub index: usize,
    /// The label the layout detector gave the region.
    pub label: String,
    /// The region's box, normalised to 0–1000.
    #[serde(default, rename = "bbox_2d")]
    pub bbox: Vec<f64>,
    /// What the OCR read in the region.
    #[serde(
        default,
        alias = "merged",
        alias = "vlm_merged",
        alias = "llm_merged",
        alias = "text"
    )]
    pub content: String,
}

/// The four kinds of region the merge understands.
const LABEL_VISUALIZATION_MAPPING: [(&str, &[&str]); 5] = [
    ("image", &["chart", "image"]),
    (
        "text",
        &[
            "abstract",
            "algorithm",
            "content",
            "doc_title",
            "figure_title",
            "paragraph_title",
            "reference_content",
            "text",
            "vertical_text",
            "vision_footnote",
            "seal",
            "formula_number",
        ],
    ),
    ("table", &["table"]),
    ("formula", &["display_formula", "inline_formula"]),
    (
        "abandon",
        &[
            "header",
            "footer",
            "number",
            "footnote",
            "aside_text",
            "reference",
            "footer_image",
            "header_image",
        ],
    ),
];

/// An inline formula: `$…$` that is not part of a `$$…$$` block.
static RE_INLINE_FORMULA: LazyLock<Regex> = LazyLock::new(|| {
    compile(concat!(
        r"(?<!\$)",
        r"\$",
        py_space!(),
        r"*((?:[^$\\]|\\.)+?)",
        py_space!(),
        r"*",
        r"\$",
        r"(?!\$)"
    ))
});

/// Leading markdown hashes, as in `### Title`.
static RE_LEADING_HASHES: LazyLock<Regex> =
    LazyLock::new(|| compile(concat!(r"^#+", py_space!(), r"*")));

/// A newline that is not part of a blank line.
static RE_LONE_NEWLINE: LazyLock<Regex> = LazyLock::new(|| compile(r"(?<!\n)\n(?!\n)"));

/// The map from the detector's labels to the kinds the merge understands.
fn map_label(label: &str) -> &str {
    for (mapped, labels) in LABEL_VISUALIZATION_MAPPING {
        if labels.contains(&label) {
            return mapped;
        }
    }
    label
}

/// One region of the page, once it has been formatted.
#[derive(Debug, Clone, PartialEq)]
struct Block {
    /// The region's order on the page.
    index: usize,
    /// The kind of region: `text`, `table` or `formula`.
    label: String,
    /// The label the layout detector gave the region.
    native_label: String,
    /// The region's text, formatted.
    content: String,
    /// The region's box, normalised to 0–1000.
    bbox: Vec<f64>,
}

/// Turn a page's regions into markdown.
///
/// The regions are read in their recorded order. Images and furniture
/// (headers, footers, page numbers) are dropped: they are not the page's
/// content. What is left is joined with a blank line.
#[must_use]
pub fn merge_page_to_markdown(regions: &[LayoutRegion]) -> String {
    if regions.is_empty() {
        return String::new();
    }

    let mut sorted: Vec<&LayoutRegion> = regions.iter().collect();
    sorted.sort_by_key(|region| region.index);

    let mut processed: Vec<Block> = Vec::new();
    for region in sorted {
        let native_label = region.label.as_str();
        let mapped_label = map_label(native_label).to_string();
        if mapped_label == "image" || mapped_label == "abandon" {
            continue;
        }

        let formatted = format_content(&region.content, &mapped_label, native_label);
        if pytext::strip(&formatted).is_empty() {
            continue;
        }
        processed.push(Block {
            index: processed.len(),
            label: mapped_label,
            native_label: native_label.to_string(),
            content: formatted,
            bbox: region.bbox.clone(),
        });
    }

    let processed = merge_formula_numbers(processed);
    let processed = merge_text_blocks(processed);
    let processed = format_bullet_points(processed);

    processed
        .iter()
        .filter(|block| !block.content.is_empty())
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Format one region's text for the page.
fn format_content(content: &str, label: &str, native_label: &str) -> String {
    let mut content = match label {
        "table" => {
            if content.starts_with("<table") && content.ends_with("</table>") {
                pytext::strip(content).to_string()
            } else {
                clean_content(content)
            }
        }
        "formula" => {
            if content.starts_with("$$") && content.ends_with("$$") {
                pytext::strip(content).to_string()
            } else {
                clean_content(content)
            }
        }
        _ => clean_content(content),
    };

    if native_label == "doc_title" {
        content = format!("# {}", strip_leading_hashes(&content));
    } else if native_label == "paragraph_title" {
        if content.starts_with("- ") || content.starts_with("* ") {
            content = pytext::lstrip(&content[2..]).to_string();
        }
        content = format!("## {}", pytext::lstrip(&strip_leading_hashes(&content)));
    }

    if label == "formula" {
        if content.starts_with("$$") || content.starts_with("\\[") || content.starts_with("\\(") {
            content = pytext::strip(&content[2..]).to_string();
        }
        if content.ends_with("$$") || content.ends_with("\\]") || content.ends_with("\\)") {
            content = pytext::strip(&content[..content.len() - 2]).to_string();
        }
        content = format!("$$\n{content}\n$$");
    }

    if label == "text" {
        if content.starts_with("```") && !content.ends_with("```") {
            content.push_str("\n```");
        }
        if content.starts_with('·') || content.starts_with('•') || content.starts_with("* ") {
            content = format!("- {}", pytext::lstrip(&content[1..]));
        }
        if let Some((symbol, marker_len)) = bracketed_list_marker(&content) {
            if let Some(rest) = list_rest(&content[marker_len..]) {
                content = format!("({symbol}) {}", pytext::lstrip(rest));
            }
        } else if let Some((symbol, separator, marker_len)) = plain_list_marker(&content) {
            if let Some(rest) = list_rest(&content[marker_len..]) {
                let separator = if separator == '）' { ')' } else { separator };
                content = format!("{symbol}{separator} {}", pytext::lstrip(rest));
            }
        }
        content = substitute(&content, &RE_LONE_NEWLINE, |_| "\n\n".to_string());
    }

    content
}

/// The text after a list marker, when the block ends with one.
///
/// A block whose first line ends before a second line is not a list item: the
/// marker only counts when the block is one line, or one line and a newline.
fn list_rest(content: &str) -> Option<&str> {
    match content.find('\n') {
        None => Some(content),
        Some(index) if index == content.len() - 1 => Some(&content[..index]),
        Some(_) => None,
    }
}

/// The symbol of a `(1)` style list marker, and the length of the marker.
fn bracketed_list_marker(content: &str) -> Option<(String, usize)> {
    let mut chars = content.chars();
    let opening = chars.next()?;
    if opening != '(' && opening != '（' {
        return None;
    }
    let rest: String = chars.collect();
    let symbol_len = symbol_len(&rest)?;
    let symbol = &rest[..symbol_len];
    let after = &rest[symbol_len..];
    let closing = after.chars().next()?;
    if closing != ')' && closing != '）' {
        return None;
    }
    let marker_len = opening.len_utf8() + symbol_len + closing.len_utf8();
    Some((symbol.to_string(), marker_len))
}

/// The symbol of a `1.` style list marker, its separator and the marker's length.
fn plain_list_marker(content: &str) -> Option<(String, char, usize)> {
    let symbol_len = symbol_len(content)?;
    let symbol = &content[..symbol_len];
    let after = &content[symbol_len..];
    let separator = after.chars().next()?;
    if separator != '.' && separator != ')' && separator != '）' {
        return None;
    }
    Some((
        symbol.to_string(),
        separator,
        symbol_len + separator.len_utf8(),
    ))
}

/// The length of a list symbol: a run of digits, or a single letter.
fn symbol_len(text: &str) -> Option<usize> {
    let first = text.chars().next()?;
    if first.is_ascii_digit() {
        let len = text
            .char_indices()
            .take_while(|(_, ch)| ch.is_ascii_digit())
            .map(|(index, ch)| index + ch.len_utf8())
            .last()?;
        Some(len)
    } else if first.is_ascii_alphabetic() {
        Some(first.len_utf8())
    } else {
        None
    }
}

/// Remove the markdown hashes in front of a heading.
fn strip_leading_hashes(content: &str) -> String {
    substitute(content, &RE_LEADING_HASHES, |_| String::new())
}

/// Clean a region's text: drop tab escapes, collapse runaway punctuation,
/// remove a repeated tail and normalise inline formulas.
fn clean_content(content: &str) -> String {
    let mut text = strip_leading_tab_escapes(content);
    text = strip_trailing_tab_escapes(&text);
    text = collapse_repeats(&text, ".", 3);
    text = collapse_repeats(&text, "·", 3);
    text = collapse_repeats(&text, "_", 3);
    text = collapse_repeats(&text, "\\_", 3);
    if pytext::char_len(&text) >= 2048 {
        text = clean_repeated_content(&text);
    }
    text = normalize_inline_formula(&text);
    pytext::strip(&text).to_string()
}

/// Drop the literal `\t` escapes at the front of the text.
fn strip_leading_tab_escapes(content: &str) -> String {
    let mut start = 0usize;
    while content[start..].starts_with("\\t") {
        start += 2;
    }
    pytext::lstrip(&content[start..]).to_string()
}

/// Drop the literal `\t` escapes at the end of the text.
fn strip_trailing_tab_escapes(content: &str) -> String {
    let body = content.strip_suffix('\n').unwrap_or(content);
    let mut end = body.len();
    while end >= 2 && body[..end].ends_with("\\t") {
        end -= 2;
    }
    let mut out = String::with_capacity(content.len());
    out.push_str(&body[..end]);
    if content.ends_with('\n') {
        out.push('\n');
    }
    pytext::rstrip(&out).to_string()
}

/// Collapse a run of one repeated unit to `keep` copies.
///
/// The model sometimes falls into a punctuation loop — five dots, a row of
/// underscores — which carries no meaning and must not reach the index.
fn collapse_repeats(text: &str, unit: &str, keep: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < text.len() {
        if text[index..].starts_with(unit) {
            let mut count = 0usize;
            let mut cursor = index;
            while text[cursor..].starts_with(unit) {
                count += 1;
                cursor += unit.len();
            }
            if count > keep {
                for _ in 0..keep {
                    out.push_str(unit);
                }
            } else {
                out.push_str(&text[index..cursor]);
            }
            index = cursor;
        } else {
            let ch = text[index..].chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

/// The first place where a unit repeats itself, and the unit that repeats.
///
/// The leftmost place wins; at that place the shortest unit wins. The unit is
/// at least `min_unit_len` characters and repeats at least `min_repeats`
/// times.
fn find_consecutive_repeat(
    text: &str,
    min_unit_len: usize,
    min_repeats: usize,
) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let length = chars.len();
    if length < min_unit_len * min_repeats {
        return None;
    }
    let max_unit_len = length / min_repeats;
    if max_unit_len < min_unit_len {
        return None;
    }

    let mut best: Option<(usize, usize)> = None;
    for unit_length in min_unit_len..=max_unit_len {
        let limit = best.map(|(position, _)| position);
        let Some(position) = first_repeat(&chars, unit_length, min_repeats, limit) else {
            continue;
        };
        if best.is_none_or(|(best_position, _)| position < best_position) {
            best = Some((position, unit_length));
        }
        if best.is_some_and(|(position, _)| position == 0) {
            break;
        }
    }
    best
}

/// The smallest position where the unit of `unit_length` repeats `min_repeats` times.
fn first_repeat(
    chars: &[char],
    unit_length: usize,
    min_repeats: usize,
    limit: Option<usize>,
) -> Option<usize> {
    let length = chars.len();
    if min_repeats < 2 || unit_length == 0 {
        return None;
    }
    // `run[j]` is how many equalities in a row end at `j`, at the unit's stride.
    let mut run = vec![0u32; length];
    for j in unit_length..length {
        if chars[j] == chars[j - unit_length] {
            run[j] = run[j - unit_length] + 1;
        }
    }

    let needed = min_repeats - 1;
    let offset = (needed - 1) * unit_length;
    let mut j = offset;
    while j < length {
        if let Some(limit) = limit {
            if j - offset > limit {
                return None;
            }
        }
        if run[j] >= needed as u32 {
            return Some(j - offset);
        }
        j += 1;
    }
    None
}

/// Remove a repeated tail from a region's text.
fn clean_repeated_content(content: &str) -> String {
    const MIN_LEN: usize = 10;
    const MIN_REPEATS: usize = 10;
    const LINE_THRESHOLD: usize = 10;

    let stripped = pytext::strip(content);
    if stripped.is_empty() {
        return content.to_string();
    }

    if pytext::char_len(stripped) > MIN_LEN * MIN_REPEATS {
        if let Some((position, unit_length)) =
            find_consecutive_repeat(stripped, MIN_LEN, MIN_REPEATS)
        {
            let chars: Vec<char> = stripped.chars().collect();
            let unit: String = chars[position..position + unit_length].iter().collect();
            let head: String = chars[..position].iter().collect();
            return format!("{head}{unit}");
        }
    }

    let lines: Vec<&str> = content
        .split('\n')
        .map(pytext::strip)
        .filter(|line| !line.is_empty())
        .collect();
    let total_lines = lines.len();
    if total_lines < LINE_THRESHOLD {
        return content.to_string();
    }

    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in &lines {
        *counts.entry(line).or_insert(0) += 1;
    }
    let mut common: Option<(&str, usize)> = None;
    for line in &lines {
        let count = counts[line];
        if common.is_none_or(|(_, best)| count > best) {
            common = Some((line, count));
        }
    }
    let Some((common, count)) = common else {
        return content.to_string();
    };
    if count < LINE_THRESHOLD || (count as f64 / total_lines as f64) < 0.8 {
        return content.to_string();
    }

    for (position, line) in lines.iter().enumerate() {
        if *line != common {
            continue;
        }
        let consecutive = lines[position..(position + 3).min(lines.len())]
            .iter()
            .filter(|candidate| **candidate == common)
            .count();
        if consecutive >= 3 {
            let original_lines: Vec<&str> = content.split('\n').collect();
            let mut non_empty = 0usize;
            for (index, original) in original_lines.iter().enumerate() {
                if !pytext::strip(original).is_empty() {
                    non_empty += 1;
                    if non_empty == position + 1 {
                        return original_lines[..=index].join("\n");
                    }
                }
            }
        }
        break;
    }
    content.to_string()
}

/// Give an inline formula a space on either side when it touches a word.
fn normalize_inline_formula(content: &str) -> String {
    if !content.contains('$') {
        return content.to_string();
    }
    let mut parts = String::with_capacity(content.len());
    let mut last_end = 0usize;
    for captures in RE_INLINE_FORMULA.captures_iter(content) {
        let Ok(captures) = captures else { break };
        let Some(whole) = captures.get(0) else { break };
        let formula = group(&captures, 1);
        let formula = pytext::strip(&formula).to_string();
        if formula.is_empty() {
            continue;
        }
        let start = whole.start();
        let end = whole.end();
        let before = &content[last_end..start];
        parts.push_str(before);
        if before
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_alphanumeric())
        {
            parts.push(' ');
        }
        parts.push('$');
        parts.push_str(&formula);
        parts.push('$');
        if content[end..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_alphanumeric())
        {
            parts.push(' ');
        }
        last_end = end;
    }
    if parts.is_empty() {
        return content.to_string();
    }
    parts.push_str(&content[last_end..]);
    parts
}

/// Whether a word split across two blocks is one word.
///
/// The reference implementation asks `wordfreq` whether the joined word is a
/// common English word; that package is not part of the deployment, so the
/// shape check below is the path that runs: a short lowercase word with no
/// doubled hyphen or apostrophe.
fn is_likely_valid_merged_word(merged_word: &str) -> bool {
    let token = pytext::strip(merged_word).to_lowercase();
    if token.is_empty() {
        return false;
    }
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < 3 || chars.len() > 31 {
        return false;
    }
    if !chars[0].is_ascii_lowercase() {
        return false;
    }
    if !chars[1..]
        .iter()
        .all(|ch| ch.is_ascii_lowercase() || *ch == '\'' || *ch == '-')
    {
        return false;
    }
    if token.contains("--") || token.contains("''") {
        return false;
    }
    true
}

/// Clean a formula's number: `(3)` and `（3）` both mean `3`.
fn clean_formula_number(number_content: &str) -> String {
    let number = pytext::strip(number_content);
    if number.starts_with('(') && number.ends_with(')') && number.chars().count() >= 2 {
        return number[1..number.len() - 1].to_string();
    }
    if number.starts_with('（') && number.ends_with('）') && number.chars().count() >= 2 {
        return number[1..number.len() - 1].to_string();
    }
    number.to_string()
}

/// Join a formula with the number that belongs to it.
fn merge_formula_numbers(results: Vec<Block>) -> Vec<Block> {
    if results.is_empty() {
        return results;
    }
    let mut merged_results: Vec<Block> = Vec::new();
    let mut skip_indices: HashSet<usize> = HashSet::new();
    for index in 0..results.len() {
        if skip_indices.contains(&index) {
            continue;
        }
        let block = &results[index];
        if block.native_label == "formula_number" {
            if let Some(next_block) = results.get(index + 1) {
                if next_block.label == "formula" {
                    let number = clean_formula_number(&block.content);
                    let mut merged_block = next_block.clone();
                    if let Some(merged) = tag_formula(&next_block.content, &number) {
                        merged_block.content = merged;
                    }
                    merged_results.push(merged_block);
                    skip_indices.insert(index + 1);
                    continue;
                }
            }
            continue;
        }
        if block.label == "formula" {
            if let Some(next_block) = results.get(index + 1) {
                if next_block.native_label == "formula_number" {
                    let number = clean_formula_number(&next_block.content);
                    let mut merged_block = block.clone();
                    if let Some(merged) = tag_formula(&block.content, &number) {
                        merged_block.content = merged;
                    }
                    merged_results.push(merged_block);
                    skip_indices.insert(index + 1);
                    continue;
                }
            }
        }
        merged_results.push(block.clone());
    }
    for (index, block) in merged_results.iter_mut().enumerate() {
        block.index = index;
    }
    merged_results
}

/// Put a formula's number into a `\tag{}` before the closing `$$`.
fn tag_formula(formula_content: &str, number: &str) -> Option<String> {
    if !formula_content.ends_with("\n$$") {
        return None;
    }
    let head = &formula_content[..formula_content.len() - 3];
    Some(format!("{head} \\tag{{{number}}}\n$$"))
}

/// Join a word that the layout split across two blocks.
fn merge_text_blocks(results: Vec<Block>) -> Vec<Block> {
    if results.is_empty() {
        return results;
    }
    let mut merged_results: Vec<Block> = Vec::new();
    let mut skip_indices: HashSet<usize> = HashSet::new();
    for index in 0..results.len() {
        if skip_indices.contains(&index) {
            continue;
        }
        let block = &results[index];
        if block.label != "text" {
            merged_results.push(block.clone());
            continue;
        }
        let content_stripped = block.content.trim_end_matches(pytext::is_space);
        if content_stripped.is_empty() || !content_stripped.ends_with('-') {
            merged_results.push(block.clone());
            continue;
        }

        let mut merged = false;
        for (next_index, next_block) in results.iter().enumerate().skip(index + 1) {
            if next_block.label == "text" {
                let next_content = &next_block.content;
                let next_stripped = next_content.trim_start_matches(pytext::is_space);
                if let Some(first) = next_stripped.chars().next() {
                    if first.is_lowercase() {
                        let head = &content_stripped[..content_stripped.len() - 1];
                        let words_before = pytext::split_whitespace(head);
                        let next_words = pytext::split_whitespace(next_stripped);
                        if let (Some(last_before), Some(first_after)) =
                            (words_before.last(), next_words.first())
                        {
                            let merged_word = format!("{last_before}{first_after}");
                            if is_likely_valid_merged_word(&merged_word) {
                                let mut merged_block = block.clone();
                                merged_block.content = format!("{head}{next_stripped}");
                                merged_results.push(merged_block);
                                skip_indices.insert(next_index);
                                merged = true;
                            }
                        }
                        // Only a continuation that starts with a lowercase
                        // letter ends the search: a heading or a caption in
                        // between does not stop the word from being split.
                        break;
                    }
                }
            }
        }
        if !merged {
            merged_results.push(block.clone());
        }
    }
    for (index, block) in merged_results.iter_mut().enumerate() {
        block.index = index;
    }
    merged_results
}

/// Mark a block as a bullet when it sits between two bullets in one column.
fn format_bullet_points(results: Vec<Block>) -> Vec<Block> {
    const LEFT_ALIGN_THRESHOLD: f64 = 10.0;
    if results.len() < 3 {
        return results;
    }
    let mut results = results;
    for index in 1..(results.len() - 1) {
        let current = &results[index];
        let previous = &results[index - 1];
        let next = &results[index + 1];
        if current.native_label != "text" {
            continue;
        }
        if previous.native_label != "text" || next.native_label != "text" {
            continue;
        }
        if current.content.starts_with("- ") {
            continue;
        }
        if !(previous.content.starts_with("- ") && next.content.starts_with("- ")) {
            continue;
        }
        if current.bbox.is_empty() || previous.bbox.is_empty() || next.bbox.is_empty() {
            continue;
        }
        let current_left = current.bbox[0];
        let previous_left = previous.bbox[0];
        let next_left = next.bbox[0];
        if (current_left - previous_left).abs() <= LEFT_ALIGN_THRESHOLD
            && (current_left - next_left).abs() <= LEFT_ALIGN_THRESHOLD
        {
            results[index].content = format!("- {}", results[index].content);
        }
    }
    results
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn region(index: usize, label: &str, content: &str) -> LayoutRegion {
        LayoutRegion {
            index,
            label: label.to_string(),
            bbox: vec![100.0, 100.0, 200.0, 200.0],
            content: content.to_string(),
        }
    }

    #[test]
    fn a_page_of_text_keeps_its_order() {
        let regions = vec![region(0, "text", "first"), region(1, "text", "second")];
        assert_eq!(merge_page_to_markdown(&regions), "first\n\nsecond");
    }

    #[test]
    fn furniture_is_dropped() {
        let regions = vec![
            region(0, "header", "running head"),
            region(1, "text", "body"),
            region(2, "footer", "page 1"),
        ];
        assert_eq!(merge_page_to_markdown(&regions), "body");
    }

    #[test]
    fn a_document_title_becomes_a_heading() {
        let regions = vec![region(0, "doc_title", "The Title")];
        assert_eq!(merge_page_to_markdown(&regions), "# The Title");
    }

    #[test]
    fn a_formula_takes_its_number() {
        let regions = vec![
            region(0, "display_formula", "$$\nx = 1\n$$"),
            region(1, "formula_number", "(3)"),
        ];
        assert_eq!(merge_page_to_markdown(&regions), "$$\nx = 1 \\tag{3}\n$$");
    }

    #[test]
    fn a_word_split_across_blocks_is_joined() {
        let regions = vec![
            region(0, "text", "the analy-"),
            region(1, "text", "sis is here"),
        ];
        assert_eq!(merge_page_to_markdown(&regions), "the analysis is here");
    }

    #[test]
    fn a_split_that_is_not_a_word_is_left_alone() {
        let regions = vec![region(0, "text", "the 1-"), region(1, "text", "2 result")];
        assert_eq!(merge_page_to_markdown(&regions), "the 1-\n\n2 result");
    }

    #[test]
    fn runaway_punctuation_is_collapsed() {
        let regions = vec![region(0, "text", "end......")];
        assert_eq!(merge_page_to_markdown(&regions), "end...");
    }

    #[test]
    fn an_inline_formula_gets_its_spaces() {
        let regions = vec![region(0, "text", "value$x$here")];
        assert_eq!(merge_page_to_markdown(&regions), "value $x$ here");
    }
}
