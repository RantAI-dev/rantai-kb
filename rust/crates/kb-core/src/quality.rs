//! Extraction quality: is this text layer real text, or garbage?
//!
//! A document whose text layer exists but is unusable was indexed as `ready`
//! with almost no chunks, and nothing said so (`#29`). The gate here is the
//! pure part of the answer: what a page's text looks like, and what a
//! document's pages add up to. It never reads a file and never calls a model,
//! so both the router (which decides whether a page goes to OCR) and the
//! ingest worker (which decides whether a document may be marked `ready`)
//! can ask the same question and get the same answer.
//!
//! Two levels, because neither alone is enough:
//!
//! * **per page** — the text layer of one page is broken, so the page is an
//!   OCR candidate even when it has plenty of visible characters (a page of
//!   shifted letters is not "a text page" just because it has text);
//! * **per document** — the pages add up to almost nothing, so the document
//!   is not `ready`: it is `needs_ocr`.
//!
//! The per-page checks are deliberately conservative. A false positive costs a
//! paid OCR pass over a good page, so every rule has to be one that real
//! prose — in any script — passes. The per-document check is the one that
//! catches the reported failures, and it is script-agnostic: a book of
//! hundreds of pages that produced a handful of chunks is broken whatever its
//! text looks like.

/// The number of non-whitespace characters in `text`.
///
/// The same measure every rule in this module uses, exposed so the caller that
/// reports "how much text did this document produce" counts the same way the
/// gate counted.
#[must_use]
pub fn count_chars(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

/// What is wrong with a page's text layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDefect {
    /// The page carries fewer characters than a real page has.
    TooFewChars,
    /// Most of the characters are not letters.
    NotMostlyLetters,
    /// Fewer than half the tokens look like words.
    NotWordLike,
    /// The text holds characters a broken encoding produces: a private-use
    /// codepoint, a replacement character, or a control character.
    SuspiciousCharacters,
    /// More punctuation than whitespace, in Latin text: the shape a text
    /// layer whose character codes were shifted has (its spaces came out as
    /// punctuation).
    PunctuationOverWhitespace,
    /// Far fewer characters than the document's median page.
    FarBelowMedian,
}

impl PageDefect {
    /// A short, stable label for logs and metadata.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::TooFewChars => "too_few_chars",
            Self::NotMostlyLetters => "not_mostly_letters",
            Self::NotWordLike => "not_word_like",
            Self::SuspiciousCharacters => "suspicious_characters",
            Self::PunctuationOverWhitespace => "punctuation_over_whitespace",
            Self::FarBelowMedian => "far_below_median",
        }
    }

    /// What the defect means for a reader.
    #[must_use]
    pub fn explanation(self) -> &'static str {
        match self {
            Self::TooFewChars => "the page has almost no text",
            Self::NotMostlyLetters => "the text is mostly symbols, not letters",
            Self::NotWordLike => "the text is not made of words",
            Self::SuspiciousCharacters => "the text holds characters a broken font map produces",
            Self::PunctuationOverWhitespace => "the text layer looks shifted or re-encoded",
            Self::FarBelowMedian => "the page has far less text than the other pages",
        }
    }
}

/// What one page's text layer looks like.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageVerdict {
    /// The page's non-whitespace characters.
    pub chars: usize,
    /// What is wrong with it. Empty when the page passed.
    pub defects: Vec<PageDefect>,
}

impl PageVerdict {
    /// Whether the page's text layer is unusable and the page needs OCR.
    #[must_use]
    pub fn is_broken(&self) -> bool {
        !self.defects.is_empty()
    }

    /// The defect labels, for a metadata patch.
    #[must_use]
    pub fn labels(&self) -> Vec<&'static str> {
        self.defects.iter().map(|d| d.label()).collect()
    }
}

/// The thresholds the gate uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityOptions {
    /// A page with fewer non-whitespace characters than this is treated as
    /// empty. Low on purpose: a page with a handful of characters is not a
    /// text page, whatever those characters are.
    pub min_page_chars: usize,
    /// Below this share of letters the text is not prose.
    pub min_alpha_ratio: f32,
    /// Below this share of word-like tokens the text is not prose.
    pub min_word_ratio: f32,
    /// A page with fewer than `median * this` characters is far below the
    /// document's median page.
    pub median_ratio: f32,
    /// The document needs OCR when it produced fewer than this many chunks
    /// per page.
    pub min_chunks_per_page: f32,
}

impl Default for QualityOptions {
    fn default() -> Self {
        Self {
            min_page_chars: 24,
            min_alpha_ratio: 0.5,
            min_word_ratio: 0.5,
            median_ratio: 0.25,
            min_chunks_per_page: 0.25,
        }
    }
}

/// The counters one page's text produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    chars: usize,
    letters: usize,
    ascii_letters: usize,
    spaces: usize,
    punctuation: usize,
    tokens: usize,
    word_like: usize,
    suspicious: usize,
}

impl Counts {
    fn of(text: &str) -> Self {
        let mut counts = Counts::default();
        let mut token_letters = 0usize;
        let mut in_token = false;
        for ch in text.chars() {
            if ch.is_whitespace() {
                counts.spaces += 1;
                if in_token {
                    counts.tokens += 1;
                    if token_letters >= 2 {
                        counts.word_like += 1;
                    }
                    token_letters = 0;
                    in_token = false;
                }
                continue;
            }
            counts.chars += 1;
            if ch.is_alphabetic() {
                counts.letters += 1;
                token_letters += 1;
                if ch.is_ascii_alphabetic() {
                    counts.ascii_letters += 1;
                }
            } else if ch.is_ascii_punctuation() {
                counts.punctuation += 1;
            }
            if is_suspicious(ch) {
                counts.suspicious += 1;
            }
            in_token = true;
        }
        if in_token {
            counts.tokens += 1;
            if token_letters >= 2 {
                counts.word_like += 1;
            }
        }
        counts
    }
}

/// A character a broken encoding produces.
///
/// A private-use codepoint is what a font with no usable `ToUnicode` map
/// yields; the replacement character is what a decoder gives up on; a control
/// character is never text.
fn is_suspicious(ch: char) -> bool {
    let code = ch as u32;
    let private_use = (0xE000..=0xF8FF).contains(&code)
        || (0xF0000..=0xFFFFD).contains(&code)
        || (0x100000..=0x10FFFD).contains(&code);
    private_use || ch == '\u{FFFD}' || (ch.is_control() && !ch.is_whitespace())
}

/// Check one page's text layer on its own.
///
/// The median is not known here: a page that is short compared with the rest
/// of the document is caught by [`check_page_against_median`].
#[must_use]
pub fn check_page(text: &str, opts: &QualityOptions) -> PageVerdict {
    let counts = Counts::of(text);
    let mut defects = Vec::new();

    if counts.chars < opts.min_page_chars {
        defects.push(PageDefect::TooFewChars);
    }

    let alpha_ratio = if counts.chars == 0 {
        0.0
    } else {
        counts.letters as f32 / counts.chars as f32
    };
    if counts.chars >= opts.min_page_chars && alpha_ratio < opts.min_alpha_ratio {
        defects.push(PageDefect::NotMostlyLetters);
    }

    let word_ratio = if counts.tokens == 0 {
        0.0
    } else {
        counts.word_like as f32 / counts.tokens as f32
    };
    if counts.chars >= opts.min_page_chars && word_ratio < opts.min_word_ratio {
        defects.push(PageDefect::NotWordLike);
    }

    if counts.suspicious > 0 {
        defects.push(PageDefect::SuspiciousCharacters);
    }

    // The shifted-encoding shape. Only for Latin-ish text: a script that does
    // not use spaces (Chinese, Japanese) has more punctuation than spaces in
    // perfectly good prose, and flagging it would send every page to OCR.
    let latin_ratio = if counts.letters == 0 {
        0.0
    } else {
        counts.ascii_letters as f32 / counts.letters as f32
    };
    if counts.chars >= opts.min_page_chars
        && latin_ratio >= 0.8
        && counts.punctuation > counts.spaces
    {
        defects.push(PageDefect::PunctuationOverWhitespace);
    }

    PageVerdict {
        chars: counts.chars,
        defects,
    }
}

/// Check one page's text layer against the document's median page.
///
/// A page with a fraction of the median's characters is broken even when what
/// it holds looks like text: the document's other pages say what a page of it
/// should hold.
#[must_use]
pub fn check_page_against_median(
    text: &str,
    median_chars: usize,
    opts: &QualityOptions,
) -> PageVerdict {
    let mut verdict = check_page(text, opts);
    let floor = (median_chars as f32 * opts.median_ratio) as usize;
    if median_chars >= opts.min_page_chars
        && verdict.chars > 0
        && verdict.chars < floor
        && !verdict.defects.contains(&PageDefect::TooFewChars)
    {
        verdict.defects.push(PageDefect::FarBelowMedian);
    }
    verdict
}

/// The median number of non-whitespace characters per page.
///
/// The median, not the mean: one very long page (a table, a bibliography)
/// would drag a mean far above what a normal page holds, and every other page
/// would then look short.
#[must_use]
pub fn median_page_chars(pages: &[usize]) -> usize {
    if pages.is_empty() {
        return 0;
    }
    let mut sorted: Vec<usize> = pages.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

/// What a document's pages add up to.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentVerdict {
    /// The pages that were checked.
    pub pages: usize,
    /// The characters the pages carry together.
    pub chars: usize,
    /// How many of the pages failed the page check.
    pub broken_pages: usize,
    /// The chunks the document produced.
    pub chunks: usize,
    /// The chunks per page.
    pub chunks_per_page: f32,
    /// The document is not usable as it stands: it needs OCR.
    pub needs_ocr: bool,
    /// Why, in words an operator can read.
    pub reasons: Vec<String>,
}

impl DocumentVerdict {
    /// The metadata patch the ingest worker stores on the document.
    #[must_use]
    pub fn metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "pages": self.pages,
            "charsExtracted": self.chars,
            "brokenPages": self.broken_pages,
            "chunks": self.chunks,
            "chunksPerPage": self.chunks_per_page,
            "needsOcr": self.needs_ocr,
            "reasons": self.reasons,
        })
    }
}

/// Judge a document by its pages and the chunks it produced.
///
/// `page_verdicts` holds the pages that were checked — a page with no text at
/// all is not in it, because there is nothing to check — and `pages` is how
/// many pages the document has. The two differ whenever a page carries no
/// text, and the ratio that matters is the one over *all* the pages: a book of
/// three hundred pages that produced eight chunks is broken, whether the
/// missing pages were empty or unreadable.
///
/// `chars` is how much text the document holds in total. A document with no
/// pages (a format that has none) is never judged: the check has nothing to
/// say about it.
#[must_use]
pub fn assess_document(
    page_verdicts: &[PageVerdict],
    pages: usize,
    chars: usize,
    chunks: usize,
    opts: &QualityOptions,
) -> DocumentVerdict {
    let broken_pages = page_verdicts.iter().filter(|v| v.is_broken()).count();
    let chunks_per_page = if pages == 0 {
        0.0
    } else {
        chunks as f32 / pages as f32
    };
    let mut reasons = Vec::new();

    if pages > 0 && chunks_per_page < opts.min_chunks_per_page {
        reasons.push(format!(
            "{} chunks over {} pages is below {chunks_per_page:.2} chunks per page",
            chunks, pages,
        ));
    }
    if pages > 0 && broken_pages * 2 > pages {
        reasons.push(format!(
            "{broken_pages} of {pages} pages failed the text-layer check"
        ));
    }

    DocumentVerdict {
        pages,
        chars,
        broken_pages,
        chunks,
        chunks_per_page,
        needs_ocr: !reasons.is_empty(),
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTS: QualityOptions = QualityOptions {
        min_page_chars: 24,
        min_alpha_ratio: 0.5,
        min_word_ratio: 0.5,
        median_ratio: 0.25,
        min_chunks_per_page: 0.25,
    };

    fn prose() -> String {
        "Bab ini membahas dasar-dasar geometri yang dipakai di sekolah. \
         Setiap bab berisi contoh soal dan latihan mandiri untuk siswa. "
            .repeat(4)
    }

    #[test]
    fn ordinary_prose_passes() {
        let verdict = check_page(&prose(), &OPTS);
        assert!(verdict.defects.is_empty(), "{:?}", verdict.defects);
        assert!(!verdict.is_broken());
    }

    #[test]
    fn prose_in_another_script_passes() {
        // Chinese prose has no spaces and more commas than spaces: the
        // shifted-encoding rule must not fire on it.
        let text =
            "这是一本关于几何的教科书，每章都有例题，还有很多练习题，供学生们自己完成。".repeat(3);
        let verdict = check_page(&text, &OPTS);
        assert!(verdict.defects.is_empty(), "{:?}", verdict.defects);
    }

    #[test]
    fn a_page_with_almost_no_text_is_broken() {
        let verdict = check_page("x", &OPTS);
        assert!(verdict.is_broken());
        assert!(verdict.defects.contains(&PageDefect::TooFewChars));
    }

    #[test]
    fn a_page_of_digits_and_symbols_is_broken() {
        let text = "1 2 3 4 5 6 7 8 9 0 $ % & * 1 2 3 4 5 6 7 8 9 0".repeat(2);
        let verdict = check_page(&text, &OPTS);
        assert!(verdict.defects.contains(&PageDefect::NotMostlyLetters));
    }

    #[test]
    fn a_page_of_punctuation_is_broken() {
        let text = "!@#$%^&*()_+-=[]{};':\",./<>? 12345 67890 !@#$%^&*()".repeat(2);
        let verdict = check_page(&text, &OPTS);
        assert!(verdict.defects.contains(&PageDefect::NotWordLike));
    }

    #[test]
    fn a_page_with_private_use_characters_is_broken() {
        // What a font with no usable ToUnicode map produces.
        let text = format!("{}\u{E000}\u{E001} broken glyphs everywhere", "abc ");
        let verdict = check_page(&text, &OPTS);
        assert!(verdict.defects.contains(&PageDefect::SuspiciousCharacters));
    }

    #[test]
    fn a_shifted_text_layer_is_broken() {
        // Every character code moved by one: the spaces came out as
        // punctuation. This is the shape a broken font map has.
        let text: String = prose()
            .chars()
            .map(|c| {
                let code = c as u32 + 1;
                char::from_u32(code).unwrap_or(c)
            })
            .collect();
        let verdict = check_page(&text, &OPTS);
        assert!(
            verdict
                .defects
                .contains(&PageDefect::PunctuationOverWhitespace),
            "{:?}",
            verdict.defects
        );
    }

    #[test]
    fn a_page_far_below_the_median_is_broken() {
        // Long enough to pass the page check on its own, short enough to be
        // far below the rest of the document.
        let short = "Hanya satu kalimat pendek saja di halaman ini.";
        let verdict = check_page_against_median(short, 2000, &OPTS);
        assert!(verdict.defects.contains(&PageDefect::FarBelowMedian));
    }

    #[test]
    fn the_median_ignores_one_huge_page() {
        let pages = vec![100, 110, 120, 5000];
        assert_eq!(median_page_chars(&pages), 120);
        assert_eq!(median_page_chars(&[]), 0);
    }

    #[test]
    fn a_document_with_almost_no_chunks_needs_ocr() {
        let verdicts: Vec<PageVerdict> = (0..200)
            .map(|_| PageVerdict {
                chars: 10,
                defects: vec![PageDefect::TooFewChars],
            })
            .collect();
        let verdict = assess_document(&verdicts, 200, 2000, 8, &OPTS);
        assert!(verdict.needs_ocr);
        assert_eq!(verdict.broken_pages, 200);
        assert!(verdict.chunks_per_page < 0.25);
        assert_eq!(verdict.reasons.len(), 2, "{:?}", verdict.reasons);
    }

    #[test]
    fn a_healthy_document_does_not_need_ocr() {
        let verdicts: Vec<PageVerdict> = (0..200)
            .map(|_| PageVerdict {
                chars: 2000,
                defects: Vec::new(),
            })
            .collect();
        let verdict = assess_document(&verdicts, 200, 400_000, 800, &OPTS);
        assert!(!verdict.needs_ocr);
        assert!(verdict.reasons.is_empty());
    }

    #[test]
    fn a_document_without_pages_is_never_judged() {
        let verdict = assess_document(&[], 0, 0, 0, &OPTS);
        assert!(!verdict.needs_ocr);
    }

    #[test]
    fn pages_that_carry_no_text_still_count_towards_the_ratio() {
        // Three hundred pages, eight of them with text: the book produced
        // eight chunks, and the ratio is over every page.
        let verdicts: Vec<PageVerdict> = (0..8)
            .map(|_| PageVerdict {
                chars: 2000,
                defects: Vec::new(),
            })
            .collect();
        let verdict = assess_document(&verdicts, 300, 16_000, 8, &OPTS);
        assert!(verdict.needs_ocr, "{:?}", verdict.reasons);
        assert_eq!(verdict.pages, 300);
        assert_eq!(verdict.broken_pages, 0);
    }

    #[test]
    fn the_metadata_patch_names_the_numbers() {
        let verdicts = vec![PageVerdict {
            chars: 10,
            defects: vec![PageDefect::TooFewChars],
        }];
        let verdict = assess_document(&verdicts, 1, 10, 0, &OPTS);
        let meta = verdict.metadata();
        assert_eq!(meta["pages"], 1);
        assert_eq!(meta["charsExtracted"], 10);
        assert_eq!(meta["brokenPages"], 1);
        assert_eq!(meta["needsOcr"], true);
    }

    #[test]
    fn chars_are_counted_without_whitespace() {
        assert_eq!(count_chars("a b\nc\t"), 3);
        assert_eq!(count_chars(""), 0);
        assert_eq!(count_chars("   "), 0);
        // The gate's own measure: the same number `Counts::of` counts.
        let text = "Bab ini membahas dasar-dasar geometri.";
        assert_eq!(count_chars(text), 36);
    }
}
