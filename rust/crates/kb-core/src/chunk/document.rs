//! Storage-shape chunk and recursive splitter compatibility layer.

use fancy_regex::Regex;
use serde::{Deserialize, Serialize};

use super::smart::{SmartChunkingOptions, smart_chunk};
use super::utf16::utf16_len;

/// Storage-shape chunk, mirroring the `Chunk` TypeScript interface used by
/// ingest and by `prepareChunkForEmbedding`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub content: String,
    pub metadata: ChunkMetadata,
}

/// Storage-shape metadata. Same field names as the Bun service's `Chunk` so
/// ingest → store → retrieval stays compatible.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChunkMetadata {
    pub document_title: String,
    pub category: String,
    pub subcategory: Option<String>,
    pub section: Option<String>,
    /// The chunk's heading path: the headings above it, outermost first,
    /// joined with " > ". `None` for a document with no headings.
    ///
    /// This is what the embedding text puts after the title
    /// ([`crate::title::embedding_text`]), so a chunk from the middle of a
    /// document still says where it came from.
    #[serde(default)]
    pub heading_path: Option<String>,
    pub chunk_index: u32,
    pub contextual_prefix: Option<String>,
    pub chunk_type: Option<String>,
    pub page: Option<u32>,
    #[serde(default)]
    pub asset_key: Option<String>,
    #[serde(default)]
    pub anchor_chunk_index: Option<u32>,
}

impl crate::page_map::HasContent<ChunkMetadata> for Chunk {
    fn content_owned(&self) -> String {
        self.content.clone()
    }
    fn content_for_meta(&self) -> String {
        self.content.clone()
    }
    fn metadata_owned(&self) -> ChunkMetadata {
        self.metadata.clone()
    }
    fn page(&self) -> Option<u32> {
        self.metadata.page
    }
}

impl crate::page_map::PageSettable for ChunkMetadata {
    fn set_page(&mut self, page: Option<u32>) {
        self.page = page;
    }
}

/// Options for [`chunk_document`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ChunkOptions {
    pub chunk_size: Option<u32>,
    pub chunk_overlap: Option<u32>,
}

/// Recursive separator splitter. Mirror of `chunker.ts::recursiveSplit` — keep
/// separators priority-ordered so sections split before paragraphs before
/// sentences before words.
#[must_use]
pub fn chunk_document(
    content: &str,
    document_title: &str,
    category: &str,
    subcategory: Option<&str>,
    options: ChunkOptions,
) -> Vec<Chunk> {
    let size = options.chunk_size.unwrap_or(1000);
    let overlap = options.chunk_overlap.unwrap_or(200);
    let separators = ["\n## ", "\n### ", "\n#### ", "\n\n", "\n", ". ", " "];
    let raw = recursive_split(content, &separators, size as usize);
    let overlapped = add_overlap(raw, overlap as usize);
    overlapped
        .into_iter()
        .enumerate()
        .map(|(i, body)| {
            let section = extract_section_header(&body);
            Chunk {
                content: body.trim().to_string(),
                metadata: ChunkMetadata {
                    document_title: document_title.to_string(),
                    category: category.to_string(),
                    subcategory: subcategory.map(str::to_string),
                    section,
                    chunk_index: i as u32,
                    ..Default::default()
                },
            }
        })
        .filter(|c| !c.content.is_empty())
        .collect()
}

fn recursive_split(text: &str, separators: &[&str], chunk_size: usize) -> Vec<String> {
    if utf16_len(text) <= chunk_size || separators.is_empty() {
        return vec![text.to_string()];
    }
    let sep = separators[0];
    let rest = &separators[1..];
    let parts: Vec<&str> = text.split(sep).collect();
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for part in parts {
        let candidate = if current.is_empty() {
            part.to_string()
        } else {
            format!("{current}{sep}{part}")
        };
        if utf16_len(&candidate) <= chunk_size {
            current = candidate;
        } else {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            if utf16_len(part) > chunk_size {
                chunks.extend(recursive_split(part, rest, chunk_size));
            } else {
                current = part.to_string();
            }
        }
    }
    if !current.is_empty() {
        chunks.push(std::mem::take(&mut current));
    }
    chunks
}

fn add_overlap(chunks: Vec<String>, overlap: usize) -> Vec<String> {
    if overlap == 0 || chunks.len() <= 1 {
        return chunks;
    }
    let mut out = Vec::with_capacity(chunks.len());
    for (i, body) in chunks.into_iter().enumerate() {
        if i == 0 {
            out.push(body);
            continue;
        }
        let prev = &out[i - 1];
        let prev_utf16: Vec<u16> = prev.encode_utf16().collect();
        let start = prev_utf16.len().saturating_sub(overlap);
        let mut bytes_start = prev.len();
        let mut utf16_seen = 0usize;
        for (b, ch) in prev.char_indices() {
            if utf16_seen >= start {
                bytes_start = b;
                break;
            }
            utf16_seen += ch.len_utf16();
        }
        let overlap_text = &prev[bytes_start..];
        out.push(format!("{overlap_text}{body}"));
    }
    out
}

fn extract_section_header(text: &str) -> Option<String> {
    let re = Regex::new(r"^#+\s+(.+)$").ok()?;
    let caps = re.captures(text).ok().flatten()?;
    caps.get(1).map(|m| m.as_str().to_string())
}

/// Render the prompt-prep form of a chunk. Mirrors `prepareChunkForEmbedding`
/// in `chunker.ts`.
#[must_use]
pub fn prepare_chunk_for_embedding(chunk: &Chunk) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("Category: {}", chunk.metadata.category));
    if let Some(sub) = &chunk.metadata.subcategory {
        parts.push(format!("Topic: {sub}"));
    }
    if let Some(section) = &chunk.metadata.section {
        parts.push(format!("Section: {section}"));
    }
    if let Some(prefix) = &chunk.metadata.contextual_prefix {
        let trimmed = prefix.trim();
        if !trimmed.is_empty() {
            parts.push(format!("Context: {trimmed}"));
        }
    }
    parts.push(String::new());
    parts.push(chunk.content.clone());
    parts.join("\n")
}

/// Convenience: chunk a document with the smart chunker and re-wrap the
/// output into the storage-shape [`Chunk`].
#[must_use]
pub fn smart_chunk_document(
    content: &str,
    document_title: &str,
    category: &str,
    subcategory: Option<&str>,
    options: SmartChunkingOptions,
) -> Vec<Chunk> {
    let smart = smart_chunk(content, options);
    smart
        .into_iter()
        .map(|sc| Chunk {
            content: sc.text,
            metadata: ChunkMetadata {
                document_title: document_title.to_string(),
                category: category.to_string(),
                subcategory: subcategory.map(str::to_string),
                section: sc
                    .metadata
                    .section
                    .clone()
                    .or_else(|| sc.metadata.hierarchy_path.as_ref().map(|h| h.join(" > "))),
                heading_path: sc.metadata.hierarchy_path.as_ref().and_then(|path| {
                    let joined = path.join(" > ");
                    (!joined.trim().is_empty()).then_some(joined)
                }),
                chunk_index: sc.chunk_index,
                chunk_type: Some(sc.metadata.chunk_type),
                page: sc.metadata.page_number,
                contextual_prefix: None,
                asset_key: None,
                anchor_chunk_index: None,
            },
        })
        .filter(|c| !c.content.is_empty())
        .collect()
}

/// Chunk a paged document one page at a time, when its content **is** its
/// pages.
///
/// `pages` is the extractor's `(0-based page, text)` list. When `content` is
/// exactly those texts joined by a blank line (the PDF text-layer path: one
/// entry per page), every page is chunked on its own, so no chunk crosses a
/// page break and every chunk carries its page by construction. Chunk indices
/// run on across pages, as one call over the whole document would number them.
///
/// Returns `None` when `content` is anything else (a layout parser's merged
/// markdown, block-level page maps): the caller chunks the whole document and
/// tags pages afterwards, as before.
///
/// Why: tagging pages after chunking the whole text put 19% of the chunks of
/// the ViDoRe v3 HR PDFs on the wrong page (nearly all one page early, misled
/// by running headers and the 200-character overlap), and 15% straddled a
/// page break. Chunking per page lifted page-level nDCG@10 there from 0.477 to
/// 0.566 with the reranker (`docs/eval-ab/vidore3-hr-en-ours`).
#[must_use]
pub fn smart_chunk_pages(
    content: &str,
    pages: &[(u32, String)],
    document_title: &str,
    category: &str,
    subcategory: Option<&str>,
    options: SmartChunkingOptions,
) -> Option<Vec<Chunk>> {
    if pages.is_empty() {
        return None;
    }
    let joined_len: usize =
        pages.iter().map(|(_, t)| t.len()).sum::<usize>() + 2 * (pages.len() - 1);
    if joined_len != content.len() {
        return None;
    }
    let mut rest = content;
    for (i, (_, text)) in pages.iter().enumerate() {
        rest = rest.strip_prefix(text.as_str())?;
        if i + 1 < pages.len() {
            rest = rest.strip_prefix("\n\n")?;
        }
    }
    if !rest.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    for (page, text) in pages {
        for mut chunk in smart_chunk_document(text, document_title, category, subcategory, options)
        {
            chunk.metadata.chunk_index = u32::try_from(out.len()).unwrap_or(u32::MAX);
            chunk.metadata.page = Some(*page);
            out.push(chunk);
        }
    }
    Some(out)
}
