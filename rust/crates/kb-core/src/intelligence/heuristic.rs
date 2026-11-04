//! The free, local, no-network entity extractor.
//!
//! # Why this exists
//!
//! `docs/architecture-target.md` §11 puts the entity graph on the retrieval
//! path, and the graph is only worth anything if the graph arm can be *measured*
//! against a labelled set. The default extractor is an OpenRouter call to
//! `openai/gpt-4.1-nano`, which means every experiment about the graph arm costs
//! money and needs an API key (AGENTS rule 7). This module is the zero-spend
//! option: pure functions over text, deterministic, no clock, no network.
//!
//! It is deliberately *not* an NER model. It is a small set of surface signals
//! that are cheap and good enough to build a graph worth walking:
//!
//! * **title-case runs** — "Bank Indonesia", "Sultan Agung", "Scott Derrickson".
//!   Indonesian proper nouns are capitalised the same way English ones are, so
//!   one signal covers both of the corpora this service runs on.
//! * **acronyms** — "BI", "DPTools", "KPU": an all-caps run of 2–8 letters, which
//!   in running text is almost never an ordinary word.
//! * **numbers with units** — "40 km", "Rp 5 miliar", "$2.3 billion". A
//!   multi-hop question about a number needs the number's unit-bearing phrase to
//!   be a node; a proper-noun-only extractor never sees it.
//!
//! # The part that matters more than the patterns: the dedupe key
//!
//! [`key_of`] folds case, whitespace, punctuation and diacritics, so `"Bank
//! Indonesia"`, `"bank  indonesia"` and `"Bank  Índonesia"` are one entity.
//! Without it the graph fills with near-duplicate nodes, PPR splits its mass
//! between them, and the arm looks broken for a reason that has nothing to do
//! with the walk. The same function normalises the query terms that seed a walk,
//! so a lowercased Indonesian question still finds a capitalised entity.
//!
//! # What it is not
//!
//! There are no typed relations here beyond co-occurrence. A heuristic that
//! guessed `WORKS_FOR` would be inventing facts, and §11's whole claim is that
//! graph RAG earns an extra arm only if the edges are real. Co-occurrence inside
//! one sentence, weighted by how often the pair is seen, is the one relation
//! signal a surface extractor can defend — and a sentence boundary keeps it from
//! degenerating into the complete graph, which has no structure to walk.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use super::types::{Entity, Relation};

/// Words that are capitalised for no reason: sentence-initial function words,
/// fixed phrase openers, and the structural vocabulary of a report. Indonesian
/// and English together, because the corpora are Indonesian and the questions
/// asked about them are too.
///
/// This set is the difference between a graph of forty real entities and a graph
/// of four hundred that are ninety per cent "Pada", "The" and "Table".
const STOPWORDS: &[&str] = &[
    // English function words
    "a",
    "about",
    "after",
    "again",
    "against",
    "all",
    "along",
    "also",
    "although",
    "am",
    "an",
    "and",
    "another",
    "any",
    "anyone",
    "are",
    "around",
    "as",
    "at",
    "away",
    "back",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "beside",
    "between",
    "both",
    "but",
    "by",
    "came",
    "can",
    "cannot",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "done",
    "down",
    "during",
    "each",
    "either",
    "else",
    "even",
    "ever",
    "every",
    "few",
    "first",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "however",
    "i",
    "if",
    "in",
    "including",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "later",
    "least",
    "less",
    "like",
    "made",
    "make",
    "many",
    "may",
    "might",
    "more",
    "most",
    "much",
    "must",
    "my",
    "myself",
    "neither",
    "never",
    "new",
    "next",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "often",
    "on",
    "once",
    "one",
    "only",
    "onto",
    "or",
    "other",
    "others",
    "our",
    "ours",
    "out",
    "over",
    "own",
    "per",
    "perhaps",
    "put",
    "rather",
    "same",
    "second",
    "see",
    "several",
    "shall",
    "she",
    "should",
    "since",
    "so",
    "some",
    "someone",
    "something",
    "soon",
    "still",
    "such",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "third",
    "this",
    "those",
    "though",
    "three",
    "through",
    "thus",
    "to",
    "together",
    "too",
    "toward",
    "under",
    "until",
    "up",
    "upon",
    "us",
    "very",
    "via",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "whether",
    "which",
    "while",
    "who",
    "whom",
    "whose",
    "why",
    "will",
    "with",
    "within",
    "without",
    "would",
    "yet",
    "you",
    "your",
    "yours",
    // Report scaffolding and titles
    "abstract",
    "appendix",
    "bibliography",
    "caption",
    "chapter",
    "conclusion",
    "contents",
    "figure",
    "figures",
    "footnote",
    "glossary",
    "index",
    "introduction",
    "note",
    "notes",
    "overview",
    "page",
    "preface",
    "references",
    "section",
    "summary",
    "table",
    "tables",
    // Indonesian function words
    "adalah",
    "agar",
    "akan",
    "antara",
    "apa",
    "atas",
    "atau",
    "bagai",
    "bagi",
    "bahkan",
    "bahwa",
    "banyak",
    "baru",
    "beberapa",
    "begitu",
    "belum",
    "benar",
    "berada",
    "berapa",
    "berbagai",
    "berikut",
    "bersama",
    "bertahun",
    "besar",
    "bila",
    "bisa",
    "buat",
    "bukan",
    "bulan",
    "cara",
    "cukup",
    "dalam",
    "dan",
    "dapat",
    "dari",
    "daripada",
    "dekat",
    "demi",
    "demikian",
    "dengan",
    "di",
    "dia",
    "diri",
    "dua",
    "dulu",
    "hingga",
    "hanya",
    "hari",
    "harus",
    "ia",
    "jadi",
    "jangan",
    "jika",
    "jikalau",
    "juga",
    "kalau",
    "kali",
    "kami",
    "kamu",
    "karena",
    "kata",
    "ke",
    "kemudian",
    "kepada",
    "kita",
    "lagi",
    "lain",
    "lalu",
    "lebih",
    "lewat",
    "maka",
    "makin",
    "mampu",
    "masih",
    "masing",
    "mau",
    "melalui",
    "melihat",
    "memang",
    "mempunyai",
    "mengapa",
    "mereka",
    "meski",
    "mungkin",
    "namun",
    "oleh",
    "orang",
    "para",
    "pasti",
    "per",
    "pernah",
    "pertama",
    "pula",
    "pun",
    "rupanya",
    "saat",
    "saja",
    "saling",
    "sama",
    "sambil",
    "sampai",
    "sangat",
    "satu",
    "saya",
    "sebagai",
    "sebelum",
    "secara",
    "sedang",
    "sehingga",
    "sejak",
    "sekali",
    "selain",
    "semua",
    "seni",
    "sepada",
    "seperti",
    "seraya",
    "serta",
    "sesuai",
    "setelah",
    "setiap",
    "siapa",
    "suatu",
    "sudah",
    "supaya",
    "tanpa",
    "tapi",
    "telah",
    "tentang",
    "terhadap",
    "tersebut",
    "tetapi",
    "tidak",
    "tiga",
    "untuk",
    "usai",
    "waktu",
    "walau",
    "walaupun",
    "yaitu",
    "yakni",
    "yang",
    // Verbs/nouns that appear capitalised at the start of an Indonesian clause
    "adanya",
    "akhir",
    "akhirnya",
    "ikut",
    "marga",
    "menurut",
    "pada",
    "pindah",
    "pun",
    "rata",
    "sana",
    "sini",
    "terpun",
    "umum",
    "utama",
    "yaitu",
    // Months and demonyms: capitalised, frequent, and never the thing a
    // multi-hop question asks about. Leaving them in is what turns a 40-entity
    // graph into a 400-entity one.
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "januari",
    "februari",
    "maret",
    "april",
    "mei",
    "juni",
    "juli",
    "agustus",
    "september",
    "oktober",
    "november",
    "desember",
    "american",
    "british",
    "english",
    "indonesian",
    "german",
    "french",
    "japanese",
    "chinese",
    "russian",
    "dutch",
    "italian",
    "spanish",
    "australian",
    "canadian",
    "european",
    "asian",
    "african",
    "american",
];

/// Lowercased stopwords, for a token test that has to survive capitalisation.
static STOPWORD_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| STOPWORDS.iter().copied().collect());

/// Units that turn a bare number into an entity. Matched case-insensitively as a
/// whole word, so "40 km" is a quantity and "40 kmh" is not.
const UNITS: &[&str] = &[
    "acre", "bab", "billion", "cm", "day", "days", "decade", "feet", "foot", "gr", "gram", "grams",
    "ha", "hectare", "hour", "hours", "inch", "juta", "kg", "km", "km/h", "kwh", "l", "mile",
    "miliar", "mm", "month", "months", "mph", "page", "percent", "ribu", "rupiah", "ton", "tonnes",
    "triliun", "year", "years", "yard",
];

/// Currency markers that precede rather than follow a number: "Rp 500", "$20".
const CURRENCY_MARKERS: &[&str] = &["rp", "rp.", "usd", "idr", "$"];

/// Name-internal connectors: lowercase words that are *part of* a name rather
/// than words next to it — "Bank of Indonesia", "Universitas Gadjah Mada".
///
/// Coordination is deliberately **not** here. `dan`, `and` and `atau` join two
/// different things ("Bank Indonesia dan Bank Mandiri"), and gluing them makes one
/// node out of two organisations, which is exactly the node no question will ever
/// mention and every walk would have to traverse past. So a coordinating word ends
/// the run instead: two entities, and a co-occurrence edge between them.
///
/// Indonesian `di` and `yang` are excluded for the same reason: in "Bank Mandiri
/// di Jakarta" the locative glues the organisation to the city it sits in, and the
/// graph gets a node per city branch instead of a node per organisation.
const CONNECTORS: &[&str] = &["de", "da", "del", "for", "of", "the", "van", "von"];

/// Longest title-case run, in words. "Universitas Gadjah Mada" is three; a run of
/// six words is a sentence fragment that happened to start with a capital.
const MAX_RUN_WORDS: usize = 6;

/// Longest word accepted inside a run, in characters.
const MAX_WORD_LEN: usize = 40;

/// Longest contiguous query phrase emitted as one seed term. Two covers the names
/// these corpora use ("Bank Indonesia", "Scott Derrickson") and stops before a
/// question's first three words become one phrase that matches no entity key and
/// dilutes every weight downstream.
const MAX_QUERY_NGRAM: usize = 2;

/// Knobs. Every one of them is about precision: a heuristic extractor that
/// over-fires produces a graph that is cheap to build and useless to walk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeuristicConfig {
    /// Surface forms shorter than this many normalised characters are dropped.
    pub min_key_len: usize,
    /// Cap on entities from one document, kept by descending frequency.
    pub max_entities: usize,
    /// Entities seen fewer than this many times are dropped.
    pub min_frequency: usize,
    /// Distinct entities considered for co-occurrence inside one sentence.
    pub max_entities_per_sentence: usize,
    /// Confidence of the least frequent entity kept, and the base of the range.
    pub min_confidence: f32,
    /// Confidence of the most frequent entity in the document.
    pub max_confidence: f32,
}

impl Default for HeuristicConfig {
    fn default() -> Self {
        Self {
            min_key_len: 3,
            max_entities: 512,
            min_frequency: 1,
            max_entities_per_sentence: 24,
            min_confidence: 0.30,
            max_confidence: 0.90,
        }
    }
}

/// The dedupe key: case, whitespace, punctuation and diacritics folded.
///
/// Returns `None` when nothing survives — a string of punctuation is not an
/// entity, and an empty key would collapse every such extraction onto one node.
///
/// This is the identity used by the graph store, by the entity upserts written at
/// ingest and by the query terms that seed a walk. It is lossy in exactly one
/// direction: `"Bank Indonesia"` and `"bank indonesia"` merge, and two genuinely
/// different entities that share a normalised key are *assumed* to be one. For
/// the corpora this runs on that trade is right, and it is stated here because
/// the alternative — a real canonicaliser — costs a model call.
#[must_use]
pub fn key_of(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut pending_space = false;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            push_folded(&mut out, ch);
        } else if !out.is_empty() {
            pending_space = true;
        }
    }
    let trimmed = out.trim_end();
    if trimmed.len() < 2 {
        return None;
    }
    Some(trimmed.to_string())
}

/// Push one character's base form, decomposed, into `out`.
///
/// A `match` rather than a table lookup so there is no allocation on the hot
/// path: this runs over every character of every token of every document.
fn push_folded(out: &mut String, ch: char) {
    match ch {
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => out.push('a'),
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
        'Æ' => {
            out.push('a');
            out.push('e');
        }
        'æ' => {
            out.push('a');
            out.push('e');
        }
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' => out.push('c'),
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => out.push('c'),
        'Ď' | 'Đ' | 'Ð' => out.push('d'),
        'ď' | 'đ' | 'ð' => out.push('d'),
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => out.push('e'),
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => out.push('g'),
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
        'Ĥ' | 'Ħ' => out.push('h'),
        'ĥ' | 'ħ' => out.push('h'),
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' => out.push('i'),
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
        'Ĵ' => out.push('j'),
        'ĵ' => out.push('j'),
        'Ķ' => out.push('k'),
        'ķ' => out.push('k'),
        'Ĺ' | 'Ļ' | 'Ľ' | 'Ŀ' | 'Ł' => out.push('l'),
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => out.push('l'),
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' => out.push('n'),
        'ñ' | 'ń' | 'ņ' | 'ň' => out.push('n'),
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ō' | 'Ŏ' | 'Ő' => out.push('o'),
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
        'Œ' => {
            out.push('o');
            out.push('e');
        }
        'œ' => {
            out.push('o');
            out.push('e');
        }
        'Ŕ' | 'Ŗ' | 'Ř' => out.push('r'),
        'ŕ' | 'ŗ' | 'ř' => out.push('r'),
        'Ś' | 'Ŝ' | 'Ş' | 'Š' => out.push('s'),
        'ś' | 'ŝ' | 'ş' | 'š' => out.push('s'),
        'Ţ' | 'Ť' | 'Ŧ' => out.push('t'),
        'ţ' | 'ť' | 'ŧ' => out.push('t'),
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => out.push('u'),
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
        'Ŵ' => out.push('w'),
        'ŵ' => out.push('w'),
        'Ý' | 'ſ' | 'Ÿ' => out.push('y'),
        'ý' | 'ÿ' | 'ŷ' => out.push('y'),
        'Ź' | 'Ż' | 'Ž' => out.push('z'),
        'ź' | 'ż' | 'ž' => out.push('z'),
        'Ø' => out.push('o'),
        'ø' => out.push('o'),
        'ß' => {
            out.push('s');
            out.push('s');
        }
        _ => out.extend(ch.to_lowercase()),
    }
}

/// One token of running text, with the offsets that make it a *word* rather than
/// a piece of punctuation.
#[derive(Debug, Clone)]
struct Token {
    text: String,
    /// Character offset of the first character, from the start of the text.
    start: usize,
    /// Number of characters in `text`.
    len: usize,
    /// First alphabetic character is uppercase.
    title: bool,
    /// Every alphabetic character is uppercase, 2–8 of them, no digits.
    acronym: bool,
}

/// Split text into alphanumeric tokens.
fn tokenize(text: &str) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let mut current = String::new();
    let mut start = 0usize;

    let flush = |out: &mut Vec<Token>, current: &mut String, start: usize| {
        if current.is_empty() {
            return;
        }
        let text = std::mem::take(current);
        let len = text.chars().count();
        let alpha: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
        let title = alpha.first().is_some_and(|c| c.is_uppercase());
        let all_upper = alpha.len() >= 2 && alpha.iter().all(|c| c.is_uppercase());
        let no_digits = !text.chars().any(|c| c.is_numeric());
        out.push(Token {
            len,
            text,
            start,
            title,
            acronym: all_upper && no_digits && (2..=8).contains(&alpha.len()),
        });
    };

    for (offset, ch) in text.chars().enumerate() {
        if ch.is_alphanumeric() || ch == '\'' || ch == '\u{2019}' {
            if current.is_empty() {
                start = offset;
            }
            current.push(ch);
        } else {
            flush(&mut out, &mut current, start);
        }
    }
    flush(&mut out, &mut current, start);
    out
}

/// A lowercased token's stopword membership.
fn is_stopword(token: &Token) -> bool {
    STOPWORD_SET.contains(token.text.to_lowercase().as_str())
}

/// A normalised key with no referent: too short, all digits, or a stopword.
fn is_generic(key: &str) -> bool {
    key.chars().count() < 3 || key.chars().all(|c| c.is_numeric()) || STOPWORD_SET.contains(key)
}

/// Every candidate surface form in `text`, before frequency filtering.
///
/// Duplicates are expected and wanted: the frequency is the signal, and the
/// caller counts them.
fn candidates(text: &str, cfg: &HeuristicConfig) -> Vec<String> {
    let tokens = tokenize(text);
    let mut out: Vec<String> = Vec::new();

    // Title-case runs. Scanned left to right; the longest run starting at a given
    // token is emitted, then the scan resumes after it, so "Bank Indonesia" wins
    // over the bare "Indonesia" inside it and "data pemilih" is not emitted again
    // once "data pemilih di" has been.
    let mut i = 0usize;
    while i < tokens.len() {
        if !is_word(&tokens[i]) {
            i += 1;
            continue;
        }
        let (parts, words) = run_from(&tokens, i);
        let end = parts.last().copied().unwrap_or(i);
        // A single-word run that is only an acronym is left to the acronym pass,
        // which finds those mid-sentence too.
        if words >= 2 || (words == 1 && !tokens[i].acronym) {
            push_surface(&mut out, &parts, &tokens, cfg);
        }
        i = end + 1;
    }

    // Acronyms anywhere, including mid-sentence and inside a longer run.
    for token in &tokens {
        if token.acronym {
            push_one(&mut out, token.text.clone(), cfg);
        }
    }

    out.extend(quantities(&tokens));
    out.extend(quoted(text));
    out
}

/// Is this token usable as a word of a name?
fn is_word(token: &Token) -> bool {
    token.title && token.len <= MAX_WORD_LEN && !is_stopword(token)
}

/// The tokens of the maximal title-case run starting at `from`, and how many of
/// them are words rather than connectors.
///
/// A connector ("of", "dan", "van") is only pulled in when a word follows it, so
/// a run never ends on one. Everything between two words must be at most one
/// character apart, which is what stops a run from crossing a clause boundary
/// that happens to have a capital after a full stop.
fn run_from(tokens: &[Token], from: usize) -> (Vec<usize>, usize) {
    let mut parts = vec![from];
    let mut words = 1usize;
    let mut cursor = from + 1;
    while cursor < tokens.len() && words < MAX_RUN_WORDS {
        let gap = tokens[cursor].start - (tokens[cursor - 1].start + tokens[cursor - 1].len);
        if gap > 1 {
            break;
        }
        if is_word(&tokens[cursor]) {
            parts.push(cursor);
            words += 1;
            cursor += 1;
            continue;
        }
        let lower = tokens[cursor].text.to_lowercase();
        if CONNECTORS.contains(&lower.as_str())
            && tokens
                .get(cursor + 1)
                .is_some_and(|next| gap_of(tokens, cursor + 1) <= 1 && is_word(next))
        {
            parts.push(cursor);
            cursor += 1;
            continue;
        }
        break;
    }
    (parts, words)
}

/// Characters between token `index` and the one before it.
fn gap_of(tokens: &[Token], index: usize) -> usize {
    tokens[index].start - (tokens[index - 1].start + tokens[index - 1].len)
}

fn push_surface(out: &mut Vec<String>, parts: &[usize], tokens: &[Token], cfg: &HeuristicConfig) {
    let surface = parts
        .iter()
        .map(|i| tokens[*i].text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    push_one(out, surface, cfg);
}

fn push_one(out: &mut Vec<String>, surface: String, cfg: &HeuristicConfig) {
    if let Some(key) = key_of(&surface)
        && key.chars().count() >= cfg.min_key_len
        && !is_generic(&key)
    {
        out.push(surface);
    }
}

/// Quoted spans, which in both corpora are titles of works and named things.
///
/// `"The Exorcism of Emily Rose"` is the whole name, and the run scanner cannot
/// see it: the leading article stops the run before "Exorcism". The leading
/// article is stripped so the quoted surface and the run surface normalise to the
/// same key instead of splitting one entity in two.
fn quoted(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    let mut buffer = String::new();
    while let Some(ch) = chars.next() {
        let close = match ch {
            '"' => Some('"'),
            '\u{201c}' => Some('\u{201d}'),
            '\u{00ab}' => Some('\u{00bb}'),
            _ => None,
        };
        let Some(close) = close else {
            continue;
        };
        buffer.clear();
        let mut closed = false;
        for inner in chars.by_ref() {
            if inner == close {
                closed = true;
                break;
            }
            if inner == '\n' {
                break;
            }
            buffer.push(inner);
        }
        if !closed || buffer.chars().count() < 3 {
            continue;
        }
        let trimmed = buffer.trim();
        let body = trimmed
            .split_once(' ')
            .filter(|(first, _)| STOPWORD_SET.contains(first.to_lowercase().as_str()))
            .map_or(trimmed, |(_, rest)| rest);
        out.push(body.to_string());
    }
    out
}

/// `"40 km"`, `"Rp 5 miliar"`, `"$20"` — a number next to a unit or a currency
/// marker, in whichever order the language puts them.
fn quantities(tokens: &[Token]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let next = tokens.get(i + 1);
        let starts_digit = token
            .text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit());
        if starts_digit
            && let Some(next) = next
            && is_unit(&next.text)
        {
            out.push(format!("{} {}", token.text, next.text));
            continue;
        }
        // Currency before the number, as Indonesian and English both write it:
        // "Rp 5 miliar", "$20".
        if !starts_digit
            && CURRENCY_MARKERS.contains(&token.text.to_lowercase().as_str())
            && next.is_some_and(|n| n.text.chars().next().is_some_and(|c| c.is_ascii_digit()))
            && let Some(next) = next
        {
            out.push(format!("{} {}", token.text, next.text));
        }
    }
    out
}

fn is_unit(token: &str) -> bool {
    let lower = token.to_lowercase();
    UNITS.contains(&lower.as_str())
}

/// One sentence of text. Co-occurrence needs a sentence boundary: a document-wide
/// window connects every entity to every other one, and that graph has no
/// structure for a walk to find.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if matches!(
            ch,
            '.' | '!' | '?' | '\n' | ';' | '。' | '！' | '？' | '；' | ':' | '،'
        ) {
            if current.chars().any(|c| c.is_alphanumeric()) {
                out.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.chars().any(|c| c.is_alphanumeric()) {
        out.push(current);
    }
    out
}

/// Extract entities and co-occurrence relations from `text`.
///
/// Entities are ordered by descending frequency then ascending key, which is
/// deterministic for a given text — the graph arm's seeds come from this order
/// when a query matches more entities than it can afford to seed.
#[must_use]
pub fn extract(
    text: &str,
    document_id: Option<&str>,
    chunk_id: Option<&str>,
    cfg: &HeuristicConfig,
) -> (Vec<Entity>, Vec<Relation>) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut display: HashMap<String, String> = HashMap::new();
    for surface in candidates(text, cfg) {
        let Some(key) = key_of(&surface) else {
            continue;
        };
        if is_generic(&key) {
            continue;
        }
        *counts.entry(key.clone()).or_insert(0) += 1;
        display.entry(key).or_insert_with(|| collapse(&surface));
    }

    let mut ranked: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(_, n)| *n >= cfg.min_frequency)
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(cfg.max_entities);

    let top = ranked.first().map_or(1, |(_, n)| *n);
    let span = cfg.max_confidence - cfg.min_confidence;
    let entities: Vec<Entity> = ranked
        .iter()
        .map(|(key, n)| {
            let name = display.get(key).cloned().unwrap_or_else(|| key.clone());
            let confidence = cfg.min_confidence + span * (*n as f32 / top as f32);
            Entity {
                entity_type: classify(&name),
                confidence: confidence.min(cfg.max_confidence),
                name,
                document_id: document_id.map(str::to_string),
                chunk_id: chunk_id.map(str::to_string),
                metadata: Some(serde_json::json!({
                    "key": key,
                    "count": n,
                    "extractor": "heuristic",
                })),
            }
        })
        .collect();

    // Co-occurrence inside one sentence.
    let mut pair_counts: HashMap<(String, String), usize> = HashMap::new();
    for sentence in sentences(text) {
        let mut unique: Vec<String> = Vec::new();
        for surface in candidates(&sentence, cfg) {
            let Some(key) = key_of(&surface) else {
                continue;
            };
            if is_generic(&key) {
                continue;
            }
            if !unique.contains(&key) {
                unique.push(key);
            }
        }
        if unique.len() < 2 {
            continue;
        }
        unique.truncate(cfg.max_entities_per_sentence);
        for i in 0..unique.len() {
            for other in (i + 1)..unique.len() {
                let pair = if unique[i] <= unique[other] {
                    (unique[i].clone(), unique[other].clone())
                } else {
                    (unique[other].clone(), unique[i].clone())
                };
                *pair_counts.entry(pair).or_insert(0) += 1;
            }
        }
    }

    let mut pairs: Vec<((String, String), usize)> = pair_counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let relations: Vec<Relation> = pairs
        .into_iter()
        .map(|((src, dst), n)| {
            // Saturating: a pair seen once in a short sentence is a weak edge,
            // and one seen ten times is not ten times stronger than one seen
            // three. A linear ramp keeps the weak tail from swamping the walk.
            let weight = (0.10f32 + 0.30 * ((n as f32 - 1.0) / 2.0)).clamp(0.10, 0.70);
            Relation {
                relation_type: "CO_OCCURS".to_string(),
                confidence: weight,
                source_entity: src.clone(),
                target_entity: dst.clone(),
                context: None,
                metadata: Some(serde_json::json!({
                    "source_entity": src,
                    "target_entity": dst,
                    "count": n,
                    "chunk_id": chunk_id,
                    "extractor": "heuristic",
                })),
            }
        })
        .collect();

    (entities, relations)
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A coarse type label from shape alone. Five buckets is enough: the walk does
/// not branch on type, and a confidently wrong label is worse than a coarse one.
fn classify(surface: &str) -> String {
    let t = surface.trim();
    let alpha: Vec<char> = t.chars().filter(|c| c.is_alphabetic()).collect();
    if alpha.len() >= 2
        && alpha.len() <= 8
        && alpha.iter().all(|c| c.is_uppercase())
        && !t.chars().any(|c| c.is_numeric())
    {
        return "Acronym".to_string();
    }
    if t.chars().next().is_some_and(|c| c.is_numeric()) {
        return "Quantity".to_string();
    }
    if alpha.len() <= 2 {
        return "Short".to_string();
    }
    match t.split_whitespace().count() {
        0 | 1 => "ProperNoun".to_string(),
        2 => "ProperNounPair".to_string(),
        _ => "MultiWord".to_string(),
    }
}

/// Query terms to seed a walk with, normalised exactly as [`key_of`] normalises
/// entities.
///
/// A query is a question, not a document: it carries no capitalisation signal
/// worth trusting, so the seeds are its normalised tokens. N-grams matter and are
/// the difference between the arm working and not: the entity is `"Bank
/// Indonesia"` and the question says "Bank Indonesia", and unigrams alone can only
/// ever match `Bank Indonesia`, `Bank Dunia`, `Bank Rakyat` equally well. The
/// longest phrase goes first so a caller that caps the list keeps precision.
///
/// Stopwords are kept in the token stream but dropped from the output. A question
/// word like "siapa" matches no entity key, and spending a slot on it only
/// dilutes the weights [`crate::graph`] will spread PPR over.
#[must_use]
pub fn query_terms(query: &str, max: usize) -> Vec<String> {
    let tokens: Vec<String> = tokenize(query)
        .iter()
        .map(|t| key_of(&t.text).unwrap_or_default())
        .collect();
    let limit = max.max(1);
    let mut out: Vec<String> = Vec::new();

    // Longest phrase first, then progressively shorter, so truncation keeps the
    // discriminative terms.
    let max_n = tokens.len().min(MAX_QUERY_NGRAM);
    for n in (1..=max_n).rev() {
        for window in tokens.windows(n) {
            let Some(phrase) = key_of(&window.join(" ")) else {
                continue;
            };
            if phrase.chars().count() < 3 || is_generic(&phrase) {
                continue;
            }
            // A phrase whose every word is a stopword is the question's own glue.
            if window.iter().all(|w| STOPWORD_SET.contains(w.as_str())) {
                continue;
            }
            if !out.contains(&phrase) {
                out.push(phrase);
            }
        }
    }
    out.truncate(limit);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn keys(entities: &[Entity]) -> Vec<String> {
        entities
            .iter()
            .filter_map(|e| {
                e.metadata
                    .as_ref()
                    .and_then(|m| m.get("key"))
                    .and_then(|k| k.as_str())
                    .map(str::to_string)
            })
            .collect()
    }

    fn relation_keys(relation: &Relation) -> (String, String) {
        let meta = relation.metadata.as_ref().unwrap();
        (
            meta.get("source_entity")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            meta.get("target_entity")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
        )
    }

    #[test]
    fn the_key_folds_case_whitespace_punctuation_and_diacritics() {
        assert_eq!(key_of("Bank Indonesia").as_deref(), Some("bank indonesia"));
        assert_eq!(
            key_of("bank   indonesia").as_deref(),
            Some("bank indonesia")
        );
        assert_eq!(
            key_of("  Bank\tIndonesia\n").as_deref(),
            Some("bank indonesia")
        );
        assert_eq!(key_of("Bank  Índonesia").as_deref(), Some("bank indonesia"));
        assert_eq!(key_of("Bank-Indonesia,").as_deref(), Some("bank indonesia"));
        assert_eq!(key_of("Sultan Agung").as_deref(), Some("sultan agung"));
        assert_eq!(key_of("Ærø").as_deref(), Some("aero"));
        assert_eq!(key_of("Malö").as_deref(), Some("malo"));
        assert_eq!(key_of("Ørsted").as_deref(), Some("orsted"));
        assert_eq!(key_of("Straße").as_deref(), Some("strasse"));
        // The one it must *not* destroy: a name with no accents stays as it is.
        assert_eq!(key_of("Sultan").as_deref(), Some("sultan"));
    }

    #[test]
    fn the_key_rejects_surfaces_with_nothing_left_in_them() {
        assert_eq!(key_of(""), None);
        assert_eq!(key_of("   "), None);
        assert_eq!(key_of("-- , ."), None);
        // One character is below the floor: an entity key of "a" would collide
        // with every other one-letter key in the tenant.
        assert_eq!(key_of("a"), None);
        assert_eq!(key_of("42"), Some("42".to_string()));
    }

    /// Cut from `datasets/miracl-id-200/corpus/`, verbatim. Indonesian is the
    /// corpus this service actually runs on, so an extractor that only works on
    /// English is not a working extractor.
    const MIRACL_ID: &str = "Pada tahun 2010, dikembangkan DPTools 2.0 agar dapat diinstall \
secara stand-alone di kabupaten-kabupaten yang menggunakannya untuk menyempurnakan Data Pemilih \
pada Pemilihan Kepala Daerah. DPTools 2.0 sukses membenahi Data Pemilih di berbagai \
Kota/Kabupaten.";

    #[test]
    fn indonesian_proper_nouns_and_acronyms_survive_real_corpus_text() {
        let (entities, _) = extract(MIRACL_ID, Some("doc"), None, &HeuristicConfig::default());
        let keys = keys(&entities);
        assert!(keys.contains(&"dptools".to_string()), "got {keys:?}");
        assert!(keys.contains(&"data pemilih".to_string()), "got {keys:?}");
        assert!(
            keys.contains(&"pemilihan kepala daerah".to_string()),
            "got {keys:?}"
        );
        // Sentence-initial function words and the lowercase connective must not.
        for noise in ["pada", "yang", "dengan", "agar", "secara"] {
            assert!(
                !keys.contains(&noise.to_string()),
                "{noise} leaked: {keys:?}"
            );
        }
    }

    /// Cut from the HotpotQA paragraph this repo's `datasets/hotpot-200` is built
    /// from, verbatim.
    const HOTPOT_EN: &str = "Scott Derrickson (born July 16, 1966) is an American director, \
screenwriter and producer. He lives in Los Angeles, California. He is best known for directing \
horror films such as \"Sinister\", \"The Exorcism of Emily Rose\", and \"Deliver Us From Evil\"";

    #[test]
    fn english_proper_nouns_survive_sentence_boundaries_and_quotes() {
        let (entities, _) = extract(HOTPOT_EN, Some("doc"), None, &HeuristicConfig::default());
        let keys = keys(&entities);
        for expected in [
            "scott derrickson",
            "los angeles",
            "california",
            "sinister",
            // The run scanner stops at the leading "The"; the quoted-span signal
            // is what recovers the whole title, with the article stripped so it
            // normalises onto the same key as the run would.
            "exorcism of emily rose",
            "deliver us from evil",
        ] {
            assert!(
                keys.iter().any(|k| k == expected),
                "missing {expected} in {keys:?}"
            );
        }
        for noise in [
            "he", "is", "born", "american", "such", "films", "best", "known",
        ] {
            assert!(
                !keys.contains(&noise.to_string()),
                "{noise} leaked: {keys:?}"
            );
        }
    }

    #[test]
    fn a_number_with_a_unit_is_an_entity_and_a_bare_number_is_not() {
        let (entities, _) = extract(
            "Jalan itu sepanjang 40 km, dikerjakan sejak 1998 dengan biaya Rp 5 miliar.",
            None,
            None,
            &HeuristicConfig::default(),
        );
        let keys = keys(&entities);
        assert!(keys.iter().any(|k| k.contains("40 km")), "got {keys:?}");
        assert!(keys.iter().any(|k| k.contains("5 miliar")), "got {keys:?}");
        assert!(
            !keys.contains(&"1998".to_string()),
            "bare year leaked: {keys:?}"
        );
    }

    #[test]
    fn relations_are_sentence_scoped_co_occurrence_and_never_self_loops() {
        let (entities, relations) = extract(
            "Bank Indonesia bekerja sama dengan Bank Mandiri di Jakarta. \
             Pusat Statistik Amerika publishes a quarterly bulletin.",
            None,
            Some("chunk-1"),
            &HeuristicConfig::default(),
        );
        assert!(!entities.is_empty());
        let keys = keys(&entities);
        assert!(
            keys.contains(&"bank indonesia".to_string())
                && keys.contains(&"bank mandiri".to_string()),
            "got {keys:?}"
        );
        for relation in &relations {
            assert_eq!(relation.relation_type, "CO_OCCURS");
            let (src, dst) = relation_keys(relation);
            assert_ne!(src, dst);
            assert!(
                (0.0..=1.0).contains(&relation.confidence),
                "weight {} out of range",
                relation.confidence
            );
        }
        // Two sentences sharing no entity produce no edge between them.
        let (_, isolated) = extract(
            "Alpha Institution berdiri tahun 1900. Zeta Foundation berdiri tahun 1910.",
            None,
            None,
            &HeuristicConfig::default(),
        );
        assert!(isolated.is_empty(), "got {isolated:?}");
    }

    #[test]
    fn a_pair_seen_in_three_sentences_is_a_stronger_edge_than_one_seen_once() {
        let text = "Bank Indonesia dan Bank Mandiri sering bekerja sama. \
                    Bank Indonesia dan Bank Mandiri menandatangani kerja sama. \
                    Bank Indonesia dan Bank Mandiri adalah mitra dagang.";
        let (_, relations) = extract(text, None, None, &HeuristicConfig::default());
        let strong = relations
            .iter()
            .find(|r| {
                let (s, d) = relation_keys(r);
                s == "bank indonesia" && d == "bank mandiri"
            })
            .expect("the repeated pair must be present");
        let (_, single) = extract(
            "Bank Indonesia dan Bank Mandiri sering bekerja sama.",
            None,
            None,
            &HeuristicConfig::default(),
        );
        let weak = single
            .iter()
            .find(|r| {
                let (s, d) = relation_keys(r);
                s == "bank indonesia" && d == "bank mandiri"
            })
            .expect("the pair must be present in the single sentence too");
        assert!(
            strong.confidence > weak.confidence,
            "thrice-seen {} must outrank once-seen {}",
            strong.confidence,
            weak.confidence
        );
    }

    #[test]
    fn extraction_is_deterministic_for_the_same_text() {
        let a = extract(MIRACL_ID, None, None, &HeuristicConfig::default());
        let b = extract(MIRACL_ID, None, None, &HeuristicConfig::default());
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, b.1);
    }

    #[test]
    fn the_two_capitalisations_of_a_name_are_one_entity() {
        let (entities, _) = extract(
            "Bank Indonesia publishes a report. Bank Indonesia_divisi publishes one too. \
             Bank Indonesia reviewed the report. BANK INDONESIA issued a statement.",
            None,
            None,
            &HeuristicConfig::default(),
        );
        let keys = keys(&entities);
        assert_eq!(
            keys.iter().filter(|k| *k == "bank indonesia").count(),
            1,
            "expected one merged key across four surface forms, got {keys:?}"
        );
        let merged = entities
            .iter()
            .find(|e| {
                e.metadata
                    .as_ref()
                    .and_then(|m| m.get("key"))
                    .and_then(|k| k.as_str())
                    == Some("bank indonesia")
            })
            .expect("the merged entity must exist");
        assert_eq!(
            merged
                .metadata
                .as_ref()
                .and_then(|m| m.get("count"))
                .and_then(serde_json::Value::as_u64),
            Some(4),
            "the merged entity must carry the summed count"
        );
    }

    #[test]
    fn query_terms_are_normalised_the_same_way_entities_are() {
        let terms = query_terms("Siapa pendiri Bank Indonesia?", 16);
        // Phrases before single words, so a truncated list keeps the terms that
        // discriminate between two entities rather than the ones they share.
        let phrases = terms.iter().filter(|t| t.contains(' ')).count();
        let unigrams = terms.len() - phrases;
        assert_eq!(phrases, 3, "every bigram of the question: {terms:?}");
        assert_eq!(
            unigrams, 3,
            "every content unigram of the question: {terms:?}"
        );
        assert!(
            terms[..phrases].iter().all(|t| t.contains(' ')),
            "{terms:?}"
        );
        assert!(terms.iter().any(|t| t == "bank indonesia"), "got {terms:?}");
        assert!(terms.iter().any(|t| t == "pendiri"), "got {terms:?}");
        assert!(
            terms.iter().all(|t| t.chars().all(|c| !c.is_uppercase())),
            "terms must be normalised: {terms:?}"
        );
        // The question's own glue earns no slot: "siapa" matches no entity key,
        // and a slot spent on it only dilutes the seed weights.
        assert!(!terms.iter().any(|t| t == "siapa"), "got {terms:?}");
    }

    #[test]
    fn a_stopword_only_document_produces_nothing_rather_than_a_useless_graph() {
        let (entities, relations) = extract(
            "The and of to in on at for. Pada yang dan dengan dari atau adalah dalam.",
            None,
            None,
            &HeuristicConfig::default(),
        );
        assert!(entities.is_empty(), "got {:?}", keys(&entities));
        assert!(relations.is_empty());
    }

    #[test]
    fn the_entity_cap_is_respected_and_keeps_the_most_frequent() {
        let text = (0..200)
            .map(|i| format!("Institution{i} published a report. Institution{i} spoke. "))
            .collect::<String>();
        let cfg = HeuristicConfig {
            max_entities: 10,
            ..HeuristicConfig::default()
        };
        let (entities, _) = extract(&text, None, None, &cfg);
        assert_eq!(entities.len(), 10);
    }

    #[test]
    fn an_empty_document_is_not_an_error() {
        let (entities, relations) = extract("", None, None, &HeuristicConfig::default());
        assert!(entities.is_empty());
        assert!(relations.is_empty());
    }
}
