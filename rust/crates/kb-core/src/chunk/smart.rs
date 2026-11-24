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
    /// Character offset of the first *new* char of this chunk in the
    /// chunker's normalised source. The overlap prefix (if any) sits just
    /// before this offset, so `[char_start, char_end)` is exactly
    /// `text.chars().skip(overlap_prefix_len)`.
    #[serde(default)]
    pub char_start: u32,
    /// Character offset just past the last new char of this chunk.
    /// `char_end - char_start` is the number of non-overlap chars emitted.
    #[serde(default)]
    pub char_end: u32,
    /// How many leading chars of `text` repeat the tail of the previous
    /// chunk. Zero for the first chunk and for chunks that follow a hard
    /// boundary (heading, section, code, table).
    #[serde(default)]
    pub overlap_prefix_len: u32,
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
        // Offsets are in the chunker's *normalised* source: the concatenation
        // of the non-overlap bodies it emits. Block splitting trims each block,
        // prose blocks are joined with "\n\n", tables repeat their header and
        // the wall-of-text fallback rewrites separators, so these offsets only
        // equal raw-markdown offsets when that normalisation is the identity.
        let mut emitted_chars: u32 = 0;
        // The overlap prepended to `current_chunk` by the last size flush.
        // Tracked so the next chunk knows how much of its text repeats the
        // previous one; `None` means a hard boundary reset it.
        let mut pending_overlap: Option<String> = None;

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
            char_start: 0,
            char_end: 0,
            overlap_prefix_len: 0,
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

            // Atomic code: each becomes its own chunk; never merged with
            // surrounding prose. Retrieval then treats these specially.
            if structure.chunk_type == "code" {
                if utf16_len(current_chunk.trim()) > 0 {
                    Self::push_chunk(
                        &mut chunks,
                        &mut chunk_index,
                        &mut emitted_chars,
                        &current_metadata,
                        current_chunk.trim().to_string(),
                        pending_overlap.as_deref(),
                    );
                    current_chunk.clear();
                }
                pending_overlap = None;
                let meta = SmartChunkMetadata {
                    hierarchy_path: structure
                        .hierarchy_path
                        .clone()
                        .or_else(|| Some(current_hierarchy.clone())),
                    ..structure.clone()
                };
                Self::push_chunk(
                    &mut chunks,
                    &mut chunk_index,
                    &mut emitted_chars,
                    &meta,
                    block.trim().to_string(),
                    None,
                );
                continue;
            }

            // A table is its own chunk too, but a table larger than the chunk
            // budget is split at row boundaries. Every piece repeats the header
            // row, so a retrieved piece is readable on its own (`TBL-1`).
            if structure.chunk_type == "table" {
                if utf16_len(current_chunk.trim()) > 0 {
                    Self::push_chunk(
                        &mut chunks,
                        &mut chunk_index,
                        &mut emitted_chars,
                        &current_metadata,
                        current_chunk.trim().to_string(),
                        pending_overlap.as_deref(),
                    );
                    current_chunk.clear();
                }
                pending_overlap = None;
                let meta = SmartChunkMetadata {
                    hierarchy_path: structure
                        .hierarchy_path
                        .clone()
                        .or_else(|| Some(current_hierarchy.clone())),
                    ..structure.clone()
                };
                for piece in Self::split_table(block, self.opts.max_chunk_size) {
                    // A split table repeats its header in every piece. That
                    // repeated header is text the chunker inserts rather than
                    // text from the markdown; it counts as part of the
                    // normalised source, and there is no overlap to skip.
                    Self::push_chunk(
                        &mut chunks,
                        &mut chunk_index,
                        &mut emitted_chars,
                        &meta,
                        piece,
                        None,
                    );
                }
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
                Self::push_chunk(
                    &mut chunks,
                    &mut chunk_index,
                    &mut emitted_chars,
                    &current_metadata,
                    current_chunk.trim().to_string(),
                    pending_overlap.as_deref(),
                );
                current_chunk.clear();
                pending_overlap = None;
            }

            // Size flush + overlap.
            if utf16_len(&current_chunk) + utf16_len(block) > self.opts.max_chunk_size as usize
                && utf16_len(&current_chunk) > 0
            {
                Self::push_chunk(
                    &mut chunks,
                    &mut chunk_index,
                    &mut emitted_chars,
                    &current_metadata,
                    current_chunk.trim().to_string(),
                    pending_overlap.as_deref(),
                );
                let overlap = Self::get_overlap(&current_chunk, self.opts.overlap_size);
                current_chunk = overlap.clone();
                // Everything from here until the next hard boundary is a
                // repeat of the tail of the chunk just emitted.
                pending_overlap = Some(overlap);
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
            Self::push_chunk(
                &mut chunks,
                &mut chunk_index,
                &mut emitted_chars,
                &current_metadata,
                current_chunk.trim().to_string(),
                pending_overlap.as_deref(),
            );
        }

        chunks
    }

    /// Emit one chunk with its span in the normalised source. `overlap` is the
    /// exact prefix that `get_overlap` put at the start of `current_chunk`; it
    /// is used to measure how much of `text` repeats the previous chunk.
    fn push_chunk(
        chunks: &mut Vec<SmartChunk>,
        chunk_index: &mut u32,
        emitted_chars: &mut u32,
        meta: &SmartChunkMetadata,
        text: String,
        overlap: Option<&str>,
    ) {
        if text.is_empty() {
            return;
        }
        let prefix_len = Self::overlap_prefix_len(&text, overlap);
        let text_chars = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        // `char_end - char_start` is the number of non-overlap chars, exactly
        // what a future `text.chars().skip(overlap_prefix_len)` will emit.
        let body_len = text_chars.saturating_sub(prefix_len);
        chunks.push(SmartChunk {
            chunk_index: *chunk_index,
            metadata: SmartChunkMetadata {
                char_start: *emitted_chars,
                char_end: emitted_chars.saturating_add(body_len),
                overlap_prefix_len: prefix_len,
                ..meta.clone()
            },
            text,
        });
        *chunk_index = chunk_index.saturating_add(1);
        *emitted_chars = emitted_chars.saturating_add(body_len);
    }

    /// Number of leading chars of `text` that are the `overlap` prefix.
    /// `trim()` on flush can strip the overlap's leading or trailing
    /// whitespace, so compare against the trimmed variants too.
    fn overlap_prefix_len(text: &str, overlap: Option<&str>) -> u32 {
        let Some(overlap) = overlap else {
            return 0;
        };
        let count = |s: &str| u32::try_from(s.chars().count()).unwrap_or(u32::MAX);
        if text.starts_with(overlap) {
            return count(overlap);
        }
        let trimmed = overlap.trim();
        if text.starts_with(trimmed) {
            return count(trimmed);
        }
        let trimmed_start = overlap.trim_start();
        if text.starts_with(trimmed_start) {
            return count(trimmed_start);
        }
        // When the overlap budget covers the whole preceding chunk, the
        // emitted text can be nothing but overlap: no new chars at all.
        if trimmed.starts_with(text) {
            return count(text);
        }
        0
    }

    fn detect_structure(block: &str) -> SmartChunkMetadata {
        let mut meta = SmartChunkMetadata {
            chunk_type: "text".to_string(),
            document_section: None,
            heading_level: None,
            hierarchy_path: None,
            page_number: None,
            section: None,
            char_start: 0,
            char_end: 0,
            overlap_prefix_len: 0,
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
        // The heading's *text*, which is capture group 2. Group 1 is the `#`
        // run; reading it and then stripping the hashes left every heading
        // name empty, so the hierarchy path was " > " for every chunk under a
        // heading — and the embedding text inherited that.
        let clean = HEADING_LINE.replace_all(heading.trim(), "$2").to_string();
        let clean = clean.trim();
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

    /// Split a markdown table at row boundaries, repeating the header (and its
    /// separator) in every piece. A table that fits the budget stays one piece.
    fn split_table(block: &str, max_size: u32) -> Vec<String> {
        let trimmed = block.trim();
        if utf16_len(trimmed) <= max_size as usize {
            return vec![trimmed.to_string()];
        }
        let lines: Vec<&str> = trimmed.lines().collect();
        let separator = lines
            .iter()
            .position(|line| Self::is_table_separator(line))
            .unwrap_or(0);
        let header_block = lines[..=separator].join("\n");
        let rows = &lines[separator + 1..];
        if rows.is_empty() {
            return vec![header_block];
        }

        let mut pieces: Vec<String> = Vec::new();
        let mut current = header_block.clone();
        let mut rows_in_piece = 0usize;
        for row in rows {
            let candidate = utf16_len(&current) + 1 + utf16_len(row);
            if rows_in_piece > 0 && candidate > max_size as usize {
                pieces.push(current);
                current = header_block.clone();
                rows_in_piece = 0;
            }
            current.push('\n');
            current.push_str(row);
            rows_in_piece += 1;
        }
        pieces.push(current);
        pieces
    }

    /// True for the `| --- | --- |` line under a markdown table header.
    fn is_table_separator(line: &str) -> bool {
        let mut cells = 0usize;
        for cell in line.trim().trim_matches('|').split('|') {
            let cell = cell.trim();
            if cell.is_empty() {
                continue;
            }
            if !cell.chars().all(|ch| ch == '-' || ch == ':') || !cell.contains('-') {
                return false;
            }
            cells += 1;
        }
        cells > 0
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn options() -> SmartChunkingOptions {
        // Deliberately small so the overlap path really runs in the tests.
        SmartChunkingOptions {
            max_chunk_size: Some(64),
            overlap_size: Some(16),
            ..SmartChunkingOptions::default()
        }
    }

    fn fixtures() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "plain paragraphs longer than the chunk size",
                "Alpha paragraph repeats its message so that it is longer than \
                 sixty-four characters and must be split into several pieces. \
                 Beta paragraph also repeats its own message until it is longer \
                 than the configured maximum chunk size. Gamma paragraph closes \
                 this document with yet more prose to chunk.",
            ),
            (
                "headings",
                "# Laporan Keuangan\n\nPendahuluan laporan ini berisi penjelasan \
                 panjang tentang pendapatan dan pengeluaran perusahaan selama \
                 tahun yang lalu, supaya pengujian melihat pemotongan.\n\nHasil \
                 akhir dari laporan ini dirangkum kembali dalam beberapa \
                 paragraf tambahan yang sengaja dibuat panjang.",
            ),
            (
                "nested headings",
                "# Bab I\n\n## Pasal 1\n\nSetiap orang yang membaca bab ini akan \
                 menemukan penjelasan panjang mengenai ketentuan yang berlaku \
                 di dalam pasal ini dan pasal lainnya.\n\n## Pasal 2\n\nKetentuan \
                 berikutnya dijelaskan di sini dengan kalimat-kalimat yang cukup \
                 panjang untuk memicu pemotongan dan overlap.",
            ),
            (
                "markdown table",
                "| No | Nama | Nilai |\n| --- | --- | --- |\n\
                 | 1 | Alpha | 10 |\n| 2 | Beta | 20 |\n| 3 | Gamma | 30 |\n\
                 | 4 | Delta | 40 |\n| 5 | Epsilon | 50 |\n| 6 | Zeta | 60 |\n\
                 | 7 | Eta | 70 |\n\nPenjelasan setelah tabel ini sengaja \
                 dibuat cukup panjang untuk menghasilkan lebih dari satu chunk.",
            ),
            (
                "fenced code block",
                "```rust\nlet mut total = 0;\nfor value in [10, 20, 30] {\n    \
                 total += value;\n}\nprintln!(\"total = {total}\");\n```\n\nTeks \
                 setelah blok kode ini juga panjang agar chunker memotongnya \
                 menjadi beberapa bagian dengan overlap yang terukur.",
            ),
            (
                "Indonesian law text",
                "BAB I\nKETENTUAN UMUM\n\nPasal 1\nDalam peraturan ini yang \
                 dimaksud dengan pelapor adalah orang atau badan yang menyampaikan \
                 laporan berkala kepada otoritas sesuai ketentuan peraturan ini.\n\n\
                 Pasal 2\nSetiap pelapor wajib memastikan bahwa seluruh data \
                 yang disampaikan benar, lengkap, dan dapat dipertanggungjawabkan \
                 sesuai dengan ketentuan yang berlaku.",
            ),
            (
                "multibyte characters",
                "Café naïve über Größen — émoji 🚀✨ dan 日本語のテキスト.\n\n\
                 Ünicode characters count as one char each, even though they \
                 occupy several bytes or two UTF-16 units; café, naïve, and \
                 over Größen must all keep their exact positions here.\n\n🚀 \
                 Last paragraph with more emojis 😀🎉 to exercise offsets.",
            ),
            ("document shorter than one chunk", "Dokumen pendek."),
            (
                "wall of text split by sentences",
                "First sentence is reasonably long. Second sentence adds more \
                 detail to the wall of text. Third sentence keeps going with \
                 additional clarification. Fourth sentence is here as well. \
                 Fifth sentence finally closes this oversized single block.",
            ),
        ]
    }

    /// Reference text for the invariant: the chunker's **own normalised
    /// source**, not the raw markdown. It mirrors the chunker's block pipeline
    /// (block split + trim, `"\n\n"` between prose blocks, hard flushes at
    /// headings/sections/code/tables, repeated table headers and the
    /// wall-of-text sentence split) but omits overlap. Exceptions to using the
    /// raw input as reference: the chunker trims each block and the assembled
    /// chunk, drops blank-line separators, inserts `"\n\n"` between prose
    /// blocks, repeats a table header in every split piece, and rewrites
    /// separators in the wall-of-text fallback. Offsets likewise address this
    /// normalised source, which equals the received markdown only when that
    /// normalisation is the identity.
    fn normalised_source(markdown: &str, opts: ResolvedOptions) -> String {
        let mut blocks = SmartChunker::split_into_blocks(markdown, opts.preserve_code_blocks);
        let total = utf16_len(markdown);
        let avg = if blocks.is_empty() {
            total
        } else {
            total / blocks.len()
        };
        if avg > (opts.max_chunk_size * 2) as usize && total > opts.max_chunk_size as usize {
            blocks = blocks
                .into_iter()
                .flat_map(|b| {
                    let t = SmartChunker::detect_structure(&b).chunk_type;
                    if t == "table" || t == "code" {
                        vec![b]
                    } else if utf16_len(&b) > (opts.max_chunk_size * 2) as usize {
                        SmartChunker::split_by_sentences(&b, opts.max_chunk_size)
                    } else {
                        vec![b]
                    }
                })
                .collect();
        }

        let mut out = String::new();
        let mut current = String::new();
        let mut current_section: Option<String> = None;
        let mut pending_overlap: Option<String> = None;
        // The overlap at the start of `current` repeats the tail of the chunk
        // just emitted; skip it, exactly as the reader skips it in a real
        // chunk, or the reference would double it.
        let emit = |out: &mut String, text: &str, overlap: Option<&str>| {
            let prefix = SmartChunker::overlap_prefix_len(text, overlap);
            out.extend(text.chars().skip(prefix as usize));
        };
        for block in &blocks {
            let structure = SmartChunker::detect_structure(block);
            if structure.chunk_type == "code" || structure.chunk_type == "table" {
                if !current.trim().is_empty() {
                    emit(&mut out, current.trim(), pending_overlap.as_deref());
                    current.clear();
                }
                pending_overlap = None;
                if structure.chunk_type == "table" {
                    for piece in SmartChunker::split_table(block, opts.max_chunk_size) {
                        out.push_str(&piece);
                    }
                } else {
                    out.push_str(block.trim());
                }
                continue;
            }
            let should_flush = (opts.respect_heading_boundaries
                && structure.chunk_type == "heading"
                && utf16_len(&current) > 0)
                || (opts.respect_section_boundaries
                    && structure.section.is_some()
                    && structure.section != current_section
                    && utf16_len(&current) > 0);
            if should_flush {
                emit(&mut out, current.trim(), pending_overlap.as_deref());
                current.clear();
                pending_overlap = None;
            }
            if utf16_len(&current) + utf16_len(block) > opts.max_chunk_size as usize
                && utf16_len(&current) > 0
            {
                emit(&mut out, current.trim(), pending_overlap.as_deref());
                let overlap = SmartChunker::get_overlap(&current, opts.overlap_size);
                current = overlap.clone();
                pending_overlap = Some(overlap);
            }
            if current.is_empty() {
                current.push_str(block);
            } else {
                current.push_str("\n\n");
                current.push_str(block);
            }
            current_section = structure.section;
        }
        if !current.trim().is_empty() {
            emit(&mut out, current.trim(), pending_overlap.as_deref());
        }
        out
    }

    #[test]
    fn spans_reconstruct_the_normalised_source_without_duplicated_overlap() {
        let opts = options();
        let resolved = ResolvedOptions::from_options(opts);
        let mut saw_overlap = false;

        for (name, markdown) in fixtures() {
            let chunks = smart_chunk(markdown, opts);
            assert!(!chunks.is_empty(), "{name} produced no chunks");

            let reconstructed: String = chunks
                .iter()
                .flat_map(|c| c.text.chars().skip(c.metadata.overlap_prefix_len as usize))
                .collect();
            let expected = normalised_source(markdown, resolved);
            assert_eq!(reconstructed, expected, "fixture: {name}");

            let mut cursor = 0usize;
            for (i, chunk) in chunks.iter().enumerate() {
                assert_eq!(chunk.chunk_index, i as u32, "fixture: {name}");
                let prefix = chunk.metadata.overlap_prefix_len as usize;
                let body: String = chunk.text.chars().skip(prefix).collect();
                assert_eq!(
                    chunk.metadata.char_start as usize, cursor,
                    "fixture: {name}, chunk {i}: spans must be contiguous"
                );
                assert_eq!(
                    chunk.metadata.char_end - chunk.metadata.char_start,
                    u32::try_from(body.chars().count()).unwrap_or(u32::MAX),
                    "fixture: {name}, chunk {i}: span must cover non-overlap chars"
                );
                cursor = chunk.metadata.char_end as usize;
                if prefix > 0 {
                    saw_overlap = true;
                    let repeated: String = chunk.text.chars().take(prefix).collect();
                    assert!(
                        chunks[i - 1].text.ends_with(&repeated),
                        "fixture: {name}, chunk {i}: overlap must repeat the previous tail"
                    );
                }
            }
            assert_eq!(
                cursor,
                expected.chars().count(),
                "fixture: {name}: spans must cover the whole normalised source"
            );
        }
        assert!(saw_overlap, "fixture options must actually produce overlap");
    }

    #[test]
    fn spans_count_characters_not_bytes_or_utf16_units() {
        let markdown = "Café ü 🚀 — petite.";
        let chunks = smart_chunk(markdown, options());
        assert_eq!(chunks.len(), 1);
        let meta = &chunks[0].metadata;
        assert_eq!(meta.overlap_prefix_len, 0);
        assert_eq!(meta.char_start, 0);
        assert_eq!(
            meta.char_end,
            u32::try_from(markdown.chars().count()).unwrap_or(u32::MAX)
        );
        assert!(meta.char_end as usize <= markdown.len());
        assert!(meta.char_end as usize <= utf16_len(markdown));
    }

    #[test]
    fn a_short_document_has_no_overlap() {
        let chunks = smart_chunk("Dokumen pendek.", options());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].metadata.overlap_prefix_len, 0);
        assert_eq!(chunks[0].metadata.char_start, 0);
        assert_eq!(chunks[0].metadata.char_end, 15);
    }
}
