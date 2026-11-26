//! Contextual chunk headers: the `contextualPrefix` every chunk carries.
//!
//! A textbook page repeats its activity template on almost every page —
//! "Bagas BENI Here are what we will do. First, we will …" — so two chunks
//! a hundred pages apart can carry near-identical text and the retriever has
//! nothing to tell them apart by. The header is the missing where-am-I line:
//! `<document title> > <chapter> > <section>`, built from the section
//! outline the extractor already produced (`kb_extract::outline`, DEC-4) and
//! the page every chunk sits on.
//!
//! The prefix is stored on the chunk's metadata and **never folded into
//! `content`**: `content` is the passage the answer cites verbatim, and a
//! header welded into it would be quoted back at the caller. The consumers
//! that need the header — the embedding text, the BM25 sparse vector and the
//! reranker input — prepend it themselves (`kb_engine::ingest` and
//! `kb_engine::retrieval`), so one stored value serves all three.
//!
//! Pure: outline nodes, pages and strings in, prefixes out.

use super::document::Chunk;
use crate::tree::{Outline, OutlineNode};

/// The header cap, in characters.
///
/// Long enough for "Judul buku > BAB II Kewirausahaan > A. Pengertian
/// wirausaha", short enough that it cannot crowd out the passage it is
/// meant to introduce — the embedder's char budget and the reranker's both
/// treat it as a bounded header, not a second passage.
pub const CONTEXT_PREFIX_MAX_CHARS: usize = 160;

/// The outline nodes a page sits under, outermost first.
///
/// A node covers the pages of its span `[start_page, end_page]`. At one level
/// the spans do not overlap (each node ends where the next same-or-higher
/// node starts), so the result is one node per level: the chapter, then the
/// sections inside it, innermost last.
#[must_use]
pub fn nodes_for_page(nodes: &[OutlineNode], page: u32) -> Vec<&OutlineNode> {
    let mut covering: Vec<&OutlineNode> = nodes
        .iter()
        .filter(|node| page >= node.start_page && page <= node.end_page)
        .collect();
    covering.sort_by(|a, b| {
        a.level
            .cmp(&b.level)
            .then_with(|| a.start_page.cmp(&b.start_page))
    });
    covering
}

/// The header for one chunk: `title > chapter > section`, capped at
/// [`CONTEXT_PREFIX_MAX_CHARS`] characters.
///
/// A segment equal to the title (ignoring case) is dropped rather than
/// repeated, the same dedupe [`crate::title::heading_line`] applies to a
/// heading path that already starts with the document's own H1.
#[must_use]
pub fn contextual_prefix(title: &str, path: &[&OutlineNode]) -> Option<String> {
    let title = title.trim();
    let mut parts: Vec<String> = Vec::with_capacity(path.len() + 1);
    if !title.is_empty() {
        parts.push(title.to_string());
    }
    for node in path {
        let segment = node.title.trim();
        if segment.is_empty() || segment.eq_ignore_ascii_case(title) {
            continue;
        }
        parts.push(segment.to_string());
    }
    let joined = parts.join(" > ");
    cap_header(&joined)
}

/// The header for a chunk with no outline to read: the markdown chunker's own
/// heading path, or the title alone.
///
/// This is the shape documents without pages (a markdown file, a plain text
/// upload) get. `heading_line` joins title and path with the same " > " the
/// outline path uses, and dedupes the title when the path already starts
/// with it, so the fallback produces exactly the line the embedding text
/// has always carried ([`crate::title::embedding_text`]) — the header adds
/// the same words to the sparse and reranker inputs that the dense arm
/// already saw.
#[must_use]
pub fn fallback_prefix(title: &str, heading_path: Option<&str>) -> Option<String> {
    let line = crate::title::heading_line(title, heading_path);
    cap_header(&line)
}

/// Shorten to the cap on a character boundary, dropping a partial trailing
/// segment. `None` when nothing is left.
fn cap_header(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= CONTEXT_PREFIX_MAX_CHARS {
        return Some(trimmed.to_string());
    }
    let mut cut: String = trimmed.chars().take(CONTEXT_PREFIX_MAX_CHARS).collect();
    // A cut that landed mid-segment drops the partial segment: "Judul > BAB II
    // Kewiraus" reads as a corrupt heading; "Judul > BAB II" reads as the
    // chapter, honestly abridged.
    if let Some(split) = cut.rfind(" > ") {
        cut.truncate(split);
    }
    let cut = cut.trim();
    (!cut.is_empty()).then(|| cut.to_string())
}

/// Give every chunk its contextual header, in place.
///
/// For each chunk, best evidence first:
///
/// * the outline node path covering the chunk's **page** — the extracted
///   hierarchy, which already knows where every chapter starts;
/// * no outline (or a chunk without a page, such as a table spanning none):
///   the chunker's own heading path, which the markdown chunker fills from
///   the `#` headings above the passage;
/// * neither: the document title alone — still better than nothing, which is
///   what every chunk had before.
///
/// `content` is never touched: the header lives in metadata, and the
/// consumers prepend it when they need it.
pub fn assign_contextual_prefixes(chunks: &mut [Chunk], title: &str, outline: Option<&Outline>) {
    let nodes = outline.map_or(&[][..], |o| o.nodes.as_slice());
    for chunk in chunks.iter_mut() {
        let prefix = match (chunk.metadata.page, nodes.is_empty()) {
            (Some(page), false) => {
                let path = nodes_for_page(nodes, page);
                if path.is_empty() {
                    // A page before the first heading belongs to no section;
                    // the title is all the outline can say about it.
                    contextual_prefix(title, &[])
                } else {
                    contextual_prefix(title, &path)
                }
            }
            _ => fallback_prefix(title, chunk.metadata.heading_path.as_deref()),
        };
        chunk.metadata.contextual_prefix = prefix;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::chunk::ChunkMetadata;

    fn node(title: &str, level: u32, start: u32, end: u32) -> OutlineNode {
        OutlineNode {
            title: title.to_string(),
            level,
            start_page: start,
            end_page: end,
        }
    }

    fn chunk(content: &str, page: Option<u32>, heading_path: Option<&str>) -> Chunk {
        Chunk {
            content: content.to_string(),
            metadata: ChunkMetadata {
                document_title: "Buku".to_string(),
                category: "textbook".to_string(),
                subcategory: None,
                section: None,
                heading_path: heading_path.map(str::to_string),
                chunk_index: 0,
                contextual_prefix: None,
                chunk_type: Some("text".to_string()),
                page,
                asset_key: None,
                anchor_chunk_index: None,
                table_id: None,
            },
        }
    }

    fn textbook_outline() -> Outline {
        Outline {
            document_title: None,
            method: crate::tree::OutlineMethod::Headings,
            nodes: vec![
                node("BAB I Kewirausahaan", 1, 0, 9),
                node("A. Pengertian wirausaha", 2, 1, 4),
                node("B. Ciri wirausaha", 2, 5, 9),
                node("BAB II Pasar dan Uang", 1, 10, 19),
                node("A. Jenis pasar", 2, 10, 14),
            ],
        }
    }

    #[test]
    fn a_page_sits_under_its_chapter_and_innermost_section() {
        let outline = textbook_outline();
        let path = nodes_for_page(&outline.nodes, 6);
        let titles: Vec<&str> = path.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["BAB I Kewirausahaan", "B. Ciri wirausaha"],
            "outermost first, innermost last, one node per level"
        );
        // A boundary page belongs to the section that starts on it.
        let path = nodes_for_page(&outline.nodes, 10);
        let titles: Vec<&str> = path.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, vec!["BAB II Pasar dan Uang", "A. Jenis pasar"]);
    }

    #[test]
    fn the_header_is_title_chapter_section() {
        let outline = textbook_outline();
        let path = nodes_for_page(&outline.nodes, 6);
        let prefix = contextual_prefix("Ekonomi SMA", &path).unwrap();
        assert_eq!(
            prefix,
            "Ekonomi SMA > BAB I Kewirausahaan > B. Ciri wirausaha"
        );
    }

    #[test]
    fn the_title_is_not_repeated_when_a_node_says_it() {
        let titled = node("Ekonomi SMA", 1, 0, 9);
        let prefix = contextual_prefix("Ekonomi SMA", &[&titled]).unwrap();
        assert_eq!(prefix, "Ekonomi SMA");
    }

    #[test]
    fn a_long_header_is_capped_at_a_whole_segment() {
        let long_title = "Buku Ekonomi untuk SMA Kelas XI Kurikulum Merdeka Edisi Terbaru";
        let long_section = node(&"S".repeat(200), 2, 0, 9);
        let prefix = contextual_prefix(long_title, &[&long_section]).unwrap();
        assert!(
            prefix.chars().count() <= CONTEXT_PREFIX_MAX_CHARS,
            "{prefix}"
        );
        // The partial segment was dropped, not left half a word.
        assert!(prefix.starts_with(long_title), "{prefix}");
        assert!(!prefix.ends_with('S'), "{prefix}");
    }

    #[test]
    fn two_identical_passages_under_different_chapters_get_different_headers() {
        // The measured failure: the same activity template on every page,
        // two hundred pages apart. Only the header tells them apart.
        let boilerplate = "Bagas BENI Here are what we will do. First, we will \
                           read the text. Second, we will discuss it in groups.";
        let outline = textbook_outline();
        let mut chunks = vec![
            chunk(boilerplate, Some(3), None),
            chunk(boilerplate, Some(12), None),
        ];
        assign_contextual_prefixes(&mut chunks, "Ekonomi SMA", Some(&outline));
        assert_eq!(
            chunks[0].metadata.contextual_prefix.as_deref(),
            Some("Ekonomi SMA > BAB I Kewirausahaan > A. Pengertian wirausaha")
        );
        assert_eq!(
            chunks[1].metadata.contextual_prefix.as_deref(),
            Some("Ekonomi SMA > BAB II Pasar dan Uang > A. Jenis pasar")
        );
        assert_eq!(chunks[0].content, boilerplate, "content is untouched");
    }

    #[test]
    fn a_page_before_the_first_heading_gets_the_title_alone() {
        let outline = textbook_outline();
        let mut chunks = vec![chunk("Kata pengantar", Some(50), None)];
        assign_contextual_prefixes(&mut chunks, "Ekonomi SMA", Some(&outline));
        assert_eq!(
            chunks[0].metadata.contextual_prefix.as_deref(),
            Some("Ekonomi SMA")
        );
    }

    #[test]
    fn without_an_outline_the_markdown_heading_path_is_the_header() {
        let mut chunks = vec![chunk("isi", None, Some("Bab Bagian > Subbagian"))];
        assign_contextual_prefixes(&mut chunks, "Judul", None);
        assert_eq!(
            chunks[0].metadata.contextual_prefix.as_deref(),
            Some("Judul > Bab Bagian > Subbagian")
        );
        // A path that already starts with the title does not repeat it.
        let mut chunks = vec![chunk("isi", None, Some("Judul > Bagian"))];
        assign_contextual_prefixes(&mut chunks, "Judul", None);
        assert_eq!(
            chunks[0].metadata.contextual_prefix.as_deref(),
            Some("Judul > Bagian")
        );
    }

    #[test]
    fn a_chunk_with_no_structure_at_all_still_names_its_document() {
        let mut chunks = vec![chunk("isi", None, None)];
        assign_contextual_prefixes(&mut chunks, "Judul", None);
        assert_eq!(
            chunks[0].metadata.contextual_prefix.as_deref(),
            Some("Judul")
        );
        // A title-less document keeps `None` rather than an empty string.
        let mut chunks = vec![chunk("isi", None, None)];
        assign_contextual_prefixes(&mut chunks, "  ", None);
        assert_eq!(chunks[0].metadata.contextual_prefix, None);
    }

    #[test]
    fn an_empty_outline_leaves_the_fallback_in_charge() {
        let outline = Outline::empty(crate::tree::OutlineMethod::None);
        let mut chunks = vec![chunk("isi", Some(4), Some("Bagian"))];
        assign_contextual_prefixes(&mut chunks, "Judul", Some(&outline));
        assert_eq!(
            chunks[0].metadata.contextual_prefix.as_deref(),
            Some("Judul > Bagian")
        );
    }
}
