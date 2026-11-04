//! Query classification for two consumers: the relevance floor (RET-2) and
//! adaptive arm choice (RET-5).
//!
//! The floor must not throw away an exact match just because a dense embedding
//! thinks an identifier is "not similar" to the passage that contains it. That
//! is the whole reason a BM25 arm exists: a part number, a regulation code or a
//! quoted phrase is rare, and a vector averages rare tokens away.
//!
//! Adaptive arm choice asks a different question about the same text: which
//! retrieval arms can this *kind* of question possibly need? A page-image arm
//! cannot help a part-number lookup, and an entity graph cannot answer "what
//! does chapter 2 say". Skipping such an arm is where the latency win comes
//! from. Every skipped arm is named in the response, so a client can see the
//! decision rather than wonder why a plane did not run.
//!
//! This module is pure: it reads a string and returns the class, the exact
//! terms, and the [`ArmPlan`] that class implies. It never decides whether a
//! *hit* is exempt from the floor or whether a skipped arm should be retried —
//! both need data from the pipeline and live in `kb-engine`.
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
//!
//! # The class → arm table (RET-5)
//!
//! The rule is: only skip an arm where it is *clearly* useless, because a
//! wrong skip costs recall and the latency budget is spent on every query, not
//! only the ones that would not have used the arm anyway.
//!
//! | class | dense | sparse | page image | figure | neighbour | graph | rerank |
//! | --- | --- | --- | --- | --- | --- | --- | --- |
//! | `exact` | yes | yes | **no** | yes | yes | **no** | yes |
//! | `factual` | yes | yes | yes | yes | yes | yes | yes |
//! | `structural` | yes | yes | yes | yes | yes | **no** | yes |
//! | `relational` | yes | yes | yes | yes | yes | yes | yes |
//! | `aggregative` | yes | yes | **no** | **no** | yes | yes | yes |
//! | `topical` | yes | yes | yes | yes | yes | yes | yes |
//!
//! Why these, and nothing more:
//!
//! * **exact** — an identifier or quoted phrase lives in text. A page image
//!   (a picture of a page) and an entity walk cannot match a part number
//!   literally; the sparse arm is the one that does. Figures stay: a diagram
//!   caption can carry the identifier.
//! * **structural** — "chapter 2", "section 3.1", "page 12" are places in a
//!   document, so the page-image arm and neighbours stay (scanned pages and
//!   adjacent chunks are exactly where the answer is). An entity graph knows
//!   nothing about page numbers.
//! * **relational** — the graph arm is the whole point ("relationship between
//!   X and Y", "who worked with ..."), so nothing is skipped.
//! * **aggregative** — "main themes", "list every", "how many documents" want
//!   coverage across the corpus, not one page image or one figure. The text
//!   arms, neighbours and the graph (for entity-level themes) stay.
//! * **factual** / **topical** — the arms have no clear loser, so the plan is
//!   exactly today's pipeline. Adaptive mode must never make these slower or
//!   worse.

/// How a query should be treated by the relevance floor and by adaptive arm
/// choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum QueryClass {
    /// Identifier, code or quoted phrase: an exact string is being asked for.
    Exact,
    /// A direct question about a fact: who, what, when, where, why, how.
    Factual,
    /// A question about a place in a document: chapter, section, table, page.
    Structural,
    /// A question about how two things are connected.
    Relational,
    /// A question across the whole corpus: themes, counts, "list every".
    Aggregative,
    /// Ordinary natural-language query with no stronger cue.
    #[default]
    Topical,
}

impl QueryClass {
    /// Every class, in the order the documentation lists them.
    pub const ALL: [Self; 6] = [
        Self::Exact,
        Self::Factual,
        Self::Structural,
        Self::Relational,
        Self::Aggregative,
        Self::Topical,
    ];

    /// The wire spelling, matching `stats.queryClass`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Factual => "factual",
            Self::Structural => "structural",
            Self::Relational => "relational",
            Self::Aggregative => "aggregative",
            Self::Topical => "topical",
        }
    }

    /// The reason recorded on each arm this class skips.
    #[must_use]
    pub fn skip_reason(self) -> &'static str {
        match self {
            Self::Exact => "exact query",
            Self::Factual => "factual query",
            Self::Structural => "structural query",
            Self::Relational => "relational query",
            Self::Aggregative => "aggregative query",
            Self::Topical => "topical query",
        }
    }
}

/// One retrieval arm adaptive mode can turn on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arm {
    Dense,
    Sparse,
    PageImage,
    Figure,
    Neighbour,
    Graph,
    Rerank,
}

impl Arm {
    /// The wire spelling used by `stats.skippedArms[].arm`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dense => "dense",
            Self::Sparse => "sparse",
            Self::PageImage => "pageImage",
            Self::Figure => "figure",
            Self::Neighbour => "neighbour",
            Self::Graph => "graph",
            Self::Rerank => "rerank",
        }
    }
}

/// An arm adaptive mode did not run, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedArm {
    pub arm: Arm,
    pub reason: &'static str,
}

/// Which arms run for one query.
///
/// The field names deliberately match the pipeline's stages rather than the
/// response, so a reader can check the plan against `retrieval.rs` line by line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmPlan {
    pub dense: bool,
    pub sparse: bool,
    pub page_image: bool,
    pub figure: bool,
    pub neighbour: bool,
    pub graph: bool,
    pub rerank: bool,
}

impl ArmPlan {
    /// Every arm, i.e. exactly the pipeline that ran before RET-5.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            dense: true,
            sparse: true,
            page_image: true,
            figure: true,
            neighbour: true,
            graph: true,
            rerank: true,
        }
    }

    /// The plan for one question class. See the module docs for the table and
    /// the reason behind each skip.
    #[must_use]
    pub const fn for_class(class: QueryClass) -> Self {
        match class {
            QueryClass::Exact => Self {
                page_image: false,
                graph: false,
                ..Self::all()
            },
            QueryClass::Structural => Self {
                graph: false,
                ..Self::all()
            },
            QueryClass::Aggregative => Self {
                page_image: false,
                figure: false,
                ..Self::all()
            },
            QueryClass::Factual | QueryClass::Relational | QueryClass::Topical => Self::all(),
        }
    }

    /// The arms this plan turns off, each with the class's reason. Empty when
    /// the plan is the full pipeline.
    #[must_use]
    pub fn skipped(self, class: QueryClass) -> Vec<SkippedArm> {
        let reason = class.skip_reason();
        let mut skipped = Vec::new();
        let mut push = |enabled: bool, arm: Arm| {
            if !enabled {
                skipped.push(SkippedArm { arm, reason });
            }
        };
        push(self.dense, Arm::Dense);
        push(self.sparse, Arm::Sparse);
        push(self.page_image, Arm::PageImage);
        push(self.figure, Arm::Figure);
        push(self.neighbour, Arm::Neighbour);
        push(self.graph, Arm::Graph);
        push(self.rerank, Arm::Rerank);
        skipped
    }

    /// Whether this plan turns off an arm that can produce candidates on its
    /// own (dense, sparse, page image, graph). Figures and neighbours only
    /// attach context to hits the other arms already returned, and the reranker
    /// only reorders them, so skipping those cannot empty a search.
    #[must_use]
    pub const fn skips_candidate_arm(self) -> bool {
        !self.dense || !self.sparse || !self.page_image || !self.graph
    }
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

    /// Which arms this query's class needs.
    #[must_use]
    pub fn arm_plan(&self) -> ArmPlan {
        ArmPlan::for_class(self.class)
    }

    /// The arms adaptive mode would skip for this query, with reasons.
    #[must_use]
    pub fn skipped_arms(&self) -> Vec<SkippedArm> {
        self.arm_plan().skipped(self.class)
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

    // Exact wins outright: an identifier or a quoted phrase is the strongest
    // signal in the query, and no other class may take it away (RET-2's floor
    // exemption depends on this).
    let class = if !quoted_phrases.is_empty() || !exact_terms.is_empty() {
        QueryClass::Exact
    } else {
        classify_cues(query)
    };

    QuerySignature {
        class,
        exact_terms,
        quoted_phrases,
    }
}

/// Classify by cue words, in the order the plans care about.
///
/// The order is a precedence: an aggregative question that names a chapter is
/// still aggregative ("how many documents discuss chapter 2"), and a relational
/// question that starts with "who" is relational, not a plain fact question.
fn classify_cues(query: &str) -> QueryClass {
    let words = cue_words(query);
    if is_aggregative(&words) {
        QueryClass::Aggregative
    } else if is_structural(&words) {
        QueryClass::Structural
    } else if is_relational(&words) {
        QueryClass::Relational
    } else if is_factual(&words) {
        QueryClass::Factual
    } else {
        QueryClass::Topical
    }
}

/// Lower-cased cue tokens with surrounding punctuation stripped. Internal
/// `-`, `'` and `/` survive, so `X200` and `2024-11` stay one token.
fn cue_words(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|raw| {
            raw.trim_matches(|c: char| {
                !(c.is_alphanumeric() || matches!(c, '-' | '\'' | '/' | '_'))
            })
            .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Words that name a place in a document.
const STRUCTURAL_LABELS: &[&str] = &[
    "chapter", "chapters", "section", "sections", "appendix", "table", "tables", "figure",
    "figures", "page", "pages", "bab", "bagian", "lampiran", "apendiks", "tabel", "gambar",
    "halaman", "pasal", "ayat", "subbab",
];

/// A following token that looks like a section/table/page number.
fn is_number_like(word: &str) -> bool {
    const NUMBER_WORDS: &[&str] = &[
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "first",
        "second", "third", "fourth", "fifth", "satu", "dua", "tiga", "empat", "lima", "enam",
        "tujuh", "delapan", "sembilan", "sepuluh", "pertama", "kedua", "ketiga",
    ];
    word.chars().any(|c| c.is_ascii_digit()) || NUMBER_WORDS.contains(&word)
}

fn is_structural(words: &[String]) -> bool {
    for (index, word) in words.iter().enumerate() {
        if !STRUCTURAL_LABELS.contains(&word.as_str()) {
            continue;
        }
        // "in the appendix" is structural on its own; "appendix" without a
        // number is still a place in the document.
        if matches!(word.as_str(), "appendix" | "lampiran" | "apendiks") {
            return true;
        }
        // "table of contents" is not a request for table N.
        if matches!(word.as_str(), "table" | "tabel")
            && words
                .get(index + 1)
                .is_some_and(|next| matches!(next.as_str(), "of" | "dari"))
        {
            continue;
        }
        let window = &words[index + 1..words.len().min(index + 3)];
        if window.iter().any(|next| is_number_like(next)) {
            return true;
        }
    }
    false
}

/// Cue words that mean "how are these connected".
const RELATION_WORDS: &[&str] = &[
    "relationship",
    "relationships",
    "relation",
    "relations",
    "connection",
    "connections",
    "hubungan",
    "relasi",
    "keterkaitan",
    "koneksi",
];

fn is_relational(words: &[String]) -> bool {
    if words
        .iter()
        .any(|word| RELATION_WORDS.contains(&word.as_str()))
    {
        return true;
    }
    // "between X and Y" / "antara X dan Y" is the shape of a comparison of two
    // things. It also catches "difference between A and B", which is harmless:
    // the relational plan runs every arm.
    let between = words
        .iter()
        .any(|word| matches!(word.as_str(), "between" | "antara"));
    let conjunction = words
        .iter()
        .any(|word| matches!(word.as_str(), "and" | "dan"));
    if between && conjunction {
        return true;
    }
    // "who ... with ..." / "siapa ... dengan ...".
    let who = words
        .iter()
        .any(|word| matches!(word.as_str(), "who" | "siapa"));
    let with = words
        .iter()
        .any(|word| matches!(word.as_str(), "with" | "dengan"));
    who && with
}

/// Phrases that ask across the whole corpus rather than for one passage.
const AGGREGATIVE_PHRASES: &[&str] = &[
    "main theme",
    "main themes",
    "primary theme",
    "primary themes",
    "tema utama",
    "tema-tema utama",
    "summarize all",
    "summarise all",
    "summarize everything",
    "summarise everything",
    "summary of all",
    "ringkas semua",
    "ringkasan semua",
    "meringkas semua",
    "rangkum semua",
    "list all",
    "list every",
    "daftar semua",
    "sebutkan semua",
    "sebutkan seluruh",
    "sebut semua",
    "how many documents",
    "how many files",
    "how many entries",
    "berapa banyak dokumen",
    "berapa banyak berkas",
    "berapa banyak entri",
    "all documents",
    "all the documents",
    "every document",
    "all files",
    "semua dokumen",
    "seluruh dokumen",
    "semua berkas",
    "in general",
    "secara umum",
    "keseluruhan",
];

fn is_aggregative(words: &[String]) -> bool {
    let joined = words.join(" ");
    AGGREGATIVE_PHRASES
        .iter()
        .any(|phrase| joined.contains(phrase))
}

/// Question openers in both languages. A question that names no stronger cue
/// is factual; the plan is the full pipeline either way, so this label is
/// descriptive first.
const QUESTION_WORDS: &[&str] = &[
    "what",
    "who",
    "whom",
    "whose",
    "when",
    "where",
    "which",
    "why",
    "how",
    "is",
    "are",
    "was",
    "were",
    "does",
    "do",
    "did",
    "can",
    "could",
    "should",
    "would",
    "will",
    "apa",
    "apakah",
    "siapa",
    "kapan",
    "dimana",
    "mengapa",
    "kenapa",
    "bagaimana",
    "berapa",
    "adakah",
    "jelaskan",
    "terangkan",
    "sebutkan",
    "carilah",
    "cari",
    "definisikan",
];

fn is_factual(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| QUESTION_WORDS.contains(&word.as_str()))
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

    fn class_of(query: &str) -> QueryClass {
        classify_query(query).class
    }

    #[test]
    fn english_examples_per_class() {
        for (query, class) in [
            ("reset Acme X200 router", QueryClass::Exact),
            (
                r#"find the page titled "Annual Report 2024""#,
                QueryClass::Exact,
            ),
            ("who wrote the annual report?", QueryClass::Factual),
            (
                "what does chapter 2 say about funding?",
                QueryClass::Structural,
            ),
            ("summarise section 3.1", QueryClass::Structural),
            ("what is in table 4", QueryClass::Structural),
            ("in the appendix", QueryClass::Structural),
            (
                "what is the relationship between Alpha and Beta?",
                QueryClass::Relational,
            ),
            ("who worked with Alpha Corporation?", QueryClass::Relational),
            (
                "what are the main themes across these documents?",
                QueryClass::Aggregative,
            ),
            (
                "list every document that mentions the merger",
                QueryClass::Aggregative,
            ),
            (
                "how many documents discuss the merger?",
                QueryClass::Aggregative,
            ),
            ("café recommendations near the station", QueryClass::Topical),
        ] {
            assert_eq!(class_of(query), class, "{query}");
        }
    }

    #[test]
    fn indonesian_examples_per_class() {
        for (query, class) in [
            ("reset router Acme X200", QueryClass::Exact),
            (r#"cari dokumen "Laporan Tahunan 2024""#, QueryClass::Exact),
            ("Siapa penulis Candy Candy?", QueryClass::Factual),
            ("apa isi bab 2?", QueryClass::Structural),
            ("jelaskan bagian 3.1", QueryClass::Structural),
            ("tabel 4 berisi apa", QueryClass::Structural),
            ("halaman 12", QueryClass::Structural),
            ("apa yang ada di lampiran", QueryClass::Structural),
            (
                "apa hubungan antara Alpha dan Beta?",
                QueryClass::Relational,
            ),
            ("siapa yang bekerja dengan Alpha?", QueryClass::Relational),
            ("apa tema utama dokumen ini?", QueryClass::Aggregative),
            (
                "sebutkan semua dokumen tentang merger",
                QueryClass::Aggregative,
            ),
            (
                "berapa banyak dokumen yang membahas merger?",
                QueryClass::Aggregative,
            ),
            ("harga tiket bioskop minggu lalu", QueryClass::Topical),
        ] {
            assert_eq!(class_of(query), class, "{query}");
        }
    }

    #[test]
    fn ordinary_language_is_not_exact() {
        for query in [
            "Siapakah penulis Candy Candy?",
            "berapa harga tiket bioskop saya minggu lalu",
            "how do I make sourdough rise faster",
            "Apa ibu kota provinsi Jawa Barat?",
        ] {
            let sig = classify_query(query);
            assert!(!sig.is_exact(), "{query}");
            assert!(sig.exact_terms.is_empty(), "{query}");
            assert!(sig.quoted_phrases.is_empty(), "{query}");
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
            assert!(!sig.is_exact(), "{query}: {sig:?}");
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
        assert!(!sig.is_exact());
    }

    #[test]
    fn duplicate_terms_are_listed_once_in_first_seen_order() {
        let sig = classify_query("X200 lalu X200 lagi");
        assert_eq!(sig.exact_terms, vec!["x200"]);
    }

    #[test]
    fn exact_skips_only_the_page_image_and_graph_arms() {
        let sig = classify_query("part X200");
        assert_eq!(sig.class, QueryClass::Exact);
        let plan = sig.arm_plan();
        assert!(!plan.page_image);
        assert!(!plan.graph);
        assert!(plan.dense && plan.sparse && plan.figure && plan.neighbour && plan.rerank);
        let skipped = sig.skipped_arms();
        assert_eq!(
            skipped,
            vec![
                SkippedArm {
                    arm: Arm::PageImage,
                    reason: "exact query"
                },
                SkippedArm {
                    arm: Arm::Graph,
                    reason: "exact query"
                },
            ]
        );
    }

    #[test]
    fn the_conservative_classes_run_every_arm() {
        for query in [
            "who wrote the report?",
            "what is the relationship between Alpha and Beta?",
            "café recommendations near the station",
        ] {
            let sig = classify_query(query);
            assert_eq!(sig.arm_plan(), ArmPlan::all(), "{query}");
            assert!(sig.skipped_arms().is_empty(), "{query}");
        }
    }

    #[test]
    fn structural_skips_the_graph_only_and_says_why() {
        let sig = classify_query("what does chapter 2 say?");
        assert_eq!(sig.class, QueryClass::Structural);
        assert!(!sig.arm_plan().graph);
        assert!(sig.arm_plan().page_image);
        assert_eq!(
            sig.skipped_arms(),
            vec![SkippedArm {
                arm: Arm::Graph,
                reason: "structural query"
            }]
        );
    }

    #[test]
    fn aggregative_skips_the_page_image_and_figure_arms() {
        let sig = classify_query("what are the main themes?");
        assert_eq!(sig.class, QueryClass::Aggregative);
        assert!(!sig.arm_plan().page_image);
        assert!(!sig.arm_plan().figure);
        assert!(sig.arm_plan().neighbour && sig.arm_plan().graph);
        assert_eq!(
            sig.skipped_arms(),
            vec![
                SkippedArm {
                    arm: Arm::PageImage,
                    reason: "aggregative query"
                },
                SkippedArm {
                    arm: Arm::Figure,
                    reason: "aggregative query"
                },
            ]
        );
    }

    #[test]
    fn only_candidate_producing_skips_can_empty_a_search() {
        assert!(ArmPlan::for_class(QueryClass::Exact).skips_candidate_arm());
        assert!(ArmPlan::for_class(QueryClass::Structural).skips_candidate_arm());
        assert!(ArmPlan::for_class(QueryClass::Aggregative).skips_candidate_arm());
        assert!(!ArmPlan::all().skips_candidate_arm());
    }

    #[test]
    fn the_wire_names_are_stable() {
        let names: Vec<&str> = QueryClass::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "exact",
                "factual",
                "structural",
                "relational",
                "aggregative",
                "topical"
            ]
        );
        let arms: Vec<&str> = [
            Arm::Dense,
            Arm::Sparse,
            Arm::PageImage,
            Arm::Figure,
            Arm::Neighbour,
            Arm::Graph,
            Arm::Rerank,
        ]
        .iter()
        .map(|arm| arm.as_str())
        .collect();
        assert_eq!(
            arms,
            vec![
                "dense",
                "sparse",
                "pageImage",
                "figure",
                "neighbour",
                "graph",
                "rerank"
            ]
        );
    }
}
