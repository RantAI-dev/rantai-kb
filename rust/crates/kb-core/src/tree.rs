//! The tree arm: score a document's section hierarchy for a structural question.
//!
//! A question that says "what does section 4.2 say about leave" names a place
//! in a document, not a phrase. A dense vector averages "4.2" away; a BM25
//! corpus ranks whichever chunk happens to repeat the word "leave". The tree
//! index is the missing structure: each document's outline (bookmarks or
//! numbered headings) is a list of nodes, each with a title and a page span, and
//! this module scores those nodes directly.
//!
//! This is a port of EVAL-2's prototype
//! (`rust/crates/kb-eval/scripts/tree_arm.py`), and the scoring is deliberately
//! the same, because that prototype is what the +0.086 nDCG@10 on structural
//! questions was measured with:
//!
//! * **Explicit numbers.** "section 4.2", "§ 4.2", "chapter 5" or a bare "4.2"
//!   are matched against a heading's own number. A chapter number is also a
//!   prefix of every "N.x" section, so those get a smaller partial score rather
//!   than nothing.
//! * **Title words.** BM25 (k1 1.2, b 0.75) over node titles, with the document
//!   title folded into each node's text, so "the chapter on labour market
//!   developments" need not know the number.
//! * **Which document.** Query tokens vote for the document whose outline
//!   vocabulary matches best; when one document wins clearly, nodes are
//!   restricted to it. Otherwise every document competes, which is the honest
//!   behaviour for a question that never named a book.
//!
//! Pure: it reads strings and returns scores. Fetching the outlines, reading
//! chunks on the chosen pages and fusing them into the candidate list are
//! `kb-engine`'s and the stores' jobs.

use std::collections::HashMap;
use std::sync::OnceLock;

use fancy_regex::Regex;
use serde::{Deserialize, Serialize};

/// Heading nodes the arm keeps per query (`KB_TREE_ARM_NODES`).
pub const DEFAULT_NODES: usize = 5;
/// Pages taken from each chosen node, heading page first. The EVAL-2 structural
/// set's gold is five pages per section, so a 30-page chapter cannot flood a
/// ten-result answer with one heading.
pub const DEFAULT_PAGES_PER_NODE: u32 = 5;
/// Hard cap on the distinct pages one query asks the index for.
pub const DEFAULT_MAX_PAGES: usize = 50;
/// Weight of the tree arm's ranking when it is RRF-fused with the text arms.
/// Below 1.0 because an outline is a structural hint, not the retriever.
pub const DEFAULT_WEIGHT: f32 = 0.5;
/// RRF constant, the same 60 the rest of the pipeline uses.
pub const DEFAULT_RRF_K: u32 = 60;

/// How a document's outline was produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineMethod {
    /// The PDF's own bookmark tree.
    Bookmarks,
    /// Numbered headings recovered from page text.
    Headings,
    /// Neither produced anything. Reported, not silently omitted.
    #[default]
    None,
}

impl OutlineMethod {
    /// The wire spelling stored in `Document.metadata.outline.method`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bookmarks => "bookmarks",
            Self::Headings => "headings",
            Self::None => "none",
        }
    }
}

/// One section of a document.
///
/// Page numbers are **zero-based**, the same numbering as `PageText.page` and
/// `ChunkMetadata.page`/`Candidate.page` everywhere in the service. The
/// prototype's `outline.py` printed 1-based PDF pages; the conversion happens
/// once, at extraction, so no later stage has to remember which convention it
/// is holding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineNode {
    /// The heading as printed, e.g. "4.2 Leave entitlement".
    pub title: String,
    /// 1 for a chapter/top-level heading, 2 for "4.2", 3 for "4.2.1".
    pub level: u32,
    /// Zero-based page the section starts on.
    pub start_page: u32,
    /// Zero-based page the section ends on: the page before the next node at
    /// the same or a higher level starts, clamped to the document's last page.
    pub end_page: u32,
}

/// A document's section hierarchy, stored additively in
/// `Document.metadata.outline` (JSONB).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outline {
    /// The document's title at extraction time, folded into every node's bag of
    /// words so "the chapter on labour market developments" can also pick the
    /// right *document*. `None` for an outline written before this field
    /// existed; the scorer then uses the document key alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,
    pub method: OutlineMethod,
    pub nodes: Vec<OutlineNode>,
}

impl Outline {
    /// An outline with no nodes. The method still says why.
    #[must_use]
    pub fn empty(method: OutlineMethod) -> Self {
        Self {
            document_title: None,
            method,
            nodes: Vec::new(),
        }
    }

    /// Whether there is nothing to navigate.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// How many sections the outline has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
}

/// One document's outline, as the scorer sees it.
#[derive(Debug, Clone, Copy)]
pub struct TreeDocument<'a> {
    /// A stable key for deterministic tie-breaking. The engine passes the
    /// document id.
    pub key: &'a str,
    /// The document title, folded into every node's bag of words.
    pub title: &'a str,
    pub nodes: &'a [OutlineNode],
}

/// A scored node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TreeMatch {
    /// Index into the `documents` slice passed to [`score_tree`].
    pub document: usize,
    /// Index into that document's `nodes`.
    pub node: usize,
    pub score: f32,
}

/// The stopwords the prototype removes before scoring.
const STOPWORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "also",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "down",
    "during",
    "each",
    "few",
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
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "me",
    "more",
    "most",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "same",
    "she",
    "should",
    "so",
    "some",
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
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

/// A heading with a dotted number: "4.2", "2.1.1", "3.4.1.".
fn dotted() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(?:section\s+|sec\.?\s+)?(\d+(?:\.\d+)+)\.?\s+(.+)$").ok())
        .as_ref()
}

/// A top-level numbered heading: "3. Labour market developments".
fn top_numbered() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d+)\.?\s+([A-Z][^.]{2,})$").ok())
        .as_ref()
}

/// "Chapter 3", "CHAPTER 3: Title", "Part II".
fn chapter() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(chapter|part)\s+([0-9]+|[ivxlcdm]+)\b[.:]?\s*(.*)$").ok())
        .as_ref()
}

/// "Annex", "Appendix".
fn annex() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(annex|appendix)\b").ok())
        .as_ref()
}

/// Words that open a structural reference: "chapter 5", "section 3.4", "part 2".
fn chapter_query() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(?:chapter|chap\.?|part)\s+(\d+(?:\.\d+)*)\b").ok())
        .as_ref()
}

/// `section 3.4` / `§ 3.4`.
fn section_query() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:\bsection|\bsec\.?|§)\s*(\d+(?:\.\d+)*)\b").ok())
        .as_ref()
}

/// A bare dotted number: "4.2", "2.1.1". The lookaround keeps it from matching
/// inside a longer number or a version string.
fn bare_number() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?<![\w.])(\d+\.\d+(?:\.\d+)*)(?![\w.])").ok())
        .as_ref()
}

fn captures_first(re: Option<&Regex>, text: &str) -> Option<String> {
    let re = re?;
    let caps = re.captures(text).ok().flatten()?;
    caps.get(1).map(|m| m.as_str().to_string())
}

fn matches(re: Option<&Regex>, text: &str) -> bool {
    re.is_some_and(|re| re.is_match(text).unwrap_or(false))
}

fn captures_all(re: Option<&Regex>, text: &str, group: usize) -> Vec<String> {
    let Some(re) = re else {
        return Vec::new();
    };
    re.captures_iter(text)
        .flatten()
        .filter_map(|caps| caps.get(group).map(|m| m.as_str().to_string()))
        .collect()
}

/// Tokenise for scoring: lower-cased alphanumeric runs, stopwords removed.
#[must_use]
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            push_token(&mut out, &current);
            current.clear();
        }
    }
    if !current.is_empty() {
        push_token(&mut out, &current);
    }
    out
}

fn push_token(out: &mut Vec<String>, token: &str) {
    if !STOPWORDS.contains(&token) {
        out.push(token.to_string());
    }
}

/// The number a heading carries in its own title.
///
/// "4.2 Leave" → "4.2"; "Chapter 5" → "5"; anything else → `None`.
#[must_use]
pub fn node_number(title: &str) -> Option<String> {
    if let Some(caps) = captures_first(dotted(), title) {
        // The dotted heading's number is group 1, with a trailing dot removed.
        return Some(caps.trim_end_matches('.').to_string());
    }
    if let Some(caps) = captures_all(chapter(), title, 2).into_iter().next() {
        return Some(caps);
    }
    None
}

/// A title whose own word is "Chapter".
#[must_use]
pub fn is_chapter_node(title: &str) -> bool {
    matches(chapter(), title)
}

/// The level a title's own numbering implies, or `None`.
///
/// "Chapter 3" and "3." are top level; "4.2" is level 2; "2.1.1" is level 3.
#[must_use]
pub fn numbering_level(title: &str) -> Option<u32> {
    if matches(chapter(), title) || matches(annex(), title) {
        return Some(1);
    }
    if let Some(number) = captures_first(dotted(), title) {
        return u32::try_from(number.trim_end_matches('.').split('.').count()).ok();
    }
    if matches(top_numbered(), title) {
        return Some(1);
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumberKind {
    Chapter,
    Section,
}

/// Every explicit section/chapter number in the query, in query order.
#[must_use]
pub fn query_numbers(text: &str) -> Vec<String> {
    number_kinds(text)
        .into_iter()
        .map(|(number, _)| number)
        .collect()
}

fn number_kinds(text: &str) -> Vec<(String, NumberKind)> {
    let mut found: Vec<(String, NumberKind)> = captures_all(chapter_query(), text, 1)
        .into_iter()
        .map(|number| {
            (
                number.trim_end_matches('.').to_string(),
                NumberKind::Chapter,
            )
        })
        .collect();
    found.extend(
        captures_all(section_query(), text, 1)
            .into_iter()
            .map(|number| {
                (
                    number.trim_end_matches('.').to_string(),
                    NumberKind::Section,
                )
            }),
    );
    if found.is_empty() {
        found.extend(
            captures_all(bare_number(), text, 1)
                .into_iter()
                .map(|number| {
                    (
                        number.trim_end_matches('.').to_string(),
                        NumberKind::Section,
                    )
                }),
        );
    }
    found
}

fn number_bonus(number: &str, kind: NumberKind, node: Option<&str>) -> f32 {
    let Some(node_number) = node else {
        return 0.0;
    };
    if node_number == number {
        return if kind == NumberKind::Chapter {
            12.0
        } else {
            8.0
        };
    }
    // "chapter 5" is a prefix of sections 5.1, 5.2 …; the chapter node still wins.
    if node_number.starts_with(&format!("{number}.")) {
        return if kind == NumberKind::Chapter {
            2.0
        } else {
            1.0
        };
    }
    if number.starts_with(&format!("{node_number}.")) {
        return 1.5;
    }
    0.0
}

/// A crude TF-IDF-like document vote: rare query words count more.
fn document_scores<'a>(
    query: &[String],
    documents: &[TreeDocument<'a>],
    vocabulary: &[HashMap<String, f32>],
) -> Vec<f32> {
    let mut document_frequency: HashMap<&str, f32> = HashMap::new();
    for words in vocabulary {
        for word in words.keys() {
            *document_frequency.entry(word.as_str()).or_insert(0.0) += 1.0;
        }
    }
    let total = documents.len().max(1) as f32;
    let mut counts: HashMap<&str, f32> = HashMap::new();
    for token in query {
        *counts.entry(token.as_str()).or_insert(0.0) += 1.0;
    }
    let length = (query.len().max(1)) as f32;
    documents
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let mut score = 0.0_f32;
            for (token, count) in &counts {
                if vocabulary[index].contains_key(*token) {
                    let frequency = document_frequency.get(token).copied().unwrap_or(0.0);
                    score += count * (1.0 + total / frequency.max(1.0)).ln();
                }
            }
            score / length.sqrt()
        })
        .collect()
}

/// BM25 over one document's node titles, k1 1.2 / b 0.75.
fn bm25(query: &[String], nodes: &[(Vec<String>, Option<String>)]) -> Vec<f32> {
    if nodes.is_empty() || query.is_empty() {
        return vec![0.0; nodes.len()];
    }
    let lengths: Vec<f32> = nodes.iter().map(|(t, _)| t.len() as f32).collect();
    let average = lengths.iter().sum::<f32>() / lengths.len() as f32;
    let mut document_frequency: HashMap<&str, f32> = HashMap::new();
    for (words, _) in nodes {
        let unique: std::collections::HashSet<&str> = words.iter().map(String::as_str).collect();
        for word in unique {
            *document_frequency.entry(word).or_insert(0.0) += 1.0;
        }
    }
    let total = nodes.len() as f32;
    let unique_query: std::collections::HashSet<&str> = query.iter().map(String::as_str).collect();
    nodes
        .iter()
        .enumerate()
        .map(|(index, (words, _))| {
            let mut counts: HashMap<&str, f32> = HashMap::new();
            for word in words {
                *counts.entry(word.as_str()).or_insert(0.0) += 1.0;
            }
            let mut score = 0.0_f32;
            for token in &unique_query {
                let frequency = counts.get(token).copied().unwrap_or(0.0);
                if frequency == 0.0 {
                    continue;
                }
                let df = document_frequency.get(token).copied().unwrap_or(0.0);
                let idf = (1.0 + (total - df + 0.5) / (df + 0.5)).ln();
                let k1 = 1.2_f32;
                let b = 0.75_f32;
                let denominator = frequency + k1 * (1.0 - b + b * lengths[index] / average);
                score += idf * frequency * (k1 + 1.0) / denominator;
            }
            score
        })
        .collect()
}

/// Score every outline node against `query`, best first.
///
/// Returns only positive scores, so a structural question that matches no
/// heading produces an empty list rather than a page-zero guess. Ties break on
/// the document key and the node's start page, which are corpus-stable.
#[must_use]
pub fn score_tree(query: &str, documents: &[TreeDocument<'_>]) -> Vec<TreeMatch> {
    let query_tokens = tokens(query);
    if documents.is_empty() {
        return Vec::new();
    }

    let document_words: Vec<Vec<String>> = documents
        .iter()
        .map(|document| {
            let mut words = tokens(document.title);
            words.extend(tokens(&document.key.replace(['_', '-'], " ")));
            words
        })
        .collect();
    let vocabularies: Vec<HashMap<String, f32>> = documents
        .iter()
        .enumerate()
        .map(|(index, document)| {
            let mut vocabulary: HashMap<String, f32> = HashMap::new();
            for node in document.nodes {
                for token in tokens(&node.title) {
                    *vocabulary.entry(token).or_insert(0.0) += 1.0;
                }
                for token in &document_words[index] {
                    *vocabulary.entry(token.clone()).or_insert(0.0) += 1.0;
                }
            }
            vocabulary
        })
        .collect();

    let votes = document_scores(&query_tokens, documents, &vocabularies);
    let mut ranked: Vec<(usize, f32)> = votes.into_iter().enumerate().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let best = ranked.first().copied().unwrap_or((0, 0.0));
    let second = ranked.get(1).map_or(0.0, |(_, score)| *score);
    let chosen: Vec<usize> = if best.1 >= 1.0 && best.1 > 1.5 * second.max(0.0) {
        vec![best.0]
    } else {
        (0..documents.len()).collect()
    };

    let numbers = number_kinds(query);
    let mut scored: Vec<TreeMatch> = Vec::new();
    for document_index in chosen {
        let document = documents[document_index];
        let node_texts: Vec<(Vec<String>, Option<String>)> = document
            .nodes
            .iter()
            .map(|node| {
                let mut words = tokens(&node.title);
                words.extend(document_words[document_index].iter().cloned());
                (words, node_number(&node.title))
            })
            .collect();
        let lexical = bm25(&query_tokens, &node_texts);
        for (node_index, _node) in document.nodes.iter().enumerate() {
            let bonus: f32 = numbers
                .iter()
                .map(|(number, kind)| {
                    number_bonus(number, *kind, node_texts[node_index].1.as_deref())
                })
                .sum();
            let score = lexical.get(node_index).copied().unwrap_or(0.0) + bonus;
            if score > 0.0 {
                scored.push(TreeMatch {
                    document: document_index,
                    node: node_index,
                    score,
                });
            }
        }
    }
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| documents[a.document].key.cmp(documents[b.document].key))
            .then_with(|| {
                documents[a.document].nodes[a.node]
                    .start_page
                    .cmp(&documents[b.document].nodes[b.node].start_page)
            })
    });
    scored
}

/// The pages a node contributes: its start page first, at most `per_node`
/// pages, never past its end.
#[must_use]
pub fn node_pages(node: &OutlineNode, per_node: u32) -> Vec<u32> {
    if per_node == 0 {
        return Vec::new();
    }
    let last = node
        .start_page
        .saturating_add(per_node.saturating_sub(1))
        .min(node.end_page);
    (node.start_page..=last).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn node(title: &str, level: u32, start_page: u32, end_page: u32) -> OutlineNode {
        OutlineNode {
            title: title.to_string(),
            level,
            start_page,
            end_page,
        }
    }

    fn outline() -> Vec<OutlineNode> {
        vec![
            node("Chapter 1 Introduction", 1, 0, 4),
            node("1.1 Background", 2, 1, 2),
            node("Chapter 2 Labour market developments", 1, 5, 19),
            node("2.1 Employment growth", 2, 6, 9),
            node("2.2 Wage setting", 2, 10, 14),
            node("Chapter 3 Fiscal policy", 1, 20, 29),
            node("3.1 Budget balance", 2, 21, 24),
            node("Chapter 4 Social policy", 1, 30, 44),
            node("4.2 Leave entitlement", 2, 33, 36),
            node("4.2.1 Parental leave", 3, 34, 35),
        ]
    }

    fn score(query: &str) -> Vec<TreeMatch> {
        let nodes = outline();
        score_tree(
            query,
            &[TreeDocument {
                key: "doc-a",
                title: "Joint Employment Report 2024",
                nodes: &nodes,
            }],
        )
    }

    #[test]
    fn an_explicit_section_number_finds_that_node_first() {
        let hits = score("what does section 4.2 say about leave?");
        let first = hits.first().expect("a hit");
        assert_eq!(outline()[first.node].title, "4.2 Leave entitlement");
    }

    #[test]
    fn a_chapter_number_prefers_the_chapter_node_over_its_sections() {
        let hits = score("summarise chapter 2");
        let first = hits.first().expect("a hit");
        assert_eq!(
            outline()[first.node].title,
            "Chapter 2 Labour market developments"
        );
        assert!(first.score >= 12.0, "chapter bonus: {}", first.score);
    }

    #[test]
    fn a_bare_dotted_number_is_treated_as_a_section() {
        let hits = score("4.2");
        let first = hits.first().expect("a hit");
        assert_eq!(outline()[first.node].title, "4.2 Leave entitlement");
    }

    #[test]
    fn title_words_find_the_section_without_its_number() {
        let hits = score("what does the chapter on labour market developments say");
        let first = hits.first().expect("a hit");
        assert!(
            outline()[first.node]
                .title
                .to_lowercase()
                .contains("labour market"),
            "{}",
            outline()[first.node].title
        );
    }

    #[test]
    fn a_parent_section_number_also_reaches_its_child() {
        let hits = score("section 4.2 parental leave");
        let nodes = outline();
        let titles: Vec<&str> = hits
            .iter()
            .map(|hit| nodes[hit.node].title.as_str())
            .collect();
        assert!(titles.contains(&"4.2 Leave entitlement"), "{titles:?}");
        assert!(titles.contains(&"4.2.1 Parental leave"), "{titles:?}");
    }

    #[test]
    fn no_match_returns_nothing_rather_than_a_guess() {
        assert!(score("zzzq unknown phrase entirely").is_empty());
    }

    #[test]
    fn an_empty_outline_returns_nothing() {
        let hits = score_tree(
            "chapter 2",
            &[TreeDocument {
                key: "doc-a",
                title: "Report",
                nodes: &[],
            }],
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn a_clear_document_name_restricts_the_nodes_to_that_document() {
        let first = [node("Chapter 1 Introduction", 1, 0, 3)];
        let second = [node("Chapter 7 Aquaculture", 1, 0, 3)];
        let hits = score_tree(
            "chapter 7 in the aquaculture handbook",
            &[
                TreeDocument {
                    key: "annual-report",
                    title: "Annual Report",
                    nodes: &first,
                },
                TreeDocument {
                    key: "aquaculture-handbook",
                    title: "Aquaculture Handbook",
                    nodes: &second,
                },
            ],
        );
        let top = hits.first().expect("a hit");
        assert_eq!(top.document, 1, "{hits:?}");
    }

    #[test]
    fn node_pages_cap_at_the_node_span() {
        let node = node("2.1 Growth", 2, 6, 7);
        assert_eq!(node_pages(&node, 5), vec![6, 7]);
        assert_eq!(node_pages(&node, 1), vec![6]);
        assert!(node_pages(&node, 0).is_empty());
    }

    #[test]
    fn numbering_level_trusts_the_title_number_over_the_nesting() {
        assert_eq!(numbering_level("Chapter 3"), Some(1));
        assert_eq!(numbering_level("Annex A"), Some(1));
        assert_eq!(numbering_level("3. Labour market"), Some(1));
        assert_eq!(numbering_level("4.2 Leave"), Some(2));
        assert_eq!(numbering_level("4.2.1 Parental leave"), Some(3));
        assert_eq!(numbering_level("Contents"), None);
    }

    #[test]
    fn node_number_reads_dotted_and_chapter_titles() {
        assert_eq!(node_number("4.2 Leave entitlement"), Some("4.2".into()));
        assert_eq!(node_number("Chapter 5"), Some("5".into()));
        assert_eq!(node_number("Introduction"), None);
    }

    #[test]
    fn outline_serde_uses_the_stored_wire_shape() {
        let outline = Outline {
            document_title: Some("Report".into()),
            method: OutlineMethod::Bookmarks,
            nodes: vec![node("4.2 Leave", 2, 3, 3)],
        };
        let value = serde_json::to_value(&outline).unwrap();
        assert_eq!(value["method"], "bookmarks");
        assert_eq!(value["nodes"][0]["start_page"], 3);
        let back: Outline = serde_json::from_value(value).unwrap();
        assert_eq!(back, outline);
    }
}
