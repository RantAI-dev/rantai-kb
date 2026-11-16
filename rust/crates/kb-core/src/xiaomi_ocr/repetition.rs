//! Repetition analysis: is this output the model looping?
//!
//! A vision-language model asked to read a page can collapse into a loop: the
//! same character, the same block or the same line over and over. Such an
//! output is not "a page with little text", it is a failed read, and it must
//! be retried rather than indexed.
//!
//! The analysis looks for four shapes: a finish reason of `length` (the server
//! cut the answer off), a long run of one character, a repeated suffix, and one
//! line repeated across the answer.
//!
//! **Ported from** `postprocess/repetition_guard.py` of
//! `xiaomi-research/Xiaomi-OCR-0` (Apache-2.0). See `docs/model-licences.md`.

use std::collections::HashMap;

use super::pytext;

/// The longest run of one character in an output.
#[derive(Debug, Clone, PartialEq)]
pub struct SameCharacterRun {
    /// The character the run is made of.
    pub character: String,
    /// How many of them there are in a row.
    pub length: usize,
}

/// A suffix of the output that repeats itself.
#[derive(Debug, Clone, PartialEq)]
pub struct RepeatingSuffix {
    /// The repeated unit.
    pub unit: String,
    /// The unit's length in characters.
    pub unit_chars: usize,
    /// How many times the unit repeats.
    pub repeats: usize,
    /// How many characters the repetition covers.
    pub repeated_chars: usize,
    /// The share of the output the repetition covers (0.0–1.0).
    pub ratio: f64,
}

/// One line repeated across the output.
#[derive(Debug, Clone, PartialEq)]
pub struct DominantRepeatedLine {
    /// The repeated line.
    pub line: String,
    /// How many times it appears.
    pub repeats: usize,
    /// How many non-empty lines the output has.
    pub total_lines: usize,
    /// How many characters the repetition covers.
    pub repeated_chars: usize,
    /// The share of the lines the repetition covers (0.0–1.0).
    pub ratio: f64,
}

/// What the analysis found.
#[derive(Debug, Clone, PartialEq)]
pub struct RepetitionAnalysis {
    /// Whether the output looks like a loop.
    pub suspicious: bool,
    /// Why: `finish_reason_length`, `same_character_run`, `repeating_suffix`,
    /// `dominant_repeated_line`.
    pub reasons: Vec<String>,
    /// The output's length in characters.
    pub characters: usize,
    /// The output's length without whitespace.
    pub non_whitespace_characters: usize,
    /// The server's finish reason, when it sent one.
    pub finish_reason: Option<String>,
    /// The longest run of one character.
    pub same_character: SameCharacterRun,
    /// The share of the output that run covers.
    pub same_character_ratio: f64,
    /// The repeated suffix, when there is one.
    pub repeating_suffix: Option<RepeatingSuffix>,
    /// The repeated line, when there is one.
    pub dominant_repeated_line: Option<DominantRepeatedLine>,
}

/// The defaults the vendor's recipe uses.
pub const DEFAULT_SAME_CHARACTER_RUN: usize = 128;
/// Default: how many repetitions of a suffix count as a loop.
pub const DEFAULT_MIN_REPEATS: usize = 8;
/// Default: how many characters a repetition must cover.
pub const DEFAULT_MIN_REPEATED_CHARS: usize = 512;
/// Default: the share of the output a repetition must cover.
pub const DEFAULT_MIN_RATIO: f64 = 0.5;
/// Default: the longest repeated suffix unit that is inspected.
pub const DEFAULT_MAX_UNIT_CHARS: usize = 256;

/// Normalise whitespace: collapse runs and trim, as Python's `str.split()` does.
fn normalize_whitespace(text: &str) -> String {
    pytext::split_whitespace(text).join(" ")
}

/// The longest run of one character; whitespace breaks the run.
fn longest_same_character_run(text: &str) -> SameCharacterRun {
    let mut best_character = String::new();
    let mut best_length = 0usize;
    let mut current_character = String::new();
    let mut current_length = 0usize;
    for ch in text.chars() {
        if pytext::is_space(ch) {
            current_character.clear();
            current_length = 0;
        } else if current_character.starts_with(ch) {
            current_length += 1;
        } else {
            current_character = ch.to_string();
            current_length = 1;
        }
        if current_length > best_length {
            best_character = current_character.clone();
            best_length = current_length;
        }
    }
    SameCharacterRun {
        character: best_character,
        length: best_length,
    }
}

/// A suffix of `text` that repeats itself.
fn repeating_suffix(
    text: &str,
    min_repeats: usize,
    min_repeated_chars: usize,
    min_ratio: f64,
    max_unit_chars: usize,
) -> Option<RepeatingSuffix> {
    let chars: Vec<char> = text.chars().collect();
    let length = chars.len();
    if length < min_repeated_chars {
        return None;
    }
    let max_unit = max_unit_chars.min(length / min_repeats);
    let mut best: Option<RepeatingSuffix> = None;
    for unit_length in 1..=max_unit {
        let unit = &chars[length - unit_length..];
        let mut cursor = length - unit_length;
        let mut count = 1usize;
        while cursor >= unit_length && chars[cursor - unit_length..cursor] == *unit {
            cursor -= unit_length;
            count += 1;
        }
        let repeated_chars = count * unit_length;
        let ratio = repeated_chars as f64 / length as f64;
        if count < min_repeats || repeated_chars < min_repeated_chars || ratio < min_ratio {
            continue;
        }
        let candidate = RepeatingSuffix {
            unit: unit.iter().collect(),
            unit_chars: unit_length,
            repeats: count,
            repeated_chars,
            ratio,
        };
        if best
            .as_ref()
            .is_none_or(|current| repeated_chars > current.repeated_chars)
        {
            best = Some(candidate);
        }
    }
    best
}

/// The line that appears most often in the output.
fn dominant_repeated_line(
    text: &str,
    min_repeats: usize,
    min_repeated_chars: usize,
    min_ratio: f64,
) -> Option<DominantRepeatedLine> {
    let lines: Vec<String> = pytext::split_lines(text)
        .into_iter()
        .map(pytext::strip)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect();
    if lines.len() < min_repeats {
        return None;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for line in &lines {
        *counts.entry(line.as_str()).or_insert(0) += 1;
    }
    // The most common line; on a tie the one that appears first, as Python's
    // `Counter.most_common` does.
    let mut best: Option<(&str, usize)> = None;
    for line in &lines {
        let count = counts[line.as_str()];
        if best.is_none_or(|(_, current)| count > current) {
            best = Some((line.as_str(), count));
        }
    }
    let (line, count) = best?;
    let repeated_chars = line.chars().count() * count;
    let ratio = count as f64 / lines.len() as f64;
    if count < min_repeats || repeated_chars < min_repeated_chars || ratio < min_ratio {
        return None;
    }
    Some(DominantRepeatedLine {
        line: line.to_string(),
        repeats: count,
        total_lines: lines.len(),
        repeated_chars,
        ratio,
    })
}

/// Whether an output looks like the model looping.
///
/// `finish_reason` is the server's reason for stopping: `length` means the
/// answer was cut off, which on its own makes the output suspicious.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn analyze_repetition(
    text: &str,
    finish_reason: Option<&str>,
    same_character_run: usize,
    min_repeats: usize,
    min_repeated_chars: usize,
    min_ratio: f64,
    max_unit_chars: usize,
) -> RepetitionAnalysis {
    let normalized = normalize_whitespace(text);
    let non_whitespace_length = normalized.chars().filter(|ch| *ch != ' ').count();
    let mut reasons: Vec<String> = Vec::new();

    if finish_reason == Some("length") {
        reasons.push("finish_reason_length".to_string());
    }

    let same_character = longest_same_character_run(text);
    let same_character_ratio = if non_whitespace_length > 0 {
        same_character.length as f64 / non_whitespace_length as f64
    } else {
        0.0
    };
    if same_character.length >= same_character_run
        && (same_character.length >= min_repeated_chars || same_character_ratio >= min_ratio)
    {
        reasons.push("same_character_run".to_string());
    }

    let suffix = repeating_suffix(
        &normalized,
        min_repeats,
        min_repeated_chars,
        min_ratio,
        max_unit_chars,
    );
    if suffix.is_some() {
        reasons.push("repeating_suffix".to_string());
    }

    let repeated_line =
        dominant_repeated_line(text, min_repeats, min_repeated_chars, min_ratio.max(0.8));
    if repeated_line.is_some() {
        reasons.push("dominant_repeated_line".to_string());
    }

    RepetitionAnalysis {
        suspicious: !reasons.is_empty(),
        reasons,
        characters: text.chars().count(),
        non_whitespace_characters: non_whitespace_length,
        finish_reason: finish_reason.map(ToString::to_string),
        same_character,
        same_character_ratio,
        repeating_suffix: suffix,
        dominant_repeated_line: repeated_line,
    }
}

/// [`analyze_repetition`] with the vendor's defaults.
#[must_use]
pub fn analyze_repetition_default(text: &str, finish_reason: Option<&str>) -> RepetitionAnalysis {
    analyze_repetition(
        text,
        finish_reason,
        DEFAULT_SAME_CHARACTER_RUN,
        DEFAULT_MIN_REPEATS,
        DEFAULT_MIN_REPEATED_CHARS,
        DEFAULT_MIN_RATIO,
        DEFAULT_MAX_UNIT_CHARS,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_page_is_not_suspicious() {
        let analysis = analyze_repetition_default("Hello world.\n\nA page of text.", None);
        assert!(!analysis.suspicious, "{:?}", analysis.reasons);
        assert!(analysis.reasons.is_empty());
    }

    #[test]
    fn a_finish_reason_of_length_is_suspicious_on_its_own() {
        let analysis = analyze_repetition_default("a short answer", Some("length"));
        assert!(analysis.suspicious);
        assert_eq!(analysis.reasons, vec!["finish_reason_length".to_string()]);
    }

    #[test]
    fn a_long_run_of_one_character_is_suspicious() {
        let text = "x".repeat(200);
        let analysis = analyze_repetition_default(&text, None);
        assert!(analysis.suspicious);
        assert!(analysis.reasons.contains(&"same_character_run".to_string()));
        assert_eq!(analysis.same_character.length, 200);
        assert_eq!(analysis.same_character.character, "x");
    }

    #[test]
    fn a_repeated_suffix_is_suspicious() {
        let text = format!("intro text. {}", "ab".repeat(400));
        let analysis = analyze_repetition_default(&text, None);
        assert!(analysis.suspicious);
        assert!(analysis.reasons.contains(&"repeating_suffix".to_string()));
    }

    #[test]
    fn a_repeated_line_is_suspicious() {
        let text = (0..20)
            .map(|_| "a much longer repeated line of text here")
            .collect::<Vec<_>>()
            .join("\n");
        let analysis = analyze_repetition_default(&text, None);
        assert!(analysis.suspicious);
        assert!(
            analysis
                .reasons
                .contains(&"dominant_repeated_line".to_string())
        );
    }
}
