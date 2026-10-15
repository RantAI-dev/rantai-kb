//! Pure helpers for figure assets (multimodal RAG).
//!
//! Mirrors pure logic from `src/lib/rag/figure-assets.ts`:
//! - Normalizing block types to UI-facing kinds
//! - Extracting surrounding page text to enrich caption-less figures
//! - Deriving section/caption labels from context
//! - Anchoring figures to preceding prose blocks in reading order

use std::collections::HashMap;

/// Normalizes extractor's raw block type to a UI-facing figure kind.
/// Mirrors `normalizeFigureType` in `src/lib/rag/figure-assets.ts:24`.
#[must_use]
pub fn normalize_figure_type(raw: &str) -> &'static str {
    match raw {
        "chart" => "chart",
        "table" => "table",
        "image" | "image_block" => "image",
        _ => "figure",
    }
}

/// Map figure type to display kind word in Indonesian.
#[must_use]
pub fn figure_kind_word(raw: &str) -> &'static str {
    match raw {
        "table" => "Tabel",
        "chart" => "Grafik",
        _ => "Gambar",
    }
}

/// Build a page -> text map from page-tagged text chunks so caption-less figures
/// can borrow the prose around them. Capped at 1200 characters per page.
/// Mirrors `buildPageText` in `src/lib/rag/figure-assets.ts:40`.
#[must_use]
pub fn build_page_text<S: AsRef<str>>(chunks: &[(Option<u32>, S)]) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    for (page_opt, content) in chunks {
        let Some(p) = *page_opt else { continue };
        let prev = map.entry(p).or_insert_with(String::new);
        if prev.len() < 1200 {
            if !prev.is_empty() {
                prev.push(' ');
            }
            prev.push_str(content.as_ref().trim());
        }
    }
    map
}

/// Context text for a figure: same page first, then neighbours (p+1, p-1).
/// Mirrors `contextForPage` in `src/lib/rag/figure-assets.ts:51`.
#[must_use]
pub fn context_for_page(page_text: &HashMap<u32, String>, page: u32) -> String {
    if let Some(text) = page_text.get(&page).filter(|t| !t.trim().is_empty()) {
        return text.trim().to_string();
    }
    if let Some(text) = page_text.get(&(page.saturating_add(1))).filter(|t| !t.trim().is_empty()) {
        return text.trim().to_string();
    }
    if page > 0 {
        if let Some(text) = page_text.get(&(page - 1)).filter(|t| !t.trim().is_empty()) {
            return text.trim().to_string();
        }
    }
    String::new()
}

/// Derive a human label for a caption-less figure from the first sentence or heading
/// in surrounding text (between 8 and 120 chars), falling back to first 100 chars.
/// Mirrors `deriveLabel` in `src/lib/rag/figure-assets.ts:63`.
#[must_use]
pub fn derive_label(context: &str) -> Option<String> {
    if context.trim().is_empty() {
        return None;
    }

    let parts = context.split(|c| c == '\n' || c == '.' || c == ';');
    for part in parts {
        let trimmed = part.trim();
        if trimmed.len() >= 8 && trimmed.len() <= 120 {
            return Some(trimmed.to_string());
        }
    }

    let truncated: String = context.chars().take(100).collect();
    let trimmed = truncated.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Minimal view of a page block in reading order for figure anchoring.
#[derive(Debug, Clone)]
pub struct LayoutBlockRef<'a> {
    /// "text", "caption", or "figure"
    pub kind: &'a str,
    /// Identifier matching the figure's id
    pub id: Option<&'a str>,
    /// Body text of text or caption block
    pub text: Option<&'a str>,
}

/// Locate which text chunk each figure belongs to based on reading-order page blocks.
///
/// Matches on the 4 to 10 opening words of the prose block immediately preceding the figure.
/// Mirrors `resolveFigureAnchors` in `src/lib/rag/figure-assets.ts:98`.
#[must_use]
pub fn resolve_figure_anchors(
    pages_blocks: &[Vec<LayoutBlockRef<'_>>],
    text_chunks: &[(u32, &str)], // (chunk_index, normalized_content)
) -> HashMap<String, u32> {
    let mut anchors = HashMap::new();
    if pages_blocks.is_empty() || text_chunks.is_empty() {
        return anchors;
    }

    let norm = |s: &str| -> String {
        s.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };

    let normalized_chunks: Vec<(u32, String)> = text_chunks
        .iter()
        .map(|(idx, content)| (*idx, norm(content)))
        .collect();

    for page in pages_blocks {
        for (i, block) in page.iter().enumerate() {
            if block.kind != "figure" {
                continue;
            }
            let Some(fig_id) = block.id else { continue };

            // Walk backwards for the nearest prose block with length >= 25
            let mut words: Option<Vec<String>> = None;
            for j in (0..i).rev() {
                if let Some(t) = page[j].text {
                    let trimmed = t.trim();
                    if trimmed.len() >= 25 {
                        let ws: Vec<String> = norm(trimmed)
                            .split_whitespace()
                            .map(str::to_string)
                            .collect();
                        if ws.len() >= 4 {
                            words = Some(ws);
                            break;
                        }
                    }
                }
            }

            let Some(word_vec) = words else { continue };

            // Match from 10 down to 4 opening words
            let mut matched_idx = None;
            for n in (4..=word_vec.len().min(10)).rev() {
                let probe = word_vec[..n].join(" ");
                if let Some((idx, _)) = normalized_chunks.iter().find(|(_, text)| text.contains(&probe)) {
                    matched_idx = Some(*idx);
                    break;
                }
            }

            if let Some(idx) = matched_idx {
                anchors.insert(fig_id.to_string(), idx);
            }
        }
    }

    anchors
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn normalize_types_match_typescript() {
        assert_eq!(normalize_figure_type("chart"), "chart");
        assert_eq!(normalize_figure_type("table"), "table");
        assert_eq!(normalize_figure_type("image"), "image");
        assert_eq!(normalize_figure_type("image_block"), "image");
        assert_eq!(normalize_figure_type("anything_else"), "figure");
    }

    #[test]
    fn build_page_text_and_context_window() {
        let chunks = vec![
            (Some(1), "Page 1 intro text."),
            (Some(1), "Page 1 second paragraph."),
            (Some(2), "Page 2 content text."),
        ];
        let map = build_page_text(&chunks);
        assert!(map.get(&1).unwrap().contains("Page 1 intro text. Page 1 second paragraph."));

        assert_eq!(context_for_page(&map, 1), "Page 1 intro text. Page 1 second paragraph.");
        // Fallback to neighbour
        assert_eq!(context_for_page(&map, 0), "Page 1 intro text. Page 1 second paragraph.");
    }

    #[test]
    fn derive_label_extracts_sentence_or_fragment() {
        let ctx = "Siklus air menjelaskan perpindahan air dari bumi ke atmosfer. Proses ini terus berulang.";
        let label = derive_label(ctx).unwrap();
        assert_eq!(label, "Siklus air menjelaskan perpindahan air dari bumi ke atmosfer");
    }

    #[test]
    fn resolve_figure_anchors_matches_preceding_prose() {
        let blocks = vec![vec![
            LayoutBlockRef {
                kind: "text",
                id: None,
                text: Some("Gerak benda pada permukaan kasar menghasilkan gaya gesek yang besar."),
            },
            LayoutBlockRef {
                kind: "figure",
                id: Some("fig-1"),
                text: None,
            },
        ]];

        let text_chunks = vec![
            (0, "Pengantar fisika dasar."),
            (1, "Gerak benda pada permukaan kasar menghasilkan gaya gesek yang besar dan panas."),
        ];

        let anchors = resolve_figure_anchors(&blocks, &text_chunks);
        assert_eq!(anchors.get("fig-1"), Some(&1));
    }
}
