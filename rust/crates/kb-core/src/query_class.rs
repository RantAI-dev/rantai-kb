//! Query classification for the relevance floor (RET-2).
//!
//! The floor must not throw away an exact match just because a dense embedding
//! thinks an identifier is "not similar" to the passage that contains it. That
//! is the whole reason a BM25 arm exists: a part number, a regulation code or a
//! quoted phrase is rare, and a vector averages rare tokens away.
//!
//! This module is pure: it reads a string and says whether the query looks like
//! an exact-match / identifier query, and which exact terms it contains. It
//! never decides whether a *hit* is exempt — that needs the hit's arm
//! provenance and lives in `kb-engine`, where the retrieval pipeline can ask
//! "did the keyword arm match, and does the text really contain this token?".
//!
//! # What counts as an identifier
//!
//! Deliberately conservative, because a false "exempt" lets an off-topic
//! question through the floor:
//!
//! * a quoted phrase (`"four seven"`, `“Bab 2”`) — the caller is asking for
//!   those exact words;
//! * a token that mixes letters and digits (`X200`, `RFC7231`), or has a
//!   `-`/`_`/`/` separator and a digit (`2024-11`, `order_42`, `AC/120`).
//!
//! A bare number (`42`, `2024`) is **not** an identifier: years and quantities
//! are ordinary natural-language content, and treating every year as an exact
//! identifier would disable the floor for far too many questions.

/// How a query should be treated by the relevance floor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueryClass {
    /// Ordinary natural-language question. The floor applies to every hit.
    #[default]
    Natural,
    /// Exact-match / identifier query. A hit the keyword arm matched *and*
    /// whose text contains one of [`QuerySignature::exact_terms`] or
    /// [`QuerySignature::quoted_phrases`] is exempt from the floor.
    Exact,
}

/// The pure classification of one query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuerySignature {
    pub class: QueryClass,
    /// Lower-cased identifier-like tokens, deduplicated, in first-seen order.
    pub exact_terms: Vec<String>,
    /// Lower-cased quoted phrases, whitespace-collapsed, deduplicated.
    pub quoted_phrases: Vec<String>,
}

impl QuerySignature {
    /// Whether this query asked for an exact match at all.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.class == QueryClass::Exact
    }

    /// Whether `content` literally contains one of this query's exact terms or
    /// quoted phrases.
    ///
    /// Tokens are matched on word boundaries, so `X200` does not match
    /// `X2000`, and `AC/120` does not match `xAC/1200`. A quoted phrase is
    /// matched as a lower-cased substring with collapsed whitespace.
    #[must_use]
    pub fn matches_content(&self, content: &str) -> bool {
        let haystack = content.to_lowercase();
        // Quoted phrases are stored whitespace-collapsed, so collapse the
        // content the same way before asking whether it contains one.
        let collapsed = haystack.split_whitespace().collect::<Vec<_>>().join(" ");
        self.quoted_phrases
            .iter()
            .any(|phrase| collapsed.contains(phrase))
            || self
                .exact_terms
                .iter()
                .any(|term| contains_word(&haystack, term))
    }
}

/// Classify `query`.
#[must_use]
pub fn classify_query(query: &str) -> QuerySignature {
    let quoted_phrases = quoted_phrases(query);
    let exact_terms = identifier_tokens(query);

    if quoted_phrases.is_empty() && exact_terms.is_empty() {
        QuerySignature {
            class: QueryClass::Natural,
            exact_terms,
            quoted_phrases,
        }
    } else {
        QuerySignature {
            class: QueryClass::Exact,
            exact_terms,
            quoted_phrases,
        }
    }
}

/// Every quoted phrase in the query, lower-cased and whitespace-collapsed.
fn quoted_phrases(query: &str) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut current = String::new();
    let mut inside = false;
    for ch in query.chars() {
        match ch {
            '"' | '“' | '”' => {
                if inside {
                    let phrase = normalize_phrase(&current);
                    if !phrase.is_empty() && !phrases.contains(&phrase) {
                        phrases.push(phrase);
                    }
                    current.clear();
                }
                inside = !inside;
            }
            c if inside => current.push(c),
            _ => {}
        }
    }
    phrases
}

fn normalize_phrase(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Every identifier-like token in the query, lower-cased and deduplicated.
fn identifier_tokens(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for raw in query.split_whitespace() {
        let token = raw
            .trim_matches(|c: char| {
                matches!(
                    c,
                    '.' | ','
                        | ';'
                        | ':'
                        | '!'
                        | '?'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '"'
                        | '“'
                        | '”'
                        | '‘'
                        | '’'
                        | '\''
                )
            })
            .to_lowercase();
        if !is_identifier(&token) {
            continue;
        }
        if !terms.contains(&token) {
            terms.push(token);
        }
    }
    terms
}

/// See the module docs: letters+digits, or a separator with a digit.
fn is_identifier(token: &str) -> bool {
    if token.len() < 3 {
        return false;
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/'))
    {
        return false;
    }
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    if !has_digit {
        return false;
    }
    let has_letter = token.chars().any(|c| c.is_ascii_alphabetic());
    let has_separator = token.chars().any(|c| matches!(c, '-' | '_' | '/'));
    has_letter || has_separator
}

/// `contains` on word boundaries, without a regex dependency.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(offset) = haystack[start..].find(needle) {
        let begin = start + offset;
        let end = begin + needle.len();
        let left_ok = haystack[..begin]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let right_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        if left_ok && right_ok {
            return true;
        }
        // Advance by one char so overlapping occurrences are considered.
        start = begin + haystack[begin..].chars().next().map_or(0, char::len_utf8);
        if start >= haystack.len() {
            break;
        }
    }
    false
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn natural_language_questions_are_not_exact() {
        for query in [
            "Siapakah penulis Candy Candy?",
            "berapa harga tiket bioskop saya minggu lalu",
            "how do I make sourdough rise faster",
            "Apa ibu kota provinsi Jawa Barat?",
        ] {
            let sig = classify_query(query);
            assert_eq!(sig.class, QueryClass::Natural, "{query}");
            assert!(sig.exact_terms.is_empty(), "{query}");
            assert!(sig.quoted_phrases.is_empty(), "{query}");
            assert!(!sig.is_exact(), "{query}");
        }
    }

    #[test]
    fn a_quoted_phrase_is_an_exact_query() {
        let sig = classify_query(r#"cari dokumen "Laporan Tahunan 2024" untuk saya"#);
        assert!(sig.is_exact());
        assert_eq!(sig.quoted_phrases, vec!["laporan tahunan 2024"]);
        assert!(sig.matches_content("LAPORAN   TAHUNAN 2024, halaman 3"));
        assert!(!sig.matches_content("laporan tahunan 2023"));
    }

    #[test]
    fn identifier_tokens_are_recognised_by_shape() {
        for (query, term) in [
            ("reset Acme X200 router", "x200"),
            ("what does RFC-7231 say", "rfc-7231"),
            ("check order_42 status", "order_42"),
            ("lihat berkas 2024-11", "2024-11"),
            ("endpoint /v1/search gagal", "/v1/search"),
        ] {
            let sig = classify_query(query);
            assert!(sig.is_exact(), "{query}");
            assert!(
                sig.exact_terms.contains(&term.to_string()),
                "{query}: {:?}",
                sig.exact_terms
            );
        }
    }

    #[test]
    fn bare_numbers_and_ordinary_words_are_not_identifiers() {
        for query in ["berapa 42 dikali 3", "tahun 2024", "versi dua"] {
            let sig = classify_query(query);
            assert_eq!(sig.class, QueryClass::Natural, "{query}: {sig:?}");
        }
    }

    #[test]
    fn tokens_match_on_word_boundaries_not_substrings() {
        let sig = classify_query("model X200");
        assert!(sig.matches_content("The X200 router resets with a pin."));
        assert!(!sig.matches_content("The X2000 router is newer."));

        let sig = classify_query("berkas AC/120");
        assert!(sig.matches_content("lihat AC/120 di lampiran"));
        assert!(!sig.matches_content("lihat xAC/1200 di lampiran"));
    }

    #[test]
    fn punctuation_around_a_token_does_not_stop_the_match() {
        let sig = classify_query("kode (X200), tolong cek.");
        assert_eq!(sig.exact_terms, vec!["x200"]);
        assert!(sig.matches_content("produk X200: panduan"));
    }

    #[test]
    fn an_unclosed_quote_is_not_a_phrase() {
        let sig = classify_query(r#"cari "laporan tahunan"#);
        assert!(sig.quoted_phrases.is_empty());
        assert_eq!(sig.class, QueryClass::Natural);
    }

    #[test]
    fn duplicate_terms_are_listed_once_in_first_seen_order() {
        let sig = classify_query("X200 lalu X200 lagi");
        assert_eq!(sig.exact_terms, vec!["x200"]);
    }
}
