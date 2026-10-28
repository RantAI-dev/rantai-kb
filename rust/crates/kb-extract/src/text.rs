//! Text decoding and the code-fence wrapper.

/// `buffer.toString("utf-8")`: invalid byte sequences become U+FFFD, exactly
/// as Node decodes them, so a mis-encoded upload yields the same replacement
/// characters the TypeScript service would have produced.
#[must_use]
pub fn to_utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_passes_through() {
        assert_eq!(to_utf8("héllo".as_bytes()), "héllo");
    }

    #[test]
    fn invalid_bytes_become_replacement_characters() {
        assert_eq!(to_utf8(&[b'a', 0xff, 0xfe, b'b']), "a\u{fffd}\u{fffd}b");
    }
}
