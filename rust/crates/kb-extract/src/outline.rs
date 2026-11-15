//! PDF outline extraction: the document's section hierarchy for the tree arm.
//!
//! This is a port of EVAL-2's prototype (`rust/crates/kb-eval/scripts/outline.py`),
//! with the same two methods and the same rules:
//!
//! * **bookmarks** — the PDF's own outline. pdfium exposes it as a tree; each
//!   bookmark's destination gives its page. The bookmark's nesting is the level,
//!   but a title that carries its own numbering ("2.1.1") is trusted over the
//!   nesting, because producers sometimes elide parent bookmarks and the number
//!   in the title is exact.
//! * **headings** — no bookmarks. Every page's text is scanned for a numbered
//!   heading ("3.4 Title", "Chapter 3 …"). Table-of-contents pages are skipped
//!   first, or every heading would be "discovered" on the contents page and win
//!   the first-occurrence dedupe.
//! * **none** — neither produced anything. Reported, not silently dropped, so a
//!   corpus with missing structure is visible.
//!
//! End pages follow the rule a reader uses: a section ends on the page before
//! the next section at the same or a higher level starts. A section that shares
//! its start page with its first child therefore has a one-page span, which is
//! the honest answer.
//!
//! Page numbers in [`OutlineNode`] are **zero-based**, matching
//! `PageText.page`; the prototype printed 1-based PDF pages and the conversion
//! happens here, once.

use std::collections::HashSet;

use kb_core::tree::{Outline, OutlineMethod, OutlineNode, numbering_level};
use pdfium_render::prelude::{PdfDocument, Pdfium};

/// Parse the PDF's bookmark tree, in document order.
///
/// A bookmark with no destination is skipped but its children are still
/// visited: it groups rather than points at a page.
#[must_use]
pub fn bookmark_nodes(document: &PdfDocument<'_>) -> Vec<OutlineNode> {
    let Some(root) = document.bookmarks().root() else {
        return Vec::new();
    };
    let mut raw: Vec<(String, u32, u32)> = Vec::new();
    {
        let mut visited: HashSet<usize> = HashSet::new();
        let mut stack: Vec<(pdfium_render::prelude::PdfBookmark<'_>, u32)> = vec![(root, 1)];
        while let Some((bookmark, depth)) = stack.pop() {
            // A cyclic outline must not loop forever: producers do write them.
            let handle = handle_of(&bookmark);
            if !visited.insert(handle) {
                continue;
            }
            if let Some(title) = bookmark.title()
                && let Some(page) = bookmark
                    .destination()
                    .and_then(|destination| destination.page_index().ok())
            {
                raw.push((clean_title(&title), depth, u32::try_from(page).unwrap_or(0)));
            }
            // DFS pre-order: the next sibling waits while the children are
            // visited, because document order is what end pages depend on.
            if let Some(sibling) = bookmark.next_sibling() {
                stack.push((sibling, depth));
            }
            let mut children: Vec<pdfium_render::prelude::PdfBookmark<'_>> =
                bookmark.iter_direct_children().collect();
            children.reverse();
            for child in children {
                stack.push((child, depth + 1));
            }
        }
    }
    raw.into_iter()
        .map(|(title, depth, page)| {
            let level = numbering_level(&title).unwrap_or(depth.max(1));
            OutlineNode {
                title,
                level,
                start_page: page,
                end_page: page,
            }
        })
        .collect()
}

/// The internal handle of a bookmark, for the visited set.
///
/// `PdfBookmark` implements `Hash`, so hashing it is the supported identity
/// check; the hash is used here because the iterator needs an owned key and the
/// handle itself is private.
fn handle_of(bookmark: &pdfium_render::prelude::PdfBookmark<'_>) -> usize {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    bookmark.hash(&mut hasher);
    hasher.finish() as usize
}

/// Collapse whitespace and remove a stray trailing footnote marker.
#[must_use]
pub fn clean_title(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    strip_footnote_marker(&collapsed)
}

/// Remove a trailing " (3F )" / " (12)" that bookmark producers add.
fn strip_footnote_marker(text: &str) -> String {
    let trimmed = text.trim_end();
    let Some(open) = trimmed.rfind('(') else {
        return trimmed.to_string();
    };
    if !trimmed.ends_with(')') {
        return trimmed.to_string();
    }
    let inner = &trimmed[open + 1..trimmed.len() - 1];
    let mut digits = inner.chars();
    let first = digits.next();
    if first.is_some_and(|c| c.is_ascii_digit())
        && digits.all(|c| c.is_ascii_digit() || c.is_ascii_alphabetic() || c == ' ')
        && inner.len() <= 6
    {
        return trimmed[..open].trim_end().to_string();
    }
    trimmed.to_string()
}

/// Numbered headings found in page text, in document order, first wins.
///
/// `pages` is one string per page, zero-based, with empty strings for pages
/// that have no text layer.
#[must_use]
pub fn heading_nodes(pages: &[String]) -> Vec<OutlineNode> {
    let mut nodes: Vec<OutlineNode> = Vec::new();
    let mut seen: HashSet<(String, u32)> = HashSet::new();
    for (page_index, text) in pages.iter().enumerate() {
        if is_toc_page(text) {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let candidate = line.trim();
            if candidate.is_empty() || candidate.chars().count() > 120 {
                continue;
            }
            let (title, level) = heading_of(candidate, &lines, index);
            let Some(title) = title else {
                continue;
            };
            let Some(level) = level else {
                continue;
            };
            // Drop a heading that is really a sentence or a figure caption.
            let words = title.split_whitespace().count();
            if words > 16 || title.ends_with('.') {
                continue;
            }
            let key = (dedupe_key(&title), level);
            if !seen.insert(key) {
                continue;
            }
            nodes.push(OutlineNode {
                title,
                level,
                start_page: u32::try_from(page_index).unwrap_or(u32::MAX),
                end_page: u32::try_from(page_index).unwrap_or(u32::MAX),
            });
        }
    }
    nodes
}

/// The title and level of one candidate line, if it looks like a heading.
fn heading_of(candidate: &str, lines: &[&str], index: usize) -> (Option<String>, Option<u32>) {
    if let Some(chapter) = chapter_parts(candidate) {
        let mut title = clean_title(candidate);
        // A bare "Chapter 3" often has its title on the next line.
        if chapter.is_empty()
            && let Some(following) = lines.get(index + 1).map(|line| line.trim())
            && !following.is_empty()
            && following.chars().count() < 80
            && following.chars().next().is_some_and(char::is_uppercase)
        {
            title = clean_title(&format!("{title} {following}"));
        }
        return (Some(title), Some(1));
    }
    if let Some(number) = dotted_number(candidate) {
        let level = u32::try_from(number.split('.').count()).unwrap_or(1);
        return (Some(clean_title(candidate)), Some(level));
    }
    if let Some(body) = top_numbered_body(candidate)
        && body.chars().count() < 90
    {
        return (Some(clean_title(candidate)), Some(1));
    }
    (None, None)
}

/// The text after "Chapter N:" when the line is a chapter heading.
fn chapter_parts(candidate: &str) -> Option<String> {
    let lower = candidate.to_lowercase();
    let rest = lower
        .strip_prefix("chapter")
        .or_else(|| lower.strip_prefix("part"))?;
    let rest = rest.trim_start();
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        // Roman numerals: "Part II".
        let roman: String = rest
            .chars()
            .take_while(|c| {
                matches!(
                    c.to_ascii_lowercase(),
                    'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
                )
            })
            .collect();
        if roman.is_empty() || roman.len() > 4 {
            return None;
        }
        // The roman numeral is ASCII, so this cut is on a char boundary. The
        // old `candidate[rest.len()..]` sliced the original line at the
        // remainder's length, which lands inside a multi-byte character on
        // lines like "Part II – Análisis" and panicked the ingest task.
        return Some(
            rest[roman.len()..]
                .trim_start_matches(['.', ':', ' '])
                .to_string(),
        );
    }
    let after = rest[digits.len()..].trim_start_matches(['.', ':', ' ']);
    Some(after.to_string())
}

/// The dotted number at the start of a heading ("4.2", "2.1.1").
fn dotted_number(candidate: &str) -> Option<String> {
    let stripped = candidate
        .strip_prefix("section ")
        .or_else(|| candidate.strip_prefix("sec. "))
        .or_else(|| candidate.strip_prefix("Section "))
        .or_else(|| candidate.strip_prefix("Sec. "))
        .unwrap_or(candidate);
    let first = stripped.split_whitespace().next()?;
    let number = first.trim_end_matches('.');
    let parts: Vec<&str> = number.split('.').collect();
    if parts.len() < 2
        || !parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    // A dotted heading must have a title after the number.
    let rest = stripped[first.len()..].trim();
    if rest.is_empty() {
        return None;
    }
    Some(number.to_string())
}

/// The body of a top-level numbered heading ("3. Labour market developments").
fn top_numbered_body(candidate: &str) -> Option<&str> {
    let first = candidate.split_whitespace().next()?;
    let number = first.trim_end_matches('.');
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = candidate[first.len()..].trim();
    let mut chars = rest.chars();
    let first_char = chars.next()?;
    if !first_char.is_uppercase() {
        return None;
    }
    // At least three characters of body, and no sentence-ending dot after them.
    let body: String = rest.chars().take(3).collect();
    if body.chars().count() < 3 || rest.contains('.') {
        return None;
    }
    Some(rest)
}

fn dedupe_key(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
        .collect::<String>()
        .trim()
        .to_string()
}

/// A table-of-contents line ends in a page number, often after dot leaders.
fn is_toc_page(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return false;
    }
    let toc_lines = lines
        .iter()
        .filter(|line| has_trailing_page_number(line))
        .count();
    let top = lines
        .iter()
        .take(4)
        .map(|line| line.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    toc_lines >= 3 && (top.contains("contents") || toc_lines * 2 >= lines.len())
}

fn has_trailing_page_number(line: &str) -> bool {
    let trimmed = line.trim_end();
    let digits: String = trimmed
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if digits.is_empty() || digits.len() > 4 {
        return false;
    }
    let before = &trimmed[..trimmed.len() - digits.len()];
    let before = before.trim_end();
    // Dot leaders or two spaces, as `pdftotext -layout` prints a contents line.
    before.ends_with("...") || before.ends_with("  ")
}

/// Fill in end pages: the page before the next node at the same or a higher
/// level starts, clamped to the last page.
///
/// A report whose bookmarks are all flat gives a chapter a one-page span,
/// because the very next bookmark is a subsection on the same page. A chapter's
/// real boundary is the next chapter heading, whatever the bookmark depth
/// claims, so that rule overrides the level rule when the title says "Chapter".
#[must_use]
pub fn finish_nodes(mut nodes: Vec<OutlineNode>, pages: u32) -> Vec<OutlineNode> {
    let last_page = pages.saturating_sub(1);
    for index in 0..nodes.len() {
        let mut end = last_page;
        for later in &nodes[index + 1..] {
            if later.level <= nodes[index].level {
                end = later.start_page.saturating_sub(1);
                break;
            }
        }
        nodes[index].end_page = end.max(nodes[index].start_page).min(last_page);
    }
    for index in 0..nodes.len() {
        if !nodes[index].title.to_lowercase().starts_with("chapter") {
            continue;
        }
        for later in &nodes[index + 1..] {
            if later.title.to_lowercase().starts_with("chapter")
                && later.start_page > nodes[index].start_page
            {
                nodes[index].end_page = later.start_page.saturating_sub(1);
                break;
            }
        }
    }
    nodes
}

/// Extract a PDF's outline: bookmarks first, numbered headings as the fallback.
#[must_use]
pub fn extract_outline(pdfium: &Pdfium, pdf_bytes: &[u8], pages: &[String]) -> Outline {
    let Ok(document) = pdfium.load_pdf_from_byte_slice(pdf_bytes, None) else {
        return Outline::empty(OutlineMethod::None);
    };
    let page_count = u32::try_from(document.pages().len()).unwrap_or(0);
    let bookmarks = bookmark_nodes(&document);
    if !bookmarks.is_empty() {
        return Outline {
            document_title: None,
            method: OutlineMethod::Bookmarks,
            nodes: finish_nodes(bookmarks, page_count),
        };
    }
    let headings = heading_nodes(pages);
    if headings.is_empty() {
        Outline::empty(OutlineMethod::None)
    } else {
        Outline {
            document_title: None,
            method: OutlineMethod::Headings,
            nodes: finish_nodes(headings, page_count),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_roman_part_heading_with_accents_does_not_panic() {
        for line in [
            "Part II – Análisis económico",
            "Part CIÓN de la población",
            "Part IV: Información",
        ] {
            assert!(chapter_parts(line).is_some(), "{line}");
        }
        assert_eq!(
            chapter_parts("Part II – Análisis").as_deref(),
            Some("– análisis")
        );
    }
    use crate::pdfium::get_pdfium;

    /// Build a minimal, valid PDF from object bodies. Offsets are computed, so
    /// the xref is correct without hand-counting bytes.
    fn pdf_of(objects: &[&str]) -> Vec<u8> {
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", index + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    /// Three blank pages and an outline: Chapter 1 (page 0) with 1.1 (page 1),
    /// then Chapter 2 (page 2).
    fn bookmarked_pdf() -> Vec<u8> {
        pdf_of(&[
            "<< /Type /Catalog /Pages 2 0 R /Outlines 6 0 R >>",
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            "<< /Type /Outlines /First 7 0 R /Last 9 0 R /Count 2 >>",
            "<< /Title (Chapter 1 Introduction) /Parent 6 0 R /Dest [3 0 R /Fit] /First 8 0 R /Last 8 0 R /Count 1 /Next 9 0 R >>",
            "<< /Title (1.1 Background) /Parent 7 0 R /Dest [4 0 R /Fit] >>",
            "<< /Title (Chapter 2 Findings (3F)) /Parent 6 0 R /Dest [5 0 R /Fit] >>",
        ])
    }

    #[test]
    fn bookmarks_become_nodes_with_zero_based_pages_and_end_pages() {
        let Ok(pdfium) = get_pdfium() else {
            eprintln!("Skipping bookmarks test: libpdfium not found");
            return;
        };
        let outline = extract_outline(pdfium, &bookmarked_pdf(), &[]);
        assert_eq!(outline.method, OutlineMethod::Bookmarks, "{outline:?}");
        let titles: Vec<&str> = outline.nodes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Chapter 1 Introduction",
                "1.1 Background",
                "Chapter 2 Findings"
            ]
        );
        let pages: Vec<(u32, u32)> = outline
            .nodes
            .iter()
            .map(|n| (n.start_page, n.end_page))
            .collect();
        assert_eq!(pages, vec![(0, 1), (1, 1), (2, 2)]);
        assert_eq!(outline.nodes[1].level, 2);
    }

    #[test]
    fn a_pdf_without_bookmarks_has_none_rather_than_an_empty_bookmark_outline() {
        let Ok(pdfium) = get_pdfium() else {
            eprintln!("Skipping test: libpdfium not found");
            return;
        };
        let outline = extract_outline(
            pdfium,
            crate::pdfium::MINIMAL_PDF_HELLO_WORLD,
            &["Hello World".to_string()],
        );
        assert_eq!(outline.method, OutlineMethod::None);
        assert!(outline.is_empty());
    }

    #[test]
    fn heading_fallback_finds_numbered_headings_and_skips_the_contents_page() {
        let pages = vec![
            "Contents\n1. Introduction .......... 1\n2. Methods .......... 2\n3. Results .......... 3"
                .to_string(),
            "1. Introduction\nSome body text that is not a heading.".to_string(),
            "2. Methods\n2.1 Sampling\nWe drew a sample.".to_string(),
            "Chapter 3 Results\nEverything worked.".to_string(),
        ];
        let nodes = heading_nodes(&pages);
        let titles: Vec<&str> = nodes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "1. Introduction",
                "2. Methods",
                "2.1 Sampling",
                "Chapter 3 Results"
            ]
        );
        assert_eq!(nodes[2].level, 2);
        assert_eq!(nodes[1].start_page, 2);

        let finished = finish_nodes(nodes, 4);
        // "2. Methods" is closed by the next level-1 heading on page 3.
        assert_eq!(finished[1].end_page, 2);
        assert_eq!(finished[3].end_page, 3);
    }

    #[test]
    fn a_repeated_heading_is_kept_once_at_the_higher_level() {
        let pages = vec![
            "4.2 Leave entitlement\ntext".to_string(),
            "4.2 Leave entitlement\ntext again".to_string(),
        ];
        let nodes = heading_nodes(&pages);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].start_page, 0);
    }

    #[test]
    fn a_sentence_with_a_leading_number_is_not_a_heading() {
        let pages = vec!["3. This is a sentence that ends with a full stop.".to_string()];
        assert!(heading_nodes(&pages).is_empty());
    }

    #[test]
    fn a_bare_chapter_line_takes_its_title_from_the_next_line() {
        let pages = vec!["Chapter 4\nSocial policy\nBody".to_string()];
        let nodes = heading_nodes(&pages);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].title, "Chapter 4 Social policy");
        assert_eq!(nodes[0].level, 1);
    }

    #[test]
    fn clean_title_collapses_space_and_drops_a_footnote_marker() {
        assert_eq!(
            clean_title("  Chapter   2   Findings (3F) "),
            "Chapter 2 Findings"
        );
        // Same rule as the prototype: a trailing parenthesised number is a footnote
        // marker, even when it reads like a year.
        assert_eq!(clean_title("4.2 Leave (2024)"), "4.2 Leave");
    }

    #[test]
    fn a_flat_outline_still_closes_a_chapter_at_the_next_chapter() {
        let nodes = vec![
            OutlineNode {
                title: "Chapter 1 Introduction".into(),
                level: 1,
                start_page: 0,
                end_page: 0,
            },
            OutlineNode {
                title: "1.1 Background".into(),
                level: 2,
                start_page: 0,
                end_page: 0,
            },
            OutlineNode {
                title: "Chapter 2 Findings".into(),
                level: 1,
                start_page: 5,
                end_page: 5,
            },
        ];
        let finished = finish_nodes(nodes, 10);
        assert_eq!(finished[0].end_page, 4);
        assert_eq!(finished[2].end_page, 9);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod bench {
    use super::*;
    use crate::pdfium::get_pdfium;
    use std::time::Instant;

    fn pdf_of(objects: &[String]) -> Vec<u8> {
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", index + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    #[test]
    #[ignore = "manual outline-cost measurement, not a gate"]
    fn measure_outline_cost_on_a_2000_page_pdf() {
        let Ok(pdfium) = get_pdfium() else {
            return;
        };
        let pages = 2000usize;
        let mut objects: Vec<String> = Vec::new();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", i + 3)).collect();
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ));
        for _ in 0..pages {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {} 0 R >> >> /Contents {} 0 R >>",
                pages + 3,
                pages + 4
            ));
        }
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
        let body = "BT /F1 12 Tf 72 720 Td (2.1 Labour market developments) Tj ET";
        objects.push(format!(
            "<< /Length {} >>\nstream\n{body}\nendstream",
            body.len()
        ));
        let bytes = pdf_of(&objects);
        let started = Instant::now();
        let result = crate::pdfium::extract_pdf_text_layer(pdfium, &bytes).unwrap();
        let total = started.elapsed();
        let page_texts: Vec<String> = result
            .page_map
            .as_ref()
            .unwrap()
            .iter()
            .map(|p| p.text.clone())
            .collect();
        let started = Instant::now();
        let outline = extract_outline(pdfium, &bytes, &page_texts);
        let added = started.elapsed();
        println!(
            "pages={pages} text+outline={total:?} outline_alone={added:?} method={:?} nodes={}",
            outline.method,
            outline.nodes.len()
        );
    }
}
