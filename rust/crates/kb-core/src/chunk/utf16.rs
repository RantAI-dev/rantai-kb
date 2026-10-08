//! UTF-16 length helpers mirroring JS `string.length`.

/// Number of UTF-16 code units in `s`. Returns 0 for empty input.
#[must_use]
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}
