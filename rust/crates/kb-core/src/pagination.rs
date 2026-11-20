//! Opaque pagination cursors.
//!
//! A cursor is a *position* in a stable order, not a row id: it names the last
//! row a page ended on, so the next page starts strictly after it. Two
//! properties make the walk correct:
//!
//! * the order is total — `(createdAt, id)`, never `createdAt` alone, because
//!   many documents share a millisecond and a walk that only orders by time
//!   can repeat or skip a row;
//! * the cursor is opaque on the wire — base64url over a private encoding, so
//!   a caller cannot read the order out of it, cannot edit it into another
//!   tenant's position, and cannot be relied on to keep working if the order
//!   changes.
//!
//! The cursor never widens what a caller can see. It only says *where to
//! continue*; the tenant, the knowledge-base and the soft-delete filters are
//! applied by the query that reads it, exactly as they are for the first page.
//!
//! Pure logic: no I/O, no async, no environment reads.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The largest page the documents list serves.
///
/// One number, so the route and the store cannot disagree: a route that
/// promised 500 while the store stopped at 200 answered "200" and said
/// nothing, which is how a caller ends up paging a list it believes is
/// complete.
pub const MAX_PAGE_SIZE: u32 = 200;

/// The default page size when the caller does not ask for one.
pub const DEFAULT_PAGE_SIZE: u32 = 100;

/// One position in the documents order.
///
/// `created_at` is the naive UTC creation time of the row, in the form the
/// store keeps it in (`Document.createdAt` is a `TIMESTAMP(3)` holding UTC).
/// The cursor is opaque, so this never has to agree with the `createdAt` the
/// API renders; it only has to be the *same value* the ordering uses, or the
/// walk would skip and repeat rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// The creation time of the last row of the page.
    pub created_at: String,
    /// The id of the last row of the page.
    pub id: String,
}

impl Cursor {
    /// A cursor at the given position.
    #[must_use]
    pub fn new(created_at: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            created_at: created_at.into(),
            id: id.into(),
        }
    }
}

/// Encode a position as the opaque token a caller sends back.
#[must_use]
pub fn encode(cursor: &Cursor) -> String {
    let raw = format!("{}|{}", cursor.created_at, cursor.id);
    URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

/// Decode a token back into a position.
///
/// `None` when the token is not one this server minted: a caller that sends
/// rubbish gets a `400` naming the parameter, not an empty page that looks
/// like the end of the list.
#[must_use]
pub fn decode(token: &str) -> Option<Cursor> {
    let bytes = URL_SAFE_NO_PAD.decode(token.trim()).ok()?;
    let raw = String::from_utf8(bytes).ok()?;
    let (created_at, id) = raw.split_once('|')?;
    if created_at.is_empty() || id.is_empty() {
        return None;
    }
    Some(Cursor {
        created_at: created_at.to_string(),
        id: id.to_string(),
    })
}

/// The cursor's creation time as the naive UTC timestamp Postgres stores
/// `Document.createdAt` in.
///
/// `None` when the timestamp is not in the form a cursor carries it in, which
/// can only happen for a token this server did not mint.
#[must_use]
pub fn created_at_timestamp(cursor: &Cursor) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(&cursor.created_at, "%Y-%m-%dT%H:%M:%S%.3f").ok()
}

/// Format a naive UTC timestamp the way a cursor carries it.
#[must_use]
pub fn format_timestamp(timestamp: chrono::NaiveDateTime) -> String {
    timestamp.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
}

/// The page size to apply, and whether the caller's request had to be changed.
///
/// `requested` is `None` when the caller did not ask for a size at all (or
/// asked for something that is not a number): that is an absent limit, not a
/// clamped one, and the two must not be reported as the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSize {
    /// The number of rows the query will fetch.
    pub limit: u32,
    /// `true` when the caller asked for a number and got a different one.
    pub clamped: bool,
}

/// Resolve a requested page size against the cap.
///
/// A request above the cap is **clamped, never refused**: an existing caller
/// that asks for 500 documents must keep working. But it is told: `clamped` is
/// `true`, so the response can report both the limit that was applied and the
/// fact that it was not the one that was asked for.
#[must_use]
pub fn resolve_page_size(requested: Option<u32>) -> PageSize {
    let limit = requested
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    PageSize {
        limit,
        clamped: requested.is_some_and(|r| r != limit),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::{
        Cursor, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, created_at_timestamp, decode, encode,
        format_timestamp, resolve_page_size,
    };

    #[test]
    fn a_cursor_round_trips() {
        let cursor = Cursor::new("2026-11-20T05:37:00.000", "doc-1");
        let token = encode(&cursor);
        assert_eq!(decode(&token), Some(cursor));
    }

    #[test]
    fn the_token_is_opaque() {
        let token = encode(&Cursor::new("2026-11-20T05:37:00.000", "doc-1"));
        assert!(!token.contains("2026"));
        assert!(!token.contains("doc-1"));
        assert!(!token.contains('|'));
    }

    #[test]
    fn a_token_is_url_safe() {
        let token = encode(&Cursor::new("2026-11-20T05:37:00.000", "doc-1"));
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{token}"
        );
    }

    #[test]
    fn rubbish_is_not_a_cursor() {
        assert_eq!(decode("not base64!!"), None);
        assert_eq!(decode(""), None);
        assert_eq!(decode(&URL_SAFE_NO_PAD.encode(b"no-separator")), None);
        assert_eq!(decode(&URL_SAFE_NO_PAD.encode(b"|doc-1")), None);
        assert_eq!(
            decode(&URL_SAFE_NO_PAD.encode(b"2026-11-20T05:37:00.000|")),
            None
        );
    }

    #[test]
    fn the_timestamp_parses_as_the_column_holds_it() {
        let cursor = Cursor::new("2026-11-20T05:37:00.123", "doc-1");
        let ts = created_at_timestamp(&cursor).expect("parses");
        assert_eq!(ts.to_string(), "2026-11-20 05:37:00.123");
    }

    #[test]
    fn a_timestamp_round_trips_through_the_cursor_form() {
        let ts = chrono::NaiveDateTime::parse_from_str(
            "2026-11-20T05:37:00.123",
            "%Y-%m-%dT%H:%M:%S%.3f",
        )
        .expect("parses");
        let text = format_timestamp(ts);
        let cursor = Cursor::new(text, "doc-1");
        assert_eq!(created_at_timestamp(&cursor), Some(ts));
    }

    #[test]
    fn a_missing_limit_is_the_default_and_is_not_clamped() {
        let size = resolve_page_size(None);
        assert_eq!(size.limit, DEFAULT_PAGE_SIZE);
        assert!(!size.clamped);
    }

    #[test]
    fn a_limit_above_the_cap_is_clamped_and_reported() {
        let size = resolve_page_size(Some(500));
        assert_eq!(size.limit, MAX_PAGE_SIZE);
        assert!(size.clamped);
    }

    #[test]
    fn a_limit_inside_the_cap_is_kept_as_it_is() {
        let size = resolve_page_size(Some(10));
        assert_eq!(size.limit, 10);
        assert!(!size.clamped);
    }

    #[test]
    fn a_limit_below_one_is_raised_and_reported() {
        let size = resolve_page_size(Some(0));
        assert_eq!(size.limit, 1);
        assert!(size.clamped);
    }
}
