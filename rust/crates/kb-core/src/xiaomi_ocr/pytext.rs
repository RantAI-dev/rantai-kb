//! Python's string rules, as Rust helpers.
//!
//! The postprocessing ported from Xiaomi-OCR-0 has to agree with the Python it
//! was ported from byte for byte, and Python's string rules differ from Rust's
//! in three places that matter here: `str.strip()` and `str.split()` use
//! Python's whitespace set (which includes the ASCII file separators Rust does
//! not call whitespace), `str.splitlines()` breaks on more than `\n`, and
//! string indices count code points, not bytes.
//!
//! **Ported from** the behaviour of `xiaomi-research/Xiaomi-OCR-0`
//! (Apache-2.0) postprocessing. See `docs/model-licences.md`.

/// Whether a character is whitespace for Python's `str.strip()`/`str.split()`.
///
/// Python adds the ASCII file separators (`\x1c`–`\x1f`) to the Unicode
/// whitespace set.
#[must_use]
pub fn is_space(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}')
}

/// Python's `str.strip()`: drop leading and trailing whitespace.
#[must_use]
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// Python's `str.lstrip()`: drop leading whitespace.
#[must_use]
pub fn lstrip(text: &str) -> &str {
    text.trim_start_matches(is_space)
}

/// Python's `str.rstrip()`: drop trailing whitespace.
#[must_use]
pub fn rstrip(text: &str) -> &str {
    text.trim_end_matches(is_space)
}

/// Python's `str.split()` with no argument: split on runs of whitespace and
/// drop the empty pieces.
#[must_use]
pub fn split_whitespace(text: &str) -> Vec<&str> {
    text.split(is_space)
        .filter(|part| !part.is_empty())
        .collect()
}

/// Python's `str.splitlines()`: break after `\n`, `\r`, `\r\n`, `\v`, `\f`,
/// `\x1c`, `\x1d`, `\x1e`, `\x85`, `\u{2028}` and `\u{2029}`.
///
/// `\r\n` is one break, not two.
#[must_use]
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let Some(width) = line_break_width(bytes, index) else {
            index += 1;
            continue;
        };
        lines.push(&text[start..index]);
        index += width;
        start = index;
    }
    lines.push(&text[start..]);
    lines
}

/// The width of the line break at `index`, if there is one.
fn line_break_width(bytes: &[u8], index: usize) -> Option<usize> {
    match bytes[index] {
        b'\n' => Some(1),
        b'\r' => Some(if bytes.get(index + 1) == Some(&b'\n') {
            2
        } else {
            1
        }),
        0x0b | 0x0c | 0x1c | 0x1d | 0x1e => Some(1),
        _ => {
            // The remaining breaks are multi-byte UTF-8 sequences; they are
            // found by decoding, not by looking at one byte.
            let rest = std::str::from_utf8(&bytes[index..]).ok()?;
            let ch = rest.chars().next()?;
            matches!(ch, '\u{85}' | '\u{2028}' | '\u{2029}').then_some(ch.len_utf8())
        }
    }
}

/// Python's `len(text)`: the number of code points.
#[must_use]
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn python_whitespace_includes_the_file_separators() {
        assert!(is_space('\u{1c}'));
        assert!(is_space(' '));
        assert!(is_space('\u{a0}'));
        assert!(!is_space('a'));
    }

    #[test]
    fn split_lines_breaks_on_cr_lf_as_one() {
        assert_eq!(split_lines("a\r\nb"), vec!["a", "b"]);
        assert_eq!(split_lines("a\nb"), vec!["a", "b"]);
        assert_eq!(split_lines("a\n\nb"), vec!["a", "", "b"]);
    }

    #[test]
    fn split_whitespace_drops_the_empty_pieces() {
        assert_eq!(split_whitespace("  a  b "), vec!["a", "b"]);
    }
}
