//! Smart document chunker.
//!
//! Ported from `src/lib/rag/smart-chunker.ts` and `chunker.ts`. Sizes count
//! UTF-16 code units (the JS string `.length`) because that is the unit the
//! original implementation measured against: a Rust `char` is a Unicode scalar
//! value, which can be 1 or 2 UTF-16 units, so `String::chars().count()` would
//! drift on CJK-heavy documents. We translate UTF-16 unit count via
//! `encode_utf16`.

use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

// ─── UTF-16 length helpers ──────────────────────────────────────────────────
//
// The JS implementation measures `string.length`, which counts UTF-16 code
// units. We mirror that exactly so chunk-size thresholds line up with the
// data the Bun service produced.

/// Number of UTF-16 code units in `s`. Returns 0 for empty input.
#[must_use]
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

// ─── Smart chunker types ────────────────────────────────────────────────────

/// Chunking strategy selector. The original `smart` strategy is the only one
/// that exercises the full block/sentence fallback path; the others delegate
/// to the legacy recursive splitter (`chunk_document`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChunkingStrategy {
    Smart,
    Semantic,
    FixedSize,
    StructureAware,
}

/// Public options for [`smart_chunk`] and [`smart_chunk_document`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SmartChunkingOptions {
    /// Maximum UTF-16 units per chunk (default: 800).
    pub max_chunk_size: Option<u32>,
    /// Overlap in UTF-16 units between chunks (default: 200).
    pub overlap_size: Option<u32>,
    /// Keep code blocks intact (default: true).
    pub preserve_code_blocks: Option<bool>,
    /// Don't split across headings (default: true).
    pub respect_heading_boundaries: Option<bool>,
    /// Don't split across sections (default: true).
    pub respect_section_boundaries: Option<bool>,
    /// Strategy (default: `smart`).
    pub strategy: Option<ChunkingStrategy>,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedOptions {
    max_chunk_size: u32,
    overlap_size: u32,
    preserve_code_blocks: bool,
    respect_heading_boundaries: bool,
    respect_section_boundaries: bool,
    #[allow(dead_code)]
    strategy: ChunkingStrategy,
}

impl ResolvedOptions {
    fn from(o: SmartChunkingOptions) -> Self {
        Self {
            max_chunk_size: o.max_chunk_size.unwrap_or(800),
            overlap_size: o.overlap_size.unwrap_or(200),
            preserve_code_blocks: o.preserve_code_blocks.unwrap_or(true),
            respect_heading_boundaries: o.respect_heading_boundaries.unwrap_or(true),
            respect_section_boundaries: o.respect_section_boundaries.unwrap_or(true),
            strategy: o.strategy.unwrap_or(ChunkingStrategy::Smart),
        }
    }
}

/// One metadata field on a smart chunk. See `SmartChunkMetadata` in the TS
/// port for the equivalent shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmartChunkMetadata {
    pub chunk_type: String,
    pub document_section: Option<String>,
    pub heading_level: Option<u32>,
    pub hierarchy_path: Option<Vec<String>>,
    pub page_number: Option<u32>,
    pub section: Option<String>,
}

/// One chunk produced by [`smart_chunk`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmartChunk {
    pub chunk_index: u32,
    pub metadata: SmartChunkMetadata,
    pub text: String,
}

// ─── Sentence / heading / table regexes ────────────────────────────────────────

// JS uses /(?<=[.!?])\s+(?=[A-Z0-9])/g. fancy-regex supports lookbehind.
static SENTENCE_BOUNDARY: LazyLock<Regex> = LazyLock::lock(|| {
    Regex::new(r"(?<=[.!?])\s+(?=[A-Z0-9])").expect("sentence boundary regex")
});

static HEADING_LINE: LazyLock<Regex> =
    LazyLock::lock(|| Regex::new(r"^(#{1,6})\s+(.+)").expect("heading regex"));

static LIST_ITEM: LazyLock<Regex> =
    LazyLock::lock(|| Regex::new(r"^[\d*+.-]\s+").expect("list-item regex"));

// ─── Smart chunker ──────────────────────────────────────────────────────────

/// Smart document chunker. The TypeScript class is async because the original
/// code `await`s nothing inside `chunk`; the Rust API keeps the same shape so
/// the call sites stay readable.
pub struct SmartChunker {
    opts: ResolvedOptions,
}

impl SmartChunker {
    /// Build with the given options. Any unset option falls back to the
    /// original defaults.
    #[must_use]
    pub fn new(options: SmartChunkingOptions) -> Self {
        Self {
            opts: ResolvedOptions::from(options),
        }
    }

    /// Chunk a markdown document.
    pub fn chunk(&self, markdown: &str) -> Vec<SmartChunk> {
        let mut chunks: Vec<SmartChunk> = Vec::new();
        let mut chunk_index: u32 = 0;

        let mut blocks = Self::split_into_blocks(markdown);
        let total_length = utf16_len(markdown);
        let avg_block_size = if blocks.is_empty() {
            total_length
        } else {
            total_length / blocks.len()
        };

        // Wall-of-text fallback: when average block is huge, sentence-split
        // oversized prose. Tables and code stay atomic.
        if avg_block_size > (self.opts.max_chunk_size * 2) as usize
            && total_length > self.opts.max_chunk_size as usize
        {
            blocks = blocks
                .into_iter()
                .flat_map(|b| {
                    let t = Self::detect_structure(&b).chunk_type;
                    if t == "table" || t == "code" {
                        vec![b]
                    } else if utf16_len(&b) > (self.opts.max_chunk_size * 2) as usize {
                        Self::split_by_sentences(&b, self.opts.max_chunk_size)
                    } else {
                        vec![b]
                    }
                })
                .collect();
        }

        let mut current_chunk = String::new();
        let mut current_metadata = SmartChunkMetadata {
            chunk_type: "text".to_string(),
            document_section: None,
            heading_level: None,
            hierarchy_path: None,
            page_number: None,
            section: None,
        };
        let mut current_hierarchy: Vec<String> = Vec::new();

        for block in &blocks {
            let mut structure = Self::detect_structure(block);

            // Update hierarchy on heading.
            if structure.chunk_type == "heading" {
                if let Some(level) = structure.heading_level {
                    current_hierarchy =
                        Self::update_hierarchy(&current_hierarchy, block, level);
                    structure.hierarchy_path = Some(current_hierarchy.clone());
                }
            }

            // Atomic tables and code: each becomes its own chunk; never merged
            // with surrounding prose. Retrieval then treats these specially.
            if structure.chunk_type == "table" || structure.chunk_type == "code" {
                if utf16_len(&current_chunk.trim()) > 0 {
                    chunks.push(SmartChunk {
                        chunk_index,
                        metadata: current_metadata.clone(),
                        text: current_chunk.trim().to_string(),
                    });
                    chunk_index += 1;
                    current_chunk.clear();
                }
                let meta = SmartChunkMetadata {
                    hierarchy_path: structure
                        .hierarchy_path
                        .clone()
                        .or_else(|| Some(current_hierarchy.clone())),
                    ..structure.clone()
                };
                chunks.push(SmartChunk {
                    chunk_index,
                    metadata: meta,
                    text: block.trim().to_string(),
                });
                chunk_index += 1;
                continue;
            }

            // Boundary flush: heading or section change.
            let should_flush = (self.opts.respect_heading_boundaries
                && structure.chunk_type == "heading"
                && utf16_len(&current_chunk) > 0)
                || (self.opts.respect_section_boundaries
                    && structure.section.is_some()
                    && structure.section != current_metadata.section
                    && utf16_len(&current_chunk) > 0);

            if should_flush {
                chunks.push(SmartChunk {
                    chunk_index,
                    metadata: current_metadata.clone(),
                    text: current_chunk.trim().to_string(),
                });
                chunk_index += 1;
                current_chunk.clear();
            }

            // Size flush + overlap.
            if utf16_len(&current_chunk) + utf16_len(block) > self.opts.max_chunk_size as usize
                && utf16_len(&current_chunk) > 0
            {
                chunks.push(SmartChunk {
                    chunk_index,
                    metadata: current_metadata.clone(),
                    text: current_chunk.trim().to_string(),
                });
                chunk_index += 1;
                let overlap = Self::get_overlap(&current_chunk, self.opts.overlap_size);
                current_chunk = overlap;
            }

            // Append the block.
            if current_chunk.is_empty() {
                current_chunk.push_str(block);
            } else {
                current_chunk.push_str("\n\n");
                current_chunk.push_str(block);
            }
            current_metadata = SmartChunkMetadata {
                hierarchy_path: structure
                    .hierarchy_path
                    .clone()
                    .or_else(|| Some(current_hierarchy.clone())),
                ..structure.clone()
            };
        }

        // Flush remaining.
        if utf16_len(current_chunk.trim()) > 0 {
            chunks.push(SmartChunk {
                chunk_index,
                metadata: current_metadata,
                text: current_chunk.trim().to_string(),
            });
        }

        chunks
    }

    fn detect_structure(block: &str) -> SmartChunkMetadata {
        let mut meta = SmartChunkMetadata {
            chunk_type: "text".to_string(),
            document_section: None,
            heading_level: None,
            hierarchy_path: None,
            page_number: None,
            section: None,
        };

        if let Some(caps) = HEADING_LINE.captures(block).ok().flatten() {
            let hashes = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let title = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("");
            meta.chunk_type = "heading".to_string();
            meta.heading_level = Some(hashes.chars().count() as u32);
            meta.section = Some(title.to_string());
            meta.document_section = Some(title.to_string());
            return meta;
        }
        if block.contains('|') && block.contains("---") {
            meta.chunk_type = "table".to_string();
            return meta;
        }
        if block.starts_with("```") && block.trim_end().ends_with("```") && block.len() > 6 {
            meta.chunk_type = "code".to_string();
            return meta;
        }
        if LIST_ITEM.is_match(block).unwrap_or(false) {
            meta.chunk_type = "list".to_string();
            return meta;
        }
        meta
    }

    fn update_hierarchy(current: &[String], heading: &str, level: u32) -> Vec<String> {
        let clean = HEADING_LINE
            .replace_all(heading.trim(), "$1")
            .to_string();
        let clean = clean.trim_start_matches(|c: char| c == '#' || c.is_whitespace());
        let mut out: Vec<String> = current.iter().take((level as usize).saturating_sub(1)).cloned().collect();
        out.push(clean.to_string());
        out
    }

    fn get_overlap(text: &str, size: u32) -> String {
        if utf16_len(text) <= size as usize {
            return text.to_string();
        }
        let start = utf16_len(text).saturating_sub(size as usize);
        // `text` is char-aligned; `start` is a UTF-16 index. Re-derive a char
        // boundary by walking encode_utf16 once.
        let mut utf16_seen = 0usize;
        let mut char_start_byte = 0usize;
        for (i, ch) in text.char_indices() {
            let units = ch.len_utf16();
            if utf16_seen + units > start {
                char_start_byte = i;
                break;
            }
            utf16_seen += units;
            char_start_byte = i + ch.len_utf8();
        }
        let overlap = &text[char_start_byte..];
        // Try to break at ". " to start a sentence.
        if let Some(idx) = overlap.find(". ") {
            return overlap[idx + 2..].to_string();
        }
        overlap.to_string()
    }

    fn split_by_sentences(text: &str, target_size: u32) -> Vec<String> {
        let splits = SENTENCE_BOUNDARY.split(text);
        let sentences: Vec<&str> = splits.filter(|s| !s.trim().is_empty()).collect();
        if sentences.len() <= 1 {
            return Self::split_by_fixed_size(text, target_size);
        }
        let mut blocks: Vec<String> = Vec::new();
        let mut current = String::new();
        let target = target_size as usize;
        for sentence in sentences {
            let next_len = utf16_len(&current) + utf16_len(sentence) + if current.is_empty() { 0 } else { 1 };
            if next_len > target && utf16_len(&current) > 0 {
                blocks.push(current.trim().to_string());
                current = sentence.to_string();
            } else if current.is_empty() {
                current.push_str(sentence);
            } else {
                current.push(' ');
                current.push_str(sentence);
            }
        }
        if !current.trim().is_empty() {
            blocks.push(current.trim().to_string());
        }
        blocks
    }

    fn split_by_fixed_size(text: &str, chunk_size: u32) -> Vec<String> {
        let mut blocks: Vec<String> = Vec::new();
        let size = chunk_size as usize;
        // Walk in UTF-16 units, snapping cuts to char boundaries.
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let mut start_utf16 = 0usize;
        while start_utf16 < utf16.len() {
            let mut end_utf16 = (start_utf16 + size).min(utf16.len());

            // Try to break at a space boundary in [start..end).
            if end_utf16 < utf16.len() {
                let slice = &utf16[start_utf16..end_utf16];
                if let Some(rel) = slice.iter().rposition(|u| *u == b' ' as u16) {
                    if rel > size / 2 {
                        end_utf16 = start_utf16 + rel;
                    }
                }
            }

            // Char-aligned byte range for [start_utf16..end_utf16].
            let mut bytes_start = text.len();
            let mut utf16_seen = 0usize;
            for (b, ch) in text.char_indices() {
                if utf16_seen >= start_utf16 {
                    bytes_start = b;
                    break;
                }
                utf16_seen += ch.len_utf16();
            }
            let mut bytes_end = text.len();
            let mut utf16_seen = 0usize;
            for (b, ch) in text.char_indices() {
                let next = utf16_seen + ch.len_utf16();
                if next >= end_utf16 {
                    bytes_end = b + ch.len_utf8();
                    break;
                }
                utf16_seen = next;
            }
            if bytes_start >= bytes_end {
                break;
            }
            let s = text[bytes_start..bytes_end].trim();
            if !s.is_empty() {
                blocks.push(s.to_string());
            }
            // Avoid an infinite loop on zero-width slices.
            if end_utf16 == start_utf16 {
                start_utf16 += 1;
            } else {
                start_utf16 = end_utf16;
            }
        }
        blocks
    }

    fn split_into_blocks(text: &str) -> Vec<String> {
        let mut blocks: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut in_code_block = false;
        let mut in_table = false;

        for line in text.split('\n') {
            if line.trim_start().starts_with("```") {
                in_code_block = !in_code_block;
                current.push_str(line);
                current.push('\n');
                if !in_code_block {
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        blocks.push(trimmed);
                    }
                    current.clear();
                }
                continue;
            }

            if line.contains('|') && line.contains("---") {
                in_table = true;
            }
            if in_code_block || in_table {
                current.push_str(line);
                current.push('\n');
                if in_table && !line.contains('|') {
                    in_table = false;
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        blocks.push(trimmed);
                    }
                    current.clear();
                }
                continue;
            }

            if line.trim().is_empty() {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    blocks.push(trimmed);
                }
                current.clear();
            } else {
                current.push_str(line);
                current.push('\n');
            }
        }
        let trimmed = current.trim().to_string();
        if !trimmed.is_empty() {
            blocks.push(trimmed);
        }
        blocks.into_iter().filter(|b| utf16_len(b) > 0).collect()
    }
}

/// One-shot helper.
pub fn smart_chunk(markdown: &str, options: SmartChunkingOptions) -> Vec<SmartChunk> {
    SmartChunker::new(options).chunk(markdown)
}

// ─── Compat layer: `Chunk` interface + `prepareChunkForEmbedding` ────────────

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
    pub chunk_index: u32,
    pub contextual_prefix: Option<String>,
    pub chunk_type: Option<String>,
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
    let overlapped = add_overlap(&raw, overlap as usize);
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
                chunks.push(current);
                current.clear();
            }
            if utf16_len(part) > chunk_size {
                chunks.extend(recursive_split(part, rest, chunk_size));
            } else {
                current = part.to_string();
            }
        }
    }
    if !current.is_empty() {
        chunks.push(current);
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
        // Walk back `overlap` UTF-16 units from the end of prev.
        let prev_utf16: Vec<u16> = prev.encode_utf16().collect();
        let start = prev_utf16.len().saturating_sub(overlap);
        // Reconstruct a char-aligned byte boundary at `start`.
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
/// in `chunker.ts`. The contextual prefix line is omitted when missing or
/// blank.
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
                section: sc.metadata.section.clone().or_else(|| {
                    sc.metadata
                        .hierarchy_path
                        .as_ref()
                        .map(|h| h.join(" > "))
                }),
                chunk_index: sc.chunk_index,
                chunk_type: Some(sc.metadata.chunk_type),
                ..Default::default()
            },
        })
        .filter(|c| !c.content.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_chunks() {
        let out = SmartChunker::new(SmartChunkingOptions::default()).chunk("");
        assert!(out.is_empty());
    }

    #[test]
    fn whitespace_only_yields_no_chunks() {
        let out = SmartChunker::new(SmartChunkingOptions::default()).chunk("   \n\n   ");
        assert!(out.is_empty());
    }

    #[test]
    fn short_text_is_one_chunk() {
        let out = SmartChunker::new(SmartChunkingOptions {
            max_chunk_size: Some(500),
            ..Default::default()
        })
        .chunk("Hello world.");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Hello world.");
        assert_eq!(out[0].chunk_index, 0);
    }

    #[test]
    fn long_text_yields_multiple_chunks() {
        let out = SmartChunker::new(SmartChunkingOptions {
            max_chunk_size: Some(100),
            overlap_size: Some(0),
            ..Default::default()
        })
        .chunk(&vec!["This is a paragraph of moderate length for testing."; 10].join("\n\n"));
        assert!(out.len() > 1);
    }

    #[test]
    fn chunk_indices_are_sequential() {
        let out = SmartChunker::new(SmartChunkingOptions {
            max_chunk_size: Some(50),
            overlap_size: Some(0),
            ..Default::default()
        })
        .chunk("First paragraph.\n\nSecond paragraph.\n\nThird paragraph.");
        for (i, c) in out.iter().enumerate() {
            assert_eq!(c.chunk_index, i as u32);
        }
    }

    #[test]
    fn heading_text_is_preserved() {
        let out = SmartChunker::new(SmartChunkingOptions::default()).chunk("# Title\n\nSome body text.");
        let joined = out.iter().map(|c| c.text.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("# Title"));
        assert!(joined.contains("Some body text"));
    }

    #[test]
    fn hierarchy_path_present_for_heading() {
        let out = SmartChunker::new(SmartChunkingOptions::default()).chunk("## Section\n\nContent here.");
        let has = out.iter().any(|c| {
            c.metadata
                .hierarchy_path
                .as_ref()
                .map(|h| !h.is_empty())
                .unwrap_or(false)
        });
        assert!(has);
    }

    #[test]
    fn nested_headings_build_hierarchy() {
        let out = SmartChunker::new(SmartChunkingOptions::default())
            .chunk("# Chapter 1\n\n## Section A\n\nContent under section A.");
        let last = out.last().expect("at least one chunk");
        let path = last.metadata.hierarchy_path.as_ref().expect("hierarchy path");
        assert!(!path.is_empty());
    }

    #[test]
    fn flushes_at_heading_boundary() {
        let out = SmartChunker::new(SmartChunkingOptions {
            max_chunk_size: Some(10_000),
            respect_heading_boundaries: Some(true),
            ..Default::default()
        })
        .chunk("# Heading 1\n\nParagraph one.\n\n# Heading 2\n\nParagraph two.");
        assert!(out.len() >= 2);
    }

    #[test]
    fn fewer_chunks_when_boundaries_disabled() {
        let text = "# Heading 1\n\nParagraph one.\n\n# Heading 2\n\nParagraph two.";
        let with = SmartChunker::new(SmartChunkingOptions {
            max_chunk_size: Some(10_000),
            respect_heading_boundaries: Some(true),
            ..Default::default()
        })
        .chunk(text);
        let without = SmartChunker::new(SmartChunkingOptions {
            max_chunk_size: Some(10_000),
            respect_heading_boundaries: Some(false),
            ..Default::default()
        })
        .chunk(text);
        assert!(without.len() <= with.len());
    }

    #[test]
    fn code_blocks_preserved_atomically() {
        let out = SmartChunker::new(SmartChunkingOptions {
            preserve_code_blocks: Some(true),
            ..Default::default()
        })
        .chunk("Some intro.\n\n```javascript\nconst x = 1;\nconst y = 2;\n```\n\nSome outro.");
        let code = out
            .iter()
            .find(|c| c.text.contains("const x = 1"))
            .expect("code chunk");
        assert!(code.text.contains("```"));
    }

    #[test]
    fn code_chunk_type_detected() {
        let out = SmartChunker::new(SmartChunkingOptions::default()).chunk("```python\nprint('hi')\n```");
        assert_eq!(out[0].metadata.chunk_type, "code");
    }

    #[test]
    fn table_chunk_type_detected() {
        let out =
            SmartChunker::new(SmartChunkingOptions::default()).chunk("| Col A | Col B |\n| --- | --- |\n| 1 | 2 |");
        let table = out
            .iter()
            .find(|c| c.metadata.chunk_type == "table")
            .expect("table chunk");
        assert_eq!(table.metadata.chunk_type, "table");
    }

    #[test]
    fn list_chunk_type_detected() {
        let out = SmartChunker::new(SmartChunkingOptions::default())
            .chunk("- Item one\n- Item two\n- Item three");
        assert_eq!(out[0].metadata.chunk_type, "list");
    }

    #[test]
    fn multiple_chunks_when_exceeding_max() {
        let paragraphs = std::iter::repeat("Short sentence here.").take(20).collect::<Vec<_>>().join("\n\n");
        let out = SmartChunker::new(SmartChunkingOptions {
            max_chunk_size: Some(200),
            overlap_size: Some(0),
            ..Default::default()
        })
        .chunk(&paragraphs);
        assert!(out.len() > 1);
    }

    #[test]
    fn wall_of_text_falls_back_to_sentence_splitting() {
        let text = std::iter::repeat("This is a sentence. Another sentence here.").take(50).collect::<Vec<_>>().join(" ");
        let out = SmartChunker::new(SmartChunkingOptions {
            max_chunk_size: Some(200),
            overlap_size: Some(0),
            ..Default::default()
        })
        .chunk(&text);
        assert!(out.len() > 1);
    }

    #[test]
    fn smart_chunk_document_emits_storage_shape() {
        let out = smart_chunk_document(
            "# Title\n\nSome content here.",
            "Test Doc",
            "general",
            None,
            SmartChunkingOptions::default(),
        );
        assert!(!out.is_empty());
        assert_eq!(out[0].metadata.document_title, "Test Doc");
        assert_eq!(out[0].metadata.category, "general");
        assert!(!out[0].content.is_empty());
    }

    #[test]
    fn smart_chunk_document_filters_empty() {
        let out = smart_chunk_document(
            "Some content.",
            "Doc",
            "cat",
            None,
            SmartChunkingOptions::default(),
        );
        for c in &out {
            assert!(!c.content.is_empty());
        }
    }

    #[test]
    fn smart_chunk_document_passes_subcategory() {
        let out = smart_chunk_document(
            "Content.",
            "Doc",
            "cat",
            Some("subcat"),
            SmartChunkingOptions::default(),
        );
        assert_eq!(out[0].metadata.subcategory.as_deref(), Some("subcat"));
    }

    // ─── prepareChunkForEmbedding ────────────────────────────────────────

    fn mk_chunk() -> Chunk {
        Chunk {
            content: "Revenue was $94.9B for Q4.".to_string(),
            metadata: ChunkMetadata {
                document_title: "Apple 10-K".to_string(),
                category: "financial".to_string(),
                section: Some("Income Statement".to_string()),
                chunk_index: 3,
                ..Default::default()
            },
        }
    }

    #[test]
    fn prepare_chunk_includes_contextual_prefix() {
        let mut c = mk_chunk();
        c.metadata.contextual_prefix = Some("This chunk continues the Q4 2024 earnings summary.".to_string());
        let out = prepare_chunk_for_embedding(&c);
        assert!(out.contains("This chunk continues the Q4 2024 earnings summary."));
        assert!(out.find("This chunk continues").unwrap() < out.find("Revenue was $94.9B").unwrap());
    }

    #[test]
    fn prepare_chunk_omits_context_when_missing() {
        let c = mk_chunk();
        let out = prepare_chunk_for_embedding(&c);
        assert!(!out.contains("\nContext:"));
    }

    // ─── utf16 length sanity ────────────────────────────────────────────

    #[test]
    fn utf16_len_counts_ascii_as_units() {
        assert_eq!(utf16_len("hello"), 5);
    }

    #[test]
    fn utf16_len_counts_cjk_as_units() {
        // Two CJK code points → 2 UTF-16 units.
        assert_eq!(utf16_len("中文"), 2);
    }

    #[test]
    fn utf16_len_counts_emoji_surrogate_pair_as_two_units() {
        // One astral code point → 2 UTF-16 units.
        assert_eq!(utf16_len("🦀"), 2);
    }
}