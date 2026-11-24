//! Bounded, byte-exact reconstruction of a document from its stored chunks
//! (`RET-14`). Pure: no I/O, no store, no environment reads.
//!
//! The chunker emits overlapping windows over its *normalised* text. That
//! overlap is a prefix of a chunk that repeats the tail of its predecessor, so
//! simply concatenating `content` would print every sentence twice. Paragraph
//! separators at hard chunk boundaries are not part of any chunk's text, so
//! they have to be put back when a chunk has no overlap prefix.
//!
//! [`stitch`] rebuilds the whole document from chunk 0. [`stitch_from`] does the
//! same for a window that starts later: [`WindowStart::Selection`] drops the
//! first chunk's overlap, because the chunk it repeats is outside the selection,
//! and [`WindowStart::Continuation`] also restores a hard-boundary separator
//! that belongs between the previous page and this one. That is what makes the
//! concatenation of all cursor pages byte-identical to one unbounded read.
//!
//! Old chunks carry no spans at all. They are appended whole with `"\n\n"`
//! between them and reported through [`Stitched::missing_spans`], rather than
//! pretending an absent span means zero.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

/// One ordered chunk as the stitcher needs it. The server fills this from the
/// vector-free chunk-store read; no adapter type appears here.
#[derive(Debug, Clone)]
pub struct StitchChunk {
    pub chunk_index: u32,
    pub content: String,
    pub char_start: Option<u32>,
    pub char_end: Option<u32>,
    pub overlap_prefix_len: Option<u32>,
}

impl StitchChunk {
    /// True only when all three span fields are present. An old chunk has none
    /// of them, never a misleading zero.
    #[must_use]
    pub fn has_spans(&self) -> bool {
        self.char_start.is_some() && self.char_end.is_some() && self.overlap_prefix_len.is_some()
    }
}

/// The result of one bounded read: the stitched text, the index of the last
/// chunk that was emitted, whether more chunks remain, and whether any chunk
/// had to be emitted whole because its spans were missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stitched {
    pub text: String,
    pub last_chunk_index: u32,
    pub truncated: bool,
    pub missing_spans: bool,
}

/// How a stitched window starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowStart {
    /// The first chunk of the document: emit it whole.
    Document,
    /// A fresh section/page/chunk selection: drop the first chunk's overlap,
    /// because the chunk it repeats is outside the selection, but do not start
    /// the text with a paragraph separator.
    Selection,
    /// A cursor continuation: same as `Selection`, and a hard-boundary
    /// separator is put back because the previous chunk was on the previous
    /// page. This is what keeps concatenated cursor pages byte-exact.
    Continuation,
}

/// Stitch a window of ordered chunks under a character budget.
#[must_use]
pub fn stitch_from(chunks: &[StitchChunk], max_chars: usize, start: WindowStart) -> Stitched {
    let mut text = String::new();
    let mut chars = 0usize;
    let mut last_chunk_index = chunks.first().map_or(0, |chunk| chunk.chunk_index);
    let mut truncated = false;
    let mut missing_spans = false;

    for (i, chunk) in chunks.iter().enumerate() {
        let first = i == 0;
        let mut piece = String::new();
        if !chunk.has_spans() {
            missing_spans = true;
            if !first {
                piece.push_str("\n\n");
            }
            piece.push_str(&chunk.content);
        } else {
            let overlap = chunk.overlap_prefix_len.unwrap_or(0) as usize;
            if first && start == WindowStart::Continuation && overlap == 0 {
                // A hard boundary's "\n\n" is not stored in any chunk. The
                // previous chunk is on the previous page, so this page has to
                // put the separator back even though this is its first chunk.
                piece.push_str("\n\n");
                piece.push_str(&chunk.content);
            } else if first {
                if start != WindowStart::Document {
                    piece.extend(chunk.content.chars().skip(overlap));
                } else {
                    piece.push_str(&chunk.content);
                }
            } else if overlap == 0 {
                // A hard boundary: the chunker does not store the paragraph
                // separator, so put it back.
                piece.push_str("\n\n");
                piece.push_str(&chunk.content);
            } else {
                piece.extend(chunk.content.chars().skip(overlap));
            }
        }
        let added = piece.chars().count();
        // Cut at a chunk boundary before the budget would be exceeded. `i == 0`
        // is unconditional: a read always emits at least one chunk, even when
        // that single chunk is longer than the budget.
        if i > 0 && chars.saturating_add(added) > max_chars {
            truncated = true;
            break;
        }
        text.push_str(&piece);
        chars = chars.saturating_add(added);
        last_chunk_index = chunk.chunk_index;
    }

    Stitched {
        text,
        last_chunk_index,
        truncated,
        missing_spans,
    }
}

/// Stitch a document from its first chunk, dropping overlap from every chunk
/// after it and putting hard-boundary separators back.
#[must_use]
pub fn stitch(chunks: &[StitchChunk], max_chars: usize) -> Stitched {
    stitch_from(chunks, max_chars, WindowStart::Document)
}

/// The opaque continuation token. The wire JSON keys are deliberately short:
/// the token is embedded in a URL query string, not a published response shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentCursor {
    /// Document id the cursor was issued for.
    pub d: String,
    /// Next chunk index to read.
    pub n: u32,
    /// End chunk index of the original selection.
    pub e: u32,
    /// Content hash at issue time (`sha256`, or `updatedAt` when absent).
    pub h: String,
}

impl ContentCursor {
    /// Encode as base64url without padding, safe in a query string.
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_vec(self)
            .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
            .unwrap_or_default()
    }

    /// Decode a cursor, or `None` for anything that is not our JSON shape.
    #[must_use]
    pub fn decode(value: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn chunk(index: u32, content: &str, span: Option<(u32, u32, u32)>) -> StitchChunk {
        StitchChunk {
            chunk_index: index,
            content: content.to_string(),
            char_start: span.map(|s| s.0),
            char_end: span.map(|s| s.1),
            overlap_prefix_len: span.map(|s| s.2),
        }
    }

    #[test]
    fn overlap_is_removed_and_a_hard_boundary_gets_its_separator() {
        let chunks = vec![
            chunk(0, "abcdefgh", Some((0, 8, 0))),
            // "fgh" repeats the tail of chunk 0.
            chunk(1, "fghijklm", Some((8, 16, 3))),
            // No overlap: a paragraph boundary, whose "\n\n" is not in a chunk.
            chunk(2, "NEW PARA", Some((16, 24, 0))),
        ];
        let stitched = stitch(&chunks, 50000);
        assert_eq!(stitched.text, "abcdefghijklm\n\nNEW PARA");
        assert_eq!(stitched.last_chunk_index, 2);
        assert!(!stitched.truncated);
        assert!(!stitched.missing_spans);
    }

    #[test]
    fn overlap_is_counted_in_characters_not_bytes() {
        // "é" and "中" are multibyte: skipping two *characters* must not cut a
        // UTF-8 code point in half.
        let chunks = vec![
            chunk(0, "aé中b", Some((0, 4, 0))),
            chunk(1, "中bcd", Some((4, 7, 2))),
        ];
        let stitched = stitch(&chunks, 50000);
        assert_eq!(stitched.text, "aé中bcd");
        assert_eq!(stitched.text.chars().count(), 6);
    }

    #[test]
    fn chunks_without_spans_are_whole_and_reported_once() {
        let chunks = vec![
            chunk(0, "first", Some((0, 5, 0))),
            chunk(1, "firsttail second", None),
            chunk(2, "third", None),
        ];
        let stitched = stitch(&chunks, 50000);
        assert_eq!(stitched.text, "first\n\nfirsttail second\n\nthird");
        assert!(stitched.missing_spans);
    }

    #[test]
    fn max_chars_cuts_at_a_chunk_boundary_but_always_emits_one_chunk() {
        let chunks = vec![
            chunk(0, "abcd", Some((0, 4, 0))),
            chunk(1, "cdEF", Some((4, 8, 2))),
            chunk(2, "EFgh", Some((8, 12, 2))),
        ];
        let exactly_one = stitch(&chunks, 4);
        assert_eq!(exactly_one.text, "abcd");
        assert!(exactly_one.truncated);
        assert_eq!(exactly_one.last_chunk_index, 0);

        // Two chunks fit exactly at six characters; the third does not.
        let two = stitch(&chunks, 6);
        assert_eq!(two.text, "abcdEF");
        assert!(two.truncated);
        assert_eq!(two.last_chunk_index, 1);

        // A single chunk longer than the budget is still emitted in full.
        let long = vec![chunk(0, "0123456789", Some((0, 10, 0)))];
        let one = stitch(&long, 1);
        assert_eq!(one.text, "0123456789");
        assert!(!one.truncated);
    }

    #[test]
    fn a_continuation_drops_the_first_chunks_overlap() {
        let chunks = vec![
            chunk(10, "tail next part", Some((90, 104, 5))),
            chunk(11, "part and more", Some((104, 117, 4))),
        ];
        let whole = stitch_from(&chunks, 50000, WindowStart::Selection);
        assert_eq!(whole.text, "next part and more");
    }

    #[test]
    fn a_continuation_at_a_hard_boundary_keeps_the_separator() {
        let chunks = vec![
            chunk(10, "fresh after the break", Some((90, 111, 0))),
            chunk(11, "the break and more", Some((111, 129, 9))),
        ];
        let whole = stitch_from(&chunks, 50000, WindowStart::Continuation);
        assert_eq!(whole.text, "\n\nfresh after the break and more");
    }

    #[test]
    fn an_empty_window_is_empty_and_not_truncated() {
        let stitched = stitch(&[], 500);
        assert_eq!(stitched.text, "");
        assert!(!stitched.truncated);
        assert!(!stitched.missing_spans);
    }

    #[test]
    fn a_cursor_round_trips_and_rejects_garbage() {
        let cursor = ContentCursor {
            d: "doc-1".to_string(),
            n: 40,
            e: 57,
            h: "deadbeef".to_string(),
        };
        let encoded = cursor.encode();
        assert!(!encoded.contains('='), "base64url has no padding");
        assert_eq!(ContentCursor::decode(&encoded), Some(cursor));
        assert_eq!(ContentCursor::decode("not-base64!"), None);
        assert_eq!(
            ContentCursor::decode("e30"),
            None,
            "an empty object is not a cursor"
        );
    }
}
