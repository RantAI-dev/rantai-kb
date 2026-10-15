//! HTML and XML to text.
//!
//! `stripHtml` from `src/lib/files/parsers.ts:12`, which is what every HTML,
//! EPUB chapter and RTF document goes through. It is lossy by design: tags
//! become spaces and all whitespace collapses, so the result is one line of
//! readable text with the document's word order intact.

/// Strip markup, keeping only the text: drop `<script>` and `<style>` bodies,
/// turn every remaining tag into a space, collapse whitespace, trim.
#[must_use]
pub fn strip_html(input: &str) -> String {
    let without_scripts = drop_elements(input, "script");
    let without_styles = drop_elements(&without_scripts, "style");
    collapse_whitespace(&tags_to_spaces(&without_styles))
}

/// Strip tags without the `script`/`style` pass, which is what the TypeScript
/// XML branch does (`parsers.ts:298`): XML has no such elements, and treating
/// every one of them as a tag to drop would lose the document's content.
#[must_use]
pub fn strip_xml(input: &str) -> String {
    collapse_whitespace(&tags_to_spaces(input))
}

/// Find the byte offset of `needle` in `haystack` case-insensitively (ASCII).
fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let needle_bytes = needle.as_bytes();
    let haystack_bytes = haystack.as_bytes();
    if needle_bytes.len() > haystack_bytes.len() {
        return None;
    }
    for i in 0..=haystack_bytes.len() - needle_bytes.len() {
        if haystack.is_char_boundary(i)
            && haystack_bytes[i..i + needle_bytes.len()]
                .iter()
                .zip(needle_bytes.iter())
                .all(|(h, n)| h.to_ascii_lowercase() == n.to_ascii_lowercase())
        {
            return Some(i);
        }
    }
    None
}

/// Remove `<name …>…</name>` including the tags, case-insensitively.
///
/// A tag with no matching close tag is left alone, which is what the TypeScript
/// regular expression does: the lazy `[\s\S]*?</script>` only matches when a
/// close tag exists.
fn drop_elements(input: &str, name: &str) -> String {
    let open = format!("<{name}");
    let close = format!("</{name}");
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    loop {
        let Some(found) = find_ascii_case_insensitive(&input[cursor..], &open) else {
            break;
        };
        let tag_start = cursor + found;
        // `[^>]*` in the TypeScript pattern: the tag ends at the first `>`.
        let Some(tag_end) = input[tag_start..].find('>').map(|i| tag_start + i + 1) else {
            break;
        };
        let Some(close_at) = find_ascii_case_insensitive(&input[tag_end..], &close).map(|i| tag_end + i) else {
            break;
        };
        let Some(close_end) = input[close_at..].find('>').map(|i| close_at + i + 1) else {
            break;
        };
        out.push_str(&input[cursor..tag_start]);
        cursor = close_end;
    }
    out.push_str(&input[cursor..]);
    out
}

/// Every `<…>` becomes a space. `<>` is left as it is, matching `<[^>]+>`.
fn tags_to_spaces(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        match rest[open..].find('>') {
            // `[^>]+` needs at least one character between the brackets, so a
            // bare `<>` is not a tag.
            Some(end) if end > 1 => {
                out.push(' ');
                rest = &rest[open + end + 1..];
            }
            _ => {
                out.push('<');
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `\s+` → one space, then `trim()`. Rust's `char::is_whitespace` is the
/// Unicode `White_Space` property, which is what JavaScript's `\s` matches
/// except for U+FEFF; a BOM in the middle of a document is the one difference.
#[must_use]
pub fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut previous_was_space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            if !previous_was_space {
                out.push(' ');
                previous_was_space = true;
            }
        } else {
            out.push(ch);
            previous_was_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn tags_become_spaces_and_whitespace_collapses() {
        assert_eq!(
            strip_html("<p>Hello <b>world</b></p>"),
            "Hello world"
        );
        assert_eq!(strip_html("<div>a\n\n  b</div>"), "a b");
    }

    #[test]
    fn script_and_style_bodies_are_dropped() {
        assert_eq!(
            strip_html("<style>p{color:red}</style><p>text</p>"),
            "text"
        );
        assert_eq!(
            strip_html("<SCRIPT>var x = 1 < 2;</SCRIPT><p>text</p>"),
            "text"
        );
        // Case-insensitive, and the first close tag ends it.
        assert_eq!(strip_html("<script>a</script>b</script>c"), "b c");
    }

    #[test]
    fn an_unclosed_script_is_kept() {
        assert_eq!(strip_html("<p>a</p><script>never closed"), "a never closed");
    }

    #[test]
    fn a_bare_angle_bracket_pair_is_not_a_tag() {
        assert_eq!(strip_html("a <> b"), "a <> b");
    }

    #[test]
    fn entities_are_not_decoded() {
        // The TypeScript helper never decodes entities either, so `&amp;`
        // reaches the chunker exactly as written.
        assert_eq!(strip_html("<p>a &amp; b</p>"), "a &amp; b");
    }

    #[test]
    fn xml_tags_are_stripped_like_html_minus_the_script_pass() {
        assert_eq!(strip_tags("<a>one</a><b>two</b>"), "one two");
    }

    fn strip_tags(input: &str) -> String {
        collapse_whitespace(&tags_to_spaces(input))
    }
}
