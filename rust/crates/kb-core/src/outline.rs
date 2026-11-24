//! Outline v2: a table of contents for one document, built from what ingest
//! already stored. Pure — no I/O, no store access, no environment reads.
//!
//! An agent reading a regulation or an SOP needs a map *before* it reads a
//! single chunk: which sections exist, how big they are, and which chunks and
//! pages to ask for. Indonesian legal documents are structured by
//! `BAB / Bagian / Pasal / Paragraf` and often have no markdown headings at all,
//! so section detection cannot rely on `metadata.section` alone.
//!
//! Sources, in priority order:
//! 1. the stored outline (`Document.metadata.outline`, from the tree arm);
//! 2. headings in chunk metadata (`section`);
//! 3. legal markers at the start of lines in chunk content;
//! 4. one section per page when nothing else was found.
//!
//! The result is a flat, ordered list of [`Section`]s. Each section owns a
//! contiguous chunk range, so the caller can hand that range to the content
//! route (`RET-14`) without re-searching.

use serde::{Deserialize, Serialize};

use crate::tree::Outline;

/// What produced a section boundary. One of the five legal/markdown markers
/// or the per-page fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Marker {
    /// `BAB I …` — level 1.
    Bab,
    /// `Bagian Kesatu …` — level 2.
    Bagian,
    /// `Pasal 1 …` — level 4.
    Pasal,
    /// `Paragraf 1 …` — level 3.
    Paragraf,
    /// A markdown/chunk heading.
    Heading,
    /// The per-page fallback.
    Page,
}

impl Marker {
    /// Stable lowercase wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bab => "bab",
            Self::Bagian => "bagian",
            Self::Pasal => "pasal",
            Self::Paragraf => "paragraf",
            Self::Heading => "heading",
            Self::Page => "page",
        }
    }
}

/// One section of the outline. `chunk_start`/`chunk_end` are inclusive indices
/// into the ordered chunk list; `page_start`/`page_end` are inclusive and
/// `None` when no chunk in the range carries a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub id: String,
    pub title: String,
    pub level: u32,
    pub path: Vec<String>,
    pub page_start: Option<u32>,
    pub page_end: Option<u32>,
    pub chunk_start: u32,
    pub chunk_end: u32,
    pub estimated_chars: u32,
    pub marker: Marker,
}

/// One ordered chunk as the outline builder needs it. The server fills this
/// from the chunk-store port; no adapter type appears here.
#[derive(Debug, Clone)]
pub struct OutlineChunk {
    pub chunk_index: u32,
    pub page: Option<u32>,
    pub section: Option<String>,
    pub content: String,
    pub char_start: Option<u32>,
    pub char_end: Option<u32>,
    pub overlap_prefix_len: Option<u32>,
}

/// A detected section boundary, before chunk ranges are assigned. Several
/// boundaries can share one chunk (a chunk can open both a BAB and a Pasal).
#[derive(Debug, Clone)]
struct Boundary {
    chunk_index: u32,
    title: String,
    level: u32,
    path: Vec<String>,
    marker: Marker,
}

/// Strip leading whitespace and any run of markdown heading markers, so
/// `  ##   BAB I …` and `BAB I …` are treated the same.
fn strip_prefix(line: &str) -> &str {
    let mut text = line.trim_start();
    while let Some(rest) = text.strip_prefix('#') {
        text = rest.trim_start();
    }
    text
}

fn first_word<'a>(lower: &'a str, prefix: &str) -> Option<&'a str> {
    lower
        .strip_prefix(prefix)
        .and_then(|rest| rest.split_whitespace().next())
}

/// `^BAB [IVXLC]+`, case-insensitive.
fn is_bab(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    first_word(&lower, "bab ")
        .is_some_and(|word| !word.is_empty() && word.chars().all(|c| "ivxlc".contains(c)))
}

/// `^Bagian (Kesatu|Kedua|…|Ke\w+)`, case-insensitive.
fn is_bagian(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    first_word(&lower, "bagian ").is_some_and(|word| {
        const ORDINALS: [&str; 10] = [
            "kesatu",
            "kedua",
            "ketiga",
            "keempat",
            "kelima",
            "keenam",
            "ketujuh",
            "kedelapan",
            "kesembilan",
            "kesepuluh",
        ];
        if ORDINALS.contains(&word) {
            return true;
        }
        word.strip_prefix("ke").is_some_and(|tail| {
            !tail.is_empty() && tail.chars().all(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

/// `^Paragraf \d+`, case-insensitive.
fn is_paragraf(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    first_word(&lower, "paragraf ")
        .is_some_and(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_digit()))
}

/// `^Pasal \d+[A-Z]?`, case-insensitive.
fn is_pasal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let Some(word) = first_word(&lower, "pasal ") else {
        return false;
    };
    let mut chars = word.chars().peekable();
    let mut digits = 0;
    while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
        chars.next();
        digits += 1;
    }
    if digits == 0 {
        return false;
    }
    match chars.next() {
        None => true,
        Some(letter) => letter.is_ascii_alphabetic() && chars.next().is_none(),
    }
}

/// Detect an Indonesian legal marker at the start of a line. Leading
/// whitespace and markdown `#` markers are allowed; matching is
/// case-insensitive. The `u32` is the section level (`BAB` = 1, `Bagian` = 2,
/// `Paragraf` = 3, `Pasal` = 4).
#[must_use]
pub fn detect_legal_marker(line: &str) -> Option<(Marker, u32)> {
    let text = strip_prefix(line);
    if is_bab(text) {
        Some((Marker::Bab, 1))
    } else if is_bagian(text) {
        Some((Marker::Bagian, 2))
    } else if is_paragraf(text) {
        Some((Marker::Paragraf, 3))
    } else if is_pasal(text) {
        Some((Marker::Pasal, 4))
    } else {
        None
    }
}

/// Marker and level of a heading title, falling back to a generic heading.
fn heading_marker(title: &str) -> (Marker, u32) {
    detect_legal_marker(title).unwrap_or((Marker::Heading, 1))
}

/// The heading path of node `i`: ancestor titles with a smaller level, plus
/// the node itself. Legal levels are numbers; a stored outline from a PDF
/// bookmark tree uses the same convention.
fn stored_path(nodes: &[crate::tree::OutlineNode], i: usize) -> Vec<String> {
    let node = &nodes[i];
    let mut stack: Vec<(u32, &str)> = Vec::new();
    for previous in nodes.iter().take(i) {
        if previous.level < node.level {
            stack.retain(|(level, _)| *level < previous.level);
            stack.push((previous.level, previous.title.as_str()));
        }
    }
    let mut path: Vec<String> = stack
        .into_iter()
        .map(|(_, title)| title.to_string())
        .collect();
    path.push(node.title.clone());
    path
}

/// Source 1: the stored outline from `Document.metadata.outline`. A node is
/// mapped to the first chunk on or after its zero-based start page. Chunks
/// without a page cannot be mapped, so an outline over a pageless document
/// falls through to the next source.
fn boundaries_from_stored(outline: &Outline, chunks: &[OutlineChunk]) -> Vec<Boundary> {
    let mut boundaries = Vec::new();
    for (i, node) in outline.nodes.iter().enumerate() {
        let Some(index) = chunks
            .iter()
            .find(|chunk| chunk.page.is_some_and(|page| page >= node.start_page))
            .map(|chunk| chunk.chunk_index)
        else {
            continue;
        };
        let (marker, level) = heading_marker(&node.title);
        boundaries.push(Boundary {
            chunk_index: index,
            title: node.title.clone(),
            level,
            path: stored_path(&outline.nodes, i),
            marker,
        });
    }
    boundaries
}

/// Source 2: a new `metadata.section` starts a new section. Consecutive chunks
/// with the same section are one section. A heading that is itself a legal
/// marker keeps that marker and level.
fn boundaries_from_headings(chunks: &[OutlineChunk]) -> Vec<Boundary> {
    let mut boundaries: Vec<Boundary> = Vec::new();
    let mut previous: Option<&str> = None;
    for chunk in chunks {
        let Some(section) = chunk
            .section
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if previous == Some(section) {
            continue;
        }
        let (marker, level) = heading_marker(section);
        boundaries.push(Boundary {
            chunk_index: chunk.chunk_index,
            title: section.to_string(),
            level,
            path: vec![section.to_string()],
            marker,
        });
        previous = Some(section);
    }
    boundaries
}

/// Source 3: legal markers at the start of lines in chunk content. A chunk can
/// open more than one section; every marker becomes a boundary at that chunk.
fn boundaries_from_markers(chunks: &[OutlineChunk]) -> Vec<Boundary> {
    let mut boundaries: Vec<Boundary> = Vec::new();
    for chunk in chunks {
        for line in chunk.content.lines() {
            let text = strip_prefix(line);
            if let Some((marker, level)) = detect_legal_marker(line) {
                let title = text.trim_end().to_string();
                if title.is_empty() {
                    continue;
                }
                let path = if marker == Marker::Bab {
                    vec![title.clone()]
                } else {
                    // Legal markers nest under the most recent Bab/Bagian seen
                    // in this document.
                    let mut parent: Vec<String> = boundaries
                        .iter()
                        .rev()
                        .find(|b| b.level < level)
                        .map(|b| b.path.clone())
                        .unwrap_or_default();
                    parent.push(title.clone());
                    parent
                };
                boundaries.push(Boundary {
                    chunk_index: chunk.chunk_index,
                    title,
                    level,
                    path,
                    marker,
                });
            }
        }
    }
    boundaries
}

/// Source 4: one section per page, at the first chunk of that page. Chunks
/// without a page fall into a single trailing `Page` section so no content is
/// lost.
fn boundaries_from_pages(chunks: &[OutlineChunk]) -> Vec<Boundary> {
    let mut boundaries: Vec<Boundary> = Vec::new();
    let mut seen_pages: Vec<u32> = Vec::new();
    for chunk in chunks {
        let Some(page) = chunk.page else {
            continue;
        };
        if seen_pages.contains(&page) {
            continue;
        }
        seen_pages.push(page);
        let title = format!("Page {}", page.saturating_add(1));
        boundaries.push(Boundary {
            chunk_index: chunk.chunk_index,
            title: title.clone(),
            level: 1,
            path: vec![title],
            marker: Marker::Page,
        });
    }
    if boundaries.is_empty() {
        // A format with no page numbering still gets one navigable section.
        if let Some(first) = chunks.first() {
            boundaries.push(Boundary {
                chunk_index: first.chunk_index,
                title: "Document".to_string(),
                level: 1,
                path: vec!["Document".to_string()],
                marker: Marker::Page,
            });
        }
    }
    boundaries
}

/// The number of characters a chunk contributes. Spans count the chunker's
/// normalised, non-overlap output; without spans the content's own char count
/// is the only honest estimate.
fn chunk_chars(chunk: &OutlineChunk) -> u32 {
    match (chunk.char_start, chunk.char_end) {
        (Some(start), Some(end)) => end.saturating_sub(start),
        _ => chunk.content.chars().count() as u32,
    }
}

fn range_chars(chunks: &[OutlineChunk]) -> u32 {
    chunks
        .iter()
        .map(chunk_chars)
        .fold(0u32, u32::saturating_add)
}

fn range_pages(chunks: &[OutlineChunk]) -> (Option<u32>, Option<u32>) {
    let pages = chunks.iter().filter_map(|chunk| chunk.page);
    pages.fold((None, None), |(low, high), page| {
        (
            Some(low.map_or(page, |value: u32| value.min(page))),
            Some(high.map_or(page, |value: u32| value.max(page))),
        )
    })
}

/// Turn detected boundaries into sections with contiguous chunk ranges. A
/// leading section is added when the first boundary is not chunk 0, so
/// preamble text is navigable too.
fn sections_from_boundaries(
    chunks: &[OutlineChunk],
    mut boundaries: Vec<Boundary>,
) -> Vec<Section> {
    if chunks.is_empty() || boundaries.is_empty() {
        return Vec::new();
    }
    boundaries.sort_by_key(|boundary| boundary.chunk_index);
    let last_index = chunks.last().map_or(0, |chunk| chunk.chunk_index);
    if boundaries[0].chunk_index > chunks[0].chunk_index {
        boundaries.insert(
            0,
            Boundary {
                chunk_index: chunks[0].chunk_index,
                title: "Preamble".to_string(),
                level: 1,
                path: vec!["Preamble".to_string()],
                marker: Marker::Heading,
            },
        );
    }

    boundaries
        .iter()
        .enumerate()
        .map(|(i, boundary)| {
            let next_start = boundaries
                .get(i + 1)
                .map_or(last_index.saturating_add(1), |next| next.chunk_index);
            let chunk_end = next_start
                .saturating_sub(1)
                .clamp(boundary.chunk_index, last_index);
            let in_range: Vec<&OutlineChunk> = chunks
                .iter()
                .filter(|chunk| {
                    chunk.chunk_index >= boundary.chunk_index && chunk.chunk_index <= chunk_end
                })
                .collect();
            let owned: Vec<OutlineChunk> = in_range.into_iter().cloned().collect();
            let (page_start, page_end) = range_pages(&owned);
            Section {
                id: format!("s{}", i + 1),
                title: boundary.title.clone(),
                level: boundary.level,
                path: boundary.path.clone(),
                page_start,
                page_end,
                chunk_start: boundary.chunk_index,
                chunk_end,
                estimated_chars: range_chars(&owned),
                marker: boundary.marker,
            }
        })
        .collect()
}

/// Build the outline v2 for one document from its ordered chunks and its
/// optional stored outline. The first source that yields any boundary wins;
/// an empty stored outline is treated as absent.
#[must_use]
pub fn build_outline(chunks: &[OutlineChunk], stored: Option<&Outline>) -> Vec<Section> {
    if chunks.is_empty() {
        return Vec::new();
    }
    let boundaries = stored
        .filter(|outline| !outline.nodes.is_empty())
        .map(|outline| boundaries_from_stored(outline, chunks))
        .filter(|boundaries| !boundaries.is_empty())
        .or_else(|| {
            let found = boundaries_from_headings(chunks);
            (!found.is_empty()).then_some(found)
        })
        .or_else(|| {
            let found = boundaries_from_markers(chunks);
            (!found.is_empty()).then_some(found)
        })
        .unwrap_or_else(|| boundaries_from_pages(chunks));
    sections_from_boundaries(chunks, boundaries)
}

/// Document-level character estimate, using the same per-chunk rule as sections.
#[must_use]
pub fn estimated_chars(chunks: &[OutlineChunk]) -> u32 {
    range_chars(chunks)
}

/// True only when every chunk carries a complete span. Old chunks carry none
/// of these fields, never a misleading zero.
#[must_use]
pub fn has_spans(chunks: &[OutlineChunk]) -> bool {
    !chunks.is_empty()
        && chunks.iter().all(|chunk| {
            chunk.char_start.is_some()
                && chunk.char_end.is_some()
                && chunk.overlap_prefix_len.is_some()
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::tree::{OutlineMethod, OutlineNode};

    fn chunk(
        index: u32,
        page: Option<u32>,
        section: Option<&str>,
        content: &str,
        span: Option<(u32, u32, u32)>,
    ) -> OutlineChunk {
        OutlineChunk {
            chunk_index: index,
            page,
            section: section.map(str::to_string),
            content: content.to_string(),
            char_start: span.map(|s| s.0),
            char_end: span.map(|s| s.1),
            overlap_prefix_len: span.map(|s| s.2),
        }
    }

    #[test]
    fn every_legal_marker_is_detected_case_insensitively_with_heading_prefixes() {
        assert_eq!(detect_legal_marker("BAB IV CUTI"), Some((Marker::Bab, 1)));
        assert_eq!(
            detect_legal_marker("  ## bab ix — ketentuan"),
            Some((Marker::Bab, 1))
        );
        assert_eq!(detect_legal_marker("BAB 1"), None);
        assert_eq!(
            detect_legal_marker("Bagian Kesatu"),
            Some((Marker::Bagian, 2))
        );
        assert_eq!(
            detect_legal_marker("### bagian kesebelas"),
            Some((Marker::Bagian, 2))
        );
        assert_eq!(detect_legal_marker("Bagian Belakang"), None);
        assert_eq!(
            detect_legal_marker("Paragraf 12"),
            Some((Marker::Paragraf, 3))
        );
        assert_eq!(detect_legal_marker("Paragraf XII"), None);
        assert_eq!(detect_legal_marker("Pasal 3A"), Some((Marker::Pasal, 4)));
        assert_eq!(detect_legal_marker("  pasal 27"), Some((Marker::Pasal, 4)));
        assert_eq!(detect_legal_marker("Pasal III"), None);
        assert_eq!(detect_legal_marker("Pasal 3AB"), None);
        assert_eq!(detect_legal_marker("ordinary heading"), None);
    }

    /// The regulation-shaped fixture the spec asks for, with one marker at
    /// the start of each chunk.
    fn regulation() -> Vec<OutlineChunk> {
        vec![
            chunk(
                0,
                Some(0),
                None,
                "BAB I KETENTUAN UMUM\nPasal 1",
                Some((0, 120, 0)),
            ),
            chunk(
                1,
                Some(0),
                None,
                "Pasal 2\nDetail ketentuan.",
                Some((120, 240, 12)),
            ),
            chunk(
                2,
                Some(1),
                None,
                "BAB II HAK DAN KEWAJIBAN\nBagian Kesatu",
                Some((240, 360, 0)),
            ),
            chunk(
                3,
                Some(1),
                None,
                "Pasal 3\nHak setiap pekerja.",
                Some((360, 500, 18)),
            ),
        ]
    }

    #[test]
    fn a_regulation_gives_a_bab_pasal_tree_with_sane_ranges() {
        let sections = build_outline(&regulation(), None);
        let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "BAB I KETENTUAN UMUM",
                "Pasal 1",
                "Pasal 2",
                "BAB II HAK DAN KEWAJIBAN",
                "Bagian Kesatu",
                "Pasal 3",
            ]
        );
        let bab = &sections[0];
        assert_eq!(bab.marker, Marker::Bab);
        assert_eq!(bab.level, 1);
        assert_eq!(bab.path, vec!["BAB I KETENTUAN UMUM"]);
        assert_eq!(bab.chunk_start, 0);
        assert_eq!(bab.chunk_end, 0);
        assert_eq!(bab.page_start, Some(0));

        let pasal_two = &sections[2];
        assert_eq!(pasal_two.marker, Marker::Pasal);
        assert_eq!(pasal_two.level, 4);
        assert_eq!(pasal_two.path, vec!["BAB I KETENTUAN UMUM", "Pasal 2"]);
        assert_eq!(pasal_two.chunk_start, 1);
        assert_eq!(pasal_two.chunk_end, 1);

        let pasal_three = &sections.last().unwrap();
        assert_eq!(pasal_three.id, "s6");
        assert_eq!(
            pasal_three.path,
            vec!["BAB II HAK DAN KEWAJIBAN", "Bagian Kesatu", "Pasal 3"]
        );
        assert_eq!(pasal_three.chunk_start, 3);
        assert_eq!(pasal_three.chunk_end, 3);
        assert_eq!(pasal_three.estimated_chars, 140);
    }

    #[test]
    fn stored_outline_wins_and_maps_nodes_to_chunks_by_page() {
        let chunks = vec![
            chunk(0, Some(0), None, "opening", Some((0, 10, 0))),
            chunk(1, Some(3), None, "middle", Some((10, 20, 0))),
            chunk(2, Some(7), None, "end", Some((20, 30, 0))),
        ];
        let stored = Outline {
            document_title: None,
            method: OutlineMethod::Bookmarks,
            nodes: vec![
                OutlineNode {
                    title: "1 Introduction".to_string(),
                    level: 1,
                    start_page: 0,
                    end_page: 2,
                },
                OutlineNode {
                    title: "2 Method".to_string(),
                    level: 2,
                    start_page: 3,
                    end_page: 9,
                },
            ],
        };
        let sections = build_outline(&chunks, Some(&stored));
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "1 Introduction");
        assert_eq!(sections[0].marker, Marker::Heading);
        assert_eq!(sections[0].chunk_start, 0);
        assert_eq!(sections[0].chunk_end, 0);
        assert_eq!(sections[1].title, "2 Method");
        assert_eq!(sections[1].path, vec!["1 Introduction", "2 Method"]);
        assert_eq!(sections[1].chunk_start, 1);
        assert_eq!(sections[1].chunk_end, 2);
    }

    #[test]
    fn an_unmappable_stored_outline_falls_through_to_headings() {
        let chunks = vec![
            chunk(0, None, Some("Introduction"), "a", None),
            chunk(1, None, Some("Method"), "b", None),
        ];
        let stored = Outline {
            document_title: None,
            method: OutlineMethod::Headings,
            nodes: vec![OutlineNode {
                title: "PDF bookmark".to_string(),
                level: 1,
                start_page: 40,
                end_page: 50,
            }],
        };
        let sections = build_outline(&chunks, Some(&stored));
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "Introduction");
        assert_eq!(sections[0].marker, Marker::Heading);
        assert_eq!(sections[1].title, "Method");
    }

    #[test]
    fn headings_only_document_uses_metadata_section() {
        let chunks = vec![
            chunk(0, Some(0), Some("1. Intro"), "a", None),
            chunk(1, Some(0), Some("1. Intro"), "b", None),
            chunk(2, Some(1), Some("2. Method"), "c", None),
        ];
        let sections = build_outline(&chunks, None);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "1. Intro");
        assert_eq!(sections[0].chunk_start, 0);
        assert_eq!(sections[0].chunk_end, 1);
        assert_eq!(sections[0].estimated_chars, 2);
        assert_eq!(sections[1].title, "2. Method");
        assert_eq!(sections[1].page_start, Some(1));
    }

    #[test]
    fn a_document_with_no_structure_gets_one_section_per_page() {
        let chunks = vec![
            chunk(0, Some(0), None, "a", None),
            chunk(1, Some(1), None, "bb", None),
            chunk(2, Some(1), None, "ccc", None),
        ];
        let sections = build_outline(&chunks, None);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "Page 1");
        assert_eq!(sections[0].marker, Marker::Page);
        assert_eq!(sections[0].chunk_start, 0);
        assert_eq!(sections[0].chunk_end, 0);
        assert_eq!(sections[1].title, "Page 2");
        assert_eq!(sections[1].chunk_start, 1);
        assert_eq!(sections[1].chunk_end, 2);
        assert_eq!(sections[1].estimated_chars, 5);
    }

    #[test]
    fn a_pageless_document_still_gets_one_navigable_section() {
        let chunks = vec![
            chunk(0, None, None, "a", None),
            chunk(1, None, None, "b", None),
        ];
        let sections = build_outline(&chunks, None);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].marker, Marker::Page);
        assert_eq!(sections[0].page_start, None);
        assert_eq!(sections[0].chunk_end, 1);
    }

    #[test]
    fn a_multibyte_title_keeps_its_characters_and_char_count() {
        let chunks = vec![chunk(
            0,
            Some(0),
            None,
            "BAB I KETENTUAN UMUM — LEMBAGA PENDIDIKAN\nIsi pasal.",
            Some((0, 25, 0)),
        )];
        let sections = build_outline(&chunks, None);
        assert_eq!(
            sections[0].title,
            "BAB I KETENTUAN UMUM — LEMBAGA PENDIDIKAN"
        );
        assert_eq!(sections[0].estimated_chars, 25);

        let without_spans = vec![chunk(0, Some(0), None, "Pasal 1 — éducation", None)];
        let sections = build_outline(&without_spans, None);
        assert_eq!(sections[0].title, "Pasal 1 — éducation");
        assert_eq!(
            sections[0].estimated_chars,
            "Pasal 1 — éducation".chars().count() as u32
        );
    }

    #[test]
    fn spans_control_the_estimate_and_has_spans_is_all_or_nothing() {
        let chunks = regulation();
        assert!(has_spans(&chunks));
        assert_eq!(estimated_chars(&chunks), 120 + 120 + 120 + 140);
        let mut mixed = chunks;
        mixed[1].char_end = None;
        assert!(!has_spans(&mixed));
        assert!(!has_spans(&[]));
    }

    #[test]
    fn preamble_is_a_section_when_structure_starts_later() {
        let chunks = vec![
            chunk(0, Some(0), None, "cover page", None),
            chunk(1, Some(1), None, "BAB I ISI", None),
        ];
        let sections = build_outline(&chunks, None);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "Preamble");
        assert_eq!(sections[0].chunk_start, 0);
        assert_eq!(sections[0].chunk_end, 0);
        assert_eq!(sections[1].title, "BAB I ISI");
    }
}
