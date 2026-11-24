//! Pure version-date helpers, shared by the HTTP boundary and both document
//! stores. No I/O: parsing is a `chrono` call, and the result is a canonical
//! UTC timestamp string that compares lexicographically in the same order as
//! Postgres compares `timestamptz`.
//!
//! The API accepts either a date (`2024-06-01`, meaning midnight UTC) or a full
//! ISO-8601 timestamp with an offset (`2024-06-01T07:00:00+07:00`). Both are
//! normalised to `YYYY-MM-DDTHH:MM:SSZ` so the Postgres `asOf` bind and the
//! fake store's string comparison agree on what "in force" means. A value that
//! is neither is rejected with a 400 by the route; a store never receives an
//! unparsed string.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

/// Normalise an API date/timestamp to a UTC timestamp string.
///
/// `None` means the input is not a date this service accepts. Sub-second
/// precision is dropped deliberately: `effectiveFrom` and `asOf` are dates on
/// which a policy comes into force, not event timestamps, and a canonical
/// second-resolution string sorts correctly as text in the in-memory store.
#[must_use]
pub fn normalise_timestamp(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }
    let parsed: DateTime<Utc> = if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        date.and_hms_opt(0, 0, 0)
            .map(|naive| Utc.from_utc_datetime(&naive))?
    } else {
        DateTime::parse_from_rfc3339(value)
            .ok()?
            .with_timezone(&Utc)
    };
    Some(parsed.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

#[cfg(test)]
mod tests {
    use super::normalise_timestamp;

    #[test]
    fn a_date_means_midnight_utc() {
        assert_eq!(
            normalise_timestamp("2024-06-01").as_deref(),
            Some("2024-06-01T00:00:00Z")
        );
    }

    #[test]
    fn an_offset_timestamp_is_normalised_to_utc() {
        assert_eq!(
            normalise_timestamp("2024-06-01T07:30:00+07:00").as_deref(),
            Some("2024-06-01T00:30:00Z")
        );
    }

    #[test]
    fn malformed_dates_are_rejected() {
        for value in [
            "",
            "   ",
            "2024-13-01",
            "2024-02-30",
            "not-a-date",
            "1748736000",
        ] {
            assert_eq!(normalise_timestamp(value), None, "{value}");
        }
    }

    #[test]
    fn normalised_timestamps_sort_in_time_order() {
        let a = normalise_timestamp("2024-01-01").unwrap();
        let b = normalise_timestamp("2025-01-01T00:00:00+00:00").unwrap();
        assert!(a < b);
    }
}
