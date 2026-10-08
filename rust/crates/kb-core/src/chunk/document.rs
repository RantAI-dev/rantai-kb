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
                chunk_index: sc.chunk_index,
                chunk_type: Some(sc.metadata.chunk_type),
                ..Default::default()
            },
        })
        .filter(|c| !c.content.is_empty())
        .collect()
}
