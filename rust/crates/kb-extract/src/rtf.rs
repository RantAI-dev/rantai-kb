//! RTF (Rich Text Format) text extractor.
//!
//! Port of `src/lib/files/parsers.ts` `parseRtf`. The TypeScript version runs
//! `@iarna/rtf-to-html`, then strips tags with the same HTML stripper as `.html`.
//! We strip control words, convert character escapes (`\'hh`, `\uN`), collapse
//! whitespace, and drop ignorable groups (`{\*…}`) without pulling in a full
//! HTML intermediate pipeline.

use crate::ExtractError;

/// Parse an RTF file and return its plain-text contents.
///
/// Returns an empty string if the input contains no readable text, matching
/// the TypeScript behaviour. If the payload does not start with `{\rtf`, it is
/// decoded as plain UTF-8 text so files uploaded with an RTF MIME but plain text
/// content are not rejected.
pub fn parse(bytes: &[u8]) -> Result<String, ExtractError> {
    if !bytes.starts_with(b"{\\rtf") {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }
    let text = strip_control_words(bytes);
    let cleaned = crate::html::collapse_whitespace(&text);
    Ok(cleaned)
}

#[derive(Debug)]
struct Group {
    output_len: usize,
    ignorable: bool,
}

/// Remove RTF markup, keeping the text.
///
/// The walk is over bytes rather than characters because RTF is a byte format:
/// `\'e9` is one byte and `\u233` is a code point, and the two are the only
/// non-ASCII cases.
fn strip_control_words(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut groups: Vec<Group> = Vec::new();
    let mut ignorable_depth = 0usize;
    // `ucN` / `\binN`: how many characters of replacement or binary data to
    // swallow after the control word that set it.
    let mut skip_characters = 0usize;
    let mut unicode_replacements = 1usize;
    let mut index = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'{' => {
                let parent_ignorable = ignorable_depth > 0;
                groups.push(Group {
                    output_len: out.len(),
                    ignorable: parent_ignorable,
                });
                if parent_ignorable {
                    ignorable_depth += 1;
                }
                index += 1;
            }
            b'}' => {
                if let Some(group) = groups.pop() {
                    if group.ignorable {
                        out.truncate(group.output_len);
                        ignorable_depth = ignorable_depth.saturating_sub(1);
                    }
                }
                index += 1;
            }
            b'\\' => {
                index = control_word(
                    bytes,
                    index + 1,
                    &mut out,
                    &mut skip_characters,
                    &mut unicode_replacements,
                    &mut groups,
                    &mut ignorable_depth,
                );
            }
            b'\r' | b'\n' => index += 1,
            _ => {
                index += 1;
                if ignorable_depth > 0 {
                    continue;
                }
                if skip_characters > 0 {
                    skip_characters -= 1;
                    continue;
                }
                // Latin-1, matching `toString("binary")`.
                out.push(byte as char);
            }
        }
    }
    out
}

/// Handle one control word or escape starting at `index` (just after the
/// backslash) and return the index to continue from.
fn control_word(
    bytes: &[u8],
    mut index: usize,
    out: &mut String,
    skip_characters: &mut usize,
    unicode_replacements: &mut usize,
    groups: &mut [Group],
    ignorable_depth: &mut usize,
) -> usize {
    let Some(byte) = bytes.get(index).copied() else {
        return index;
    };
    match byte {
        b'\\' | b'{' | b'}' => {
            if *ignorable_depth == 0 {
                out.push(byte as char);
            }
            index + 1
        }
        b'\'' => {
            // `\'hh` is one byte of the document's code page. Latin-1 keeps the
            // same byte, which is what a binary-string decode gives.
            let high = bytes.get(index + 1).copied().and_then(hex_value);
            let low = bytes.get(index + 2).copied().and_then(hex_value);
            match (high, low) {
                (Some(high), Some(low)) => {
                    if *ignorable_depth == 0 {
                        out.push(u8::try_from(high * 16 + low).unwrap_or(b'\'') as char);
                    }
                    index + 3
                }
                _ => {
                    if *ignorable_depth == 0 {
                        out.push('\'');
                    }
                    index + 1
                }
            }
        }
        b'*' => {
            // `{\*\destination …}`: the group is metadata, and rtf-to-html
            // renders nothing for it.
            if let Some(group) = groups.last_mut() {
                if !group.ignorable {
                    group.ignorable = true;
                    *ignorable_depth += 1;
                }
            }
            index + 1
        }
        b'\r' | b'\n' => {
            if *ignorable_depth == 0 {
                out.push('\n');
            }
            index + 1
        }
        _ if byte.is_ascii_alphabetic() => {
            let start = index;
            while bytes.get(index).is_some_and(u8::is_ascii_alphabetic) {
                index += 1;
            }
            let word = String::from_utf8_lossy(&bytes[start..index]).into_owned();
            // An optional numeric parameter, then one delimiter space.
            let negative = bytes.get(index) == Some(&b'-');
            if negative {
                index += 1;
            }
            let number_start = index;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
            let parameter = std::str::from_utf8(&bytes[number_start..index])
                .ok()
                .and_then(|digits| digits.parse::<i64>().ok());
            if bytes.get(index) == Some(&b' ') {
                index += 1;
            }
            apply_control_word(
                &word,
                parameter,
                negative,
                out,
                skip_characters,
                unicode_replacements,
                *ignorable_depth,
            );
            index
        }
        _ => {
            if *ignorable_depth == 0 {
                out.push(byte as char);
            }
            index + 1
        }
    }
}

fn apply_control_word(
    word: &str,
    parameter: Option<i64>,
    negative: bool,
    out: &mut String,
    skip_characters: &mut usize,
    unicode_replacements: &mut usize,
    ignorable_depth: usize,
) {
    match word {
        "par" | "line" | "sect" | "page" => {
            if ignorable_depth == 0 {
                out.push('\n');
            }
        }
        "tab" => {
            if ignorable_depth == 0 {
                out.push('\t');
            }
        }
        "uc" => {
            if let Some(value) = parameter {
                *unicode_replacements = usize::try_from(value.max(0)).unwrap_or(0);
            }
        }
        "u" => {
            let code = parameter.unwrap_or(0);
            let code = if negative { -code } else { code };
            if ignorable_depth == 0 {
                if let Some(character) = u32::try_from(code).ok().and_then(char::from_u32) {
                    out.push(character);
                }
            }
            *skip_characters = *unicode_replacements;
        }
        "bin" => {
            if let Some(value) = parameter {
                *skip_characters = usize::try_from(value.max(0)).unwrap_or(0);
            }
        }
        _ => {}
    }
}

fn hex_value(byte: u8) -> Option<u32> {
    (byte as char).to_digit(16)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_survive() {
        let text = parse(br"{\rtf1\ansi Hello \b world\b0 .}").unwrap();
        assert_eq!(text, "Hello world.");
    }

    #[test]
    fn paragraphs_and_tabs_flatten_to_spaces() {
        let text = parse(br"{\rtf1\ansi First\par Second\tab third}").unwrap();
        assert_eq!(text, "First Second third");
    }

    #[test]
    fn escaped_braces_are_literal() {
        let text = parse(br"{\rtf1\ansi a\{b\}c}").unwrap();
        assert_eq!(text, "a{b}c");
    }

    #[test]
    fn ignorable_destinations_are_dropped() {
        let text = parse(br"{\rtf1{\*\generator Word 12.0;} Kept}").unwrap();
        assert_eq!(text, "Kept");
    }

    #[test]
    fn hex_escapes_decode_as_latin_one_bytes() {
        let text = parse(br"{\rtf1\ansi caf\'e9}").unwrap();
        assert_eq!(text, "café");
    }

    #[test]
    fn unicode_escapes_replace_their_fallback_character() {
        // `é` written as a code point with one replacement character after it.
        let text = parse(br"{\rtf1\ansi\uc1\u233e}").unwrap();
        assert_eq!(text, "é");
    }
}
