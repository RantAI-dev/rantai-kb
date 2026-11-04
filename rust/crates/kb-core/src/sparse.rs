//! Keyword (sparse) vector encoding.
//!
//! One tokenizer, used for documents and for queries, so a query term hashes to
//! the same id as the same term in a document. This replaces SurrealDB's
//! `kb_en` analyzer, whose English Snowball stemmer mapped `Candy` → `candi`
//! (which collides with Indonesian temples) and `menulis` → `menuli`. See
//! `docs/findings-bm25-collapse.md`.
//!
//! Deliberate omissions:
//!
//! * **No stemmer.** Indonesian stemming is a measured experiment, not a guess;
//!   see `docs/eval-baseline.md` before adding one.
//! * **No crate for the term hash.** FNV-1a 32-bit is 10 lines and pinned by
//!   `term_id_is_stable`. A hashing crate could change its output between
//!   versions, which would silently invalidate every stored sparse vector.
//!
//! Document values are BM25 *saturated* term frequencies, the way Qdrant's
//! fastembed `Bm25` encodes them:
//!
//! ```text
//! tf * (k1 + 1) / (tf + k1 * (1 - b + b * dl / avgdl))
//! ```
//!
//! IDF is deliberately absent: the index applies it (Qdrant's
//! `Modifier::Idf`), because only the index knows the corpus it is scoring
//! against.

use std::sync::LazyLock;

use fancy_regex::Regex;

/// `\w+` over Unicode, the same segmentation the independent `rank_bm25`
/// reference used when it scored 0.744 nDCG@10 against our 0.414.
static WORD: LazyLock<Regex> = LazyLock::new(|| match Regex::new(r"\w+") {
    Ok(r) => r,
    Err(_) => std::process::abort(),
});

/// BM25 term-frequency saturation `k1`. The value SurrealDB's `BM25(1.2, 0.75)`
/// index and Lucene both default to.
pub const BM25_K1: f32 = 1.2;

/// BM25 length-normalisation `b`. Same default as Lucene.
pub const BM25_B: f32 = 0.75;

/// Default average document length, in tokens. Measured `avgdl` for a chunked
/// corpus moves the numbers by a few percent; Qdrant's fastembed BM25 encoder
/// defaults to the same number.
pub const BM25_AVGDL: f32 = 256.0;

/// A sparse vector: parallel `indices` and `values`, both sorted by ascending
/// index. Sorted order is what makes the encoding comparable between documents
/// and what makes a stored vector portable to another engine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SparseVector {
    /// Term ids (`term_id` of the folded token), strictly ascending.
    pub indices: Vec<u32>,
    /// One weight per index, in the same position.
    pub values: Vec<f32>,
}

impl SparseVector {
    /// An empty vector. A document with no tokens encodes to this.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            indices: Vec::new(),
            values: Vec::new(),
        }
    }

    /// True when the vector has no terms.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Number of terms.
    #[must_use]
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    /// Assemble a vector from unsorted `(index, value)` pairs: sort by index,
    /// then merge hash collisions by summing their values. Summing is the only
    /// deterministic rule available for two distinct tokens that collided, and
    /// it is stable across runs, which is what the index needs.
    #[must_use]
    pub fn from_pairs(mut pairs: Vec<(u32, f32)>) -> Self {
        pairs.sort_unstable_by_key(|(i, _)| *i);
        let mut indices: Vec<u32> = Vec::with_capacity(pairs.len());
        let mut values: Vec<f32> = Vec::with_capacity(pairs.len());
        for (index, value) in pairs {
            if !value.is_finite() || value == 0.0 {
                continue;
            }
            match indices.last() {
                Some(last) if *last == index => {
                    let last = values.len() - 1;
                    values[last] += value;
                }
                _ => {
                    indices.push(index);
                    values.push(value);
                }
            }
        }
        Self { indices, values }
    }
}

/// Split `text` into folded terms: Unicode word characters, lowercased, with
/// Latin diacritics folded to ASCII.
///
/// "Candy Candy?" → `["candy", "candy"]`. Punctuation is a separator, never a
/// term; SurrealDB's `TOKENIZERS class` kept `?` as a token, and no document
/// contains it.
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    for m in WORD.find_iter(text).flatten() {
        let word = m.as_str();
        current.clear();
        for ch in word.chars() {
            let lowered: Vec<char> = ch.to_lowercase().collect();
            for c in lowered {
                if let Some(ascii) = fold_to_ascii(c) {
                    current.push(ascii);
                } else if c.is_alphanumeric() {
                    // A script with no ASCII fold (CJK, Cyrillic, …) keeps its
                    // own characters: dropping it would delete the document's
                    // only terms. Query and document take the same path, so the
                    // ids still match.
                    current.push(c);
                }
            }
        }
        if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    terms
}

/// A stable 32-bit id for one folded term.
///
/// FNV-1a 32-bit. Pinned by `term_id_is_stable`: these exact values must never
/// change, because every sparse vector already stored under them would silently
/// stop matching.
///
/// | term | id |
/// |---|---|
/// | `candy` | 4057490526 |
/// | `menulis` | 2990282374 |
/// | `ibukota` | 2088823824 |
#[must_use]
pub fn term_id(term: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5; // FNV offset basis
    for byte in term.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193); // FNV prime
    }
    hash
}

/// BM25 term-frequency saturation.
///
/// `tf * (k1 + 1) / (tf + k1 * (1 - b + b * dl / avgdl))`
#[must_use]
pub fn saturate_tf(tf: f32, doc_len: u32, avgdl: f32) -> f32 {
    let k1 = BM25_K1;
    let b = BM25_B;
    let dl = doc_len.max(1) as f32;
    let avgdl = if avgdl > 0.0 { avgdl } else { BM25_AVGDL };
    tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * dl / avgdl))
}

/// Encode a document for the sparse index. `avgdl` defaults to
/// [`BM25_AVGDL`]; pass the corpus's measured average when you have one.
#[must_use]
pub fn encode_document(text: &str) -> SparseVector {
    encode_document_avgdl(text, BM25_AVGDL)
}

/// [`encode_document`] with an explicit average document length.
#[must_use]
pub fn encode_document_avgdl(text: &str, avgdl: f32) -> SparseVector {
    let terms = tokenize(text);
    if terms.is_empty() {
        return SparseVector::empty();
    }
    let doc_len = u32::try_from(terms.len()).unwrap_or(u32::MAX);
    let mut counts: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for term in &terms {
        *counts.entry(term.as_str()).or_insert(0) += 1;
    }
    let pairs: Vec<(u32, f32)> = counts
        .into_iter()
        .map(|(term, tf)| (term_id(term), saturate_tf(tf as f32, doc_len, avgdl)))
        .collect();
    SparseVector::from_pairs(pairs)
}

/// Encode a query for the sparse index: every unique term, value 1.0.
///
/// No saturation, because a query term's frequency is not a statement about the
/// corpus. Two identical words in a question must not out-rank a document that
/// is genuinely about the topic — that was the bug that let a dense rank-1 hit
/// own position 1 on a noisy arm.
#[must_use]
pub fn encode_query(text: &str) -> SparseVector {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pairs: Vec<(u32, f32)> = Vec::new();
    for term in tokenize(text) {
        if seen.insert(term.clone()) {
            pairs.push((term_id(&term), 1.0));
        }
    }
    SparseVector::from_pairs(pairs)
}

/// Fold one character to ASCII, or `None` when it has no ASCII form.
///
/// Covers Latin-1 Supplement and the Latin Extended-A letters Indonesian and
/// English documents actually contain.
#[must_use]
fn fold_to_ascii(c: char) -> Option<char> {
    match c {
        'à'..='å' | 'ā' | 'ă' | 'ą' | 'ǎ' => Some('a'),
        'æ' => Some('a'),
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => Some('c'),
        'ď' | 'đ' => Some('d'),
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => Some('e'),
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => Some('g'),
        'ĥ' | 'ħ' => Some('h'),
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => Some('i'),
        'ĵ' => Some('j'),
        'ķ' => Some('k'),
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => Some('l'),
        'ñ' | 'ń' | 'ņ' | 'ň' => Some('n'),
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ơ' | 'ǒ' => Some('o'),
        'œ' => Some('o'),
        'ŕ' | 'ŗ' | 'ř' => Some('r'),
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => Some('s'),
        'ß' => Some('s'),
        'ţ' | 'ť' | 'ŧ' => Some('t'),
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ư' | 'ǔ' => Some('u'),
        'ŵ' => Some('w'),
        'ý' | 'ÿ' | 'ŷ' => Some('y'),
        'ź' | 'ż' | 'ž' => Some('z'),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn term_id_is_stable() {
        // Pinning these three matters more than the algorithm: a sparse vector
        // written under an old id stops matching silently.
        assert_eq!(term_id("candy"), 4_057_490_526);
        assert_eq!(term_id("menulis"), 2_990_282_374);
        assert_eq!(term_id("ibukota"), 2_088_823_824);
    }

    #[test]
    fn term_id_of_the_empty_term_is_the_fnv_offset_basis() {
        assert_eq!(term_id(""), 0x811c_9dc5);
    }

    #[test]
    fn tokenize_drops_punctuation_and_lowercases() {
        assert_eq!(
            tokenize("Siapakah yang menulis Candy Candy?"),
            vec!["siapakah", "yang", "menulis", "candy", "candy"]
        );
    }

    #[test]
    fn tokenize_keeps_a_question_mark_out_of_the_vocabulary() {
        // SurrealDB's `TOKENIZERS class` produced a `?` term, which is how a
        // whole question returned zero hits under `@@`.
        assert!(!tokenize("apakah nama ibukota kuba?").contains(&"?".to_string()));
    }

    #[test]
    fn tokenize_folds_diacritics() {
        assert_eq!(
            tokenize("M Königsberg Ünïcodé"),
            vec!["m", "konigsberg", "unicode"]
        );
    }

    #[test]
    fn tokenize_keeps_non_latin_scripts() {
        assert_eq!(tokenize("東京 Tokyo"), vec!["東京", "tokyo"]);
    }

    #[test]
    fn no_stemmer_runs() {
        // The exact tokens the Snowball analyzer destroyed. If a stemmer is ever
        // added to this tokenizer, this test is the thing it has to argue with.
        assert_eq!(
            tokenize("Candy nasional menulis"),
            vec!["candy", "nasional", "menulis"]
        );
    }

    #[test]
    fn saturation_matches_the_hand_computation() {
        // tf=1, dl=1, avgdl=256:
        //   num = 1 * 2.2 = 2.2
        //   den = 1 + 1.2 * (1 - 0.75 + 0.75 * 1/256)
        //       = 1 + 1.2 * (0.25 + 0.0029296875)
        //       = 1 + 1.2 * 0.2529296875 = 1 + 0.303515625 = 1.303515625
        //   2.2 / 1.303515625 = 1.6877434…
        let v = saturate_tf(1.0, 1, BM25_AVGDL);
        assert!((v - 1.687_743_5).abs() < 1e-5, "{v}");

        // tf=3, dl=3, avgdl=256:
        //   num = 3 * 2.2 = 6.6
        //   den = 3 + 1.2 * (0.25 + 0.75 * 3/256)
        //       = 3 + 1.2 * (0.25 + 0.0087890625)
        //       = 3 + 1.2 * 0.2587890625 = 3 + 0.310546875 = 3.310546875
        //   6.6 / 3.310546875 = 1.9936283…
        let v = saturate_tf(3.0, 3, BM25_AVGDL);
        assert!((v - 1.993_628_3).abs() < 1e-5, "{v}");

        // A document exactly at the average length needs no length correction,
        // so saturation is the classic 1 + (k1+1)*tf/(k1*(1+tf)) - 1 form:
        // tf=2 → 2*2.2 / (2 + 1.2) = 4.4/3.2 = 1.375
        let v = saturate_tf(2.0, 256, BM25_AVGDL);
        assert!((v - 1.375).abs() < 1e-6, "{v}");
    }

    #[test]
    fn repeated_terms_saturate_rather_than_add() {
        // 20 repetitions must not score 20x a single one.
        let once = encode_document("kucing");
        let many = encode_document(&"kucing ".repeat(20));
        let a = once.values[0];
        let b = many.values[0];
        assert!(b > a);
        assert!(b < 2.0 * a, "{a} vs {b}");
    }

    #[test]
    fn document_indices_are_sorted_and_unique() {
        let v = encode_document("kucing duduk di atas tikar, kucing");
        assert_eq!(v.len(), 5);
        assert!(v.indices.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn document_encodes_a_hardcoded_vector() {
        // "a a b" → 3 tokens, tf(a)=2, tf(b)=1.
        //   tf(a) = 2*2.2 / (2 + 1.2*(0.25 + 0.75*3/256))
        //         = 4.4 / (2 + 1.2*0.2587890625) = 4.4 / 2.310546875 = 1.9043111…
        //   tf(b) = 1*2.2 / (1 + 1.2*0.2587890625) = 2.2 / 1.310546875 = 1.6786885…
        let v = encode_document("a a b");
        let a = (term_id("a"), 1.904_311_1);
        let b = (term_id("b"), 1.678_688_5);
        assert_eq!(v.len(), 2);
        assert_eq!(v.indices[0], a.0.min(b.0));
        assert_eq!(v.indices[1], a.0.max(b.0));
        let by_index = |i: u32| {
            v.indices
                .iter()
                .position(|x| *x == i)
                .map(|p| v.values[p])
                .unwrap()
        };
        assert!((by_index(a.0) - a.1).abs() < 1e-5, "{}", by_index(a.0));
        assert!((by_index(b.0) - b.1).abs() < 1e-5, "{}", by_index(b.0));
    }

    #[test]
    fn query_terms_are_unique_and_weight_one() {
        let v = encode_query("Siapakah yang menulis Candy Candy?");
        assert_eq!(v.len(), 4);
        assert!(v.values.iter().all(|x| *x == 1.0));
        assert!(v.indices.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn query_terms_hash_to_the_same_ids_as_document_terms() {
        // The whole point of one tokenizer: a query term must find its document.
        let q = encode_query("Candy Candy adalah manga");
        let d = encode_document("candy candy adalah manga karya kyoko");
        for (i, value) in q.indices.iter().zip(&q.values) {
            let p = d.indices.iter().position(|x| x == i).unwrap();
            assert!(d.values[p] > 0.0);
            assert!(*value == 1.0);
        }
    }

    #[test]
    fn empty_text_encodes_to_an_empty_vector() {
        assert!(encode_document("").is_empty());
        assert!(encode_document("   \n\t !? ").is_empty());
        assert!(encode_query("?!").is_empty());
    }

    #[test]
    fn collisions_merge_by_summing() {
        // Two distinct terms that hash alike must produce one index whose value
        // is the sum, not two entries and not a silent drop.
        let pairs = vec![(7u32, 0.5f32), (3u32, 1.5), (7u32, 0.25)];
        let v = SparseVector::from_pairs(pairs);
        assert_eq!(v.indices, vec![3, 7]);
        assert!((v.values[0] - 1.5).abs() < f32::EPSILON);
        assert!((v.values[1] - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn a_zero_or_non_finite_value_is_dropped() {
        let v = SparseVector::from_pairs(vec![(1, 0.0), (2, f32::NAN), (3, 0.25)]);
        assert_eq!(v.indices, vec![3]);
    }

    #[test]
    fn avgdl_of_zero_falls_back_to_the_default_instead_of_dividing_by_zero() {
        let v = saturate_tf(1.0, 8, 0.0);
        assert!(v.is_finite() && v > 0.0, "{v}");
    }
}
