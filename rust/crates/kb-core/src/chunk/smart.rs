//! Smart chunker for markdown documents.

use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

use super::utf16::utf16_len;

static SENTENCE_BOUNDARY: LazyLock<Regex> =
    LazyLock::new(|| match Regex::new(r"(?<=[.!?])\s+(?=[A-Z0-9])") {
        Ok(r) => r,
        Err(_) => std::process::abort(),
    });

static HEADING_LINE: LazyLock<Regex> = LazyLock::new(|| match Regex::new(r"^(#{1,6})\s+(.+)") {
    Ok(r) => r,
    Err(_) => std::process::abort(),
});

static LIST_ITEM: LazyLock<Regex> = LazyLock::new(|| match Regex::new(r"^[\d*+.-]\s+") {
    Ok(r) => r,
    Err(_) => std::process::abort(),
});

/// Chunking strategy selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChunkingStrategy {
    Smart,
    Semantic,
    FixedSize,
    StructureAware,
}

/// Public options for [`smart_chunk`] and `smart_chunk_document`.
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
pub(crate) struct ResolvedOptions {
    pub max_chunk_size: u32,
    pub overlap_size: u32,
    pub preserve_code_blocks: bool,
    pub respect_heading_boundaries: bool,
    pub respect_section_boundaries: bool,
    #[allow(dead_code)]
    pub strategy: ChunkingStrategy,
}

impl ResolvedOptions {
    pub fn from_options(o: SmartChunkingOptions) -> Self {
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

/// One metadata field on a smart chunk.
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

/// Smart document chunker.
pub struct SmartChunker {
    opts: ResolvedOptions,
}

impl SmartChunker {
    /// Build with the given options. Any unset option falls back to the
    /// original defaults.
    #[must_use]
    pub fn new(options: SmartChunkingOptions) -> Self {
        Self {
            opts: ResolvedOptions::from_options(options),
        }
    }

    /// Chunk a markdown document.
    pub fn chunk(&self, markdown: &str) -> Vec<SmartChunk> {
        let mut chunks: Vec<SmartChunk> = Vec::new();
        let mut chunk_index: u32 = 0;

        let mut blocks = Self::split_into_blocks(markdown, self.opts.preserve_code_blocks);
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
                    current_hierarchy = Self::update_hierarchy(&current_hierarchy, block, level);
                    structure.hierarchy_path = Some(current_hierarchy.clone());
                }
            }

            // Atomic tables and code: each becomes its own chunk; never merged
            // with surrounding prose. Retrieval then treats these specially.
            if structure.chunk_type == "table" || structure.chunk_type == "code" {
                if utf16_len(current_chunk.trim()) > 0 {
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
        let clean = HEADING_LINE.replace_all(heading.trim(), "$1").to_string();
        let clean = clean.trim_start_matches(|c: char| c == '#' || c.is_whitespace());
        let mut out: Vec<String> = current
            .iter()
            .take((level as usize).saturating_sub(1))
            .cloned()
            .collect();
        out.push(clean.to_string());
        out
    }

    fn get_overlap(text: &str, size: u32) -> String {
        if utf16_len(text) <= size as usize {
            return text.to_string();
        }
        let start = utf16_len(text).saturating_sub(size as usize);
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
        if let Some(idx) = overlap.find(". ") {
            return overlap[idx + 2..].to_string();
        }
        overlap.to_string()
    }

    fn split_by_sentences(text: &str, target_size: u32) -> Vec<String> {
        let splits = SENTENCE_BOUNDARY.split(text);
        let sentences: Vec<&str> = splits
            .filter_map(|s| s.ok())
            .filter(|s| !s.trim().is_empty())
            .collect();
        if sentences.len() <= 1 {
            return Self::split_by_fixed_size(text, target_size);
        }
        let mut blocks: Vec<String> = Vec::new();
        let mut current = String::new();
        let target = target_size as usize;
        for sentence in sentences {
            let next_len =
                utf16_len(&current) + utf16_len(sentence) + if current.is_empty() { 0 } else { 1 };
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
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let mut start_utf16 = 0usize;
        while start_utf16 < utf16.len() {
            let mut end_utf16 = (start_utf16 + size).min(utf16.len());

            if end_utf16 < utf16.len() {
                let slice = &utf16[start_utf16..end_utf16];
                if let Some(rel) = slice.iter().rposition(|u| *u == b' ' as u16) {
                    if rel > size / 2 {
                        end_utf16 = start_utf16 + rel;
                    }
                }
            }

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
            if end_utf16 == start_utf16 {
                start_utf16 += 1;
            } else {
                start_utf16 = end_utf16;
            }
        }
        blocks
    }

    fn split_into_blocks(text: &str, preserve_code_blocks: bool) -> Vec<String> {
        let mut blocks: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut in_code_block = false;
        let mut in_table = false;

        for line in text.split('\n') {
            if line.trim_start().starts_with("```") {
                in_code_block = !in_code_block;
                current.push_str(line);
                current.push('\n');
                // Only a code block that is its own block gets flushed here.
                // With the option off, the fence's lines stay in `current` and
                // are flushed with the surrounding paragraph, exactly as
                // `if (!inCodeBlock && this.options.preserveCodeBlocks)` does
                // in the TypeScript chunker.
                if !in_code_block && preserve_code_blocks {
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
#[must_use]
pub fn smart_chunk(markdown: &str, options: SmartChunkingOptions) -> Vec<SmartChunk> {
    SmartChunker::new(options).chunk(markdown)
}
