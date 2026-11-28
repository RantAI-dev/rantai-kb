//! Smart document chunker and compatibility layers.

mod contextual;
mod document;
mod smart;
mod utf16;

#[cfg(test)]
mod tests;

pub use contextual::{
    CONTEXT_PREFIX_MAX_CHARS, assign_contextual_prefixes, contextual_prefix, fallback_prefix,
    nodes_for_page,
};
pub use document::{
    Chunk, ChunkMetadata, ChunkOptions, chunk_document, content_is_pages,
    prepare_chunk_for_embedding, smart_chunk_document, smart_chunk_pages,
};
pub use smart::{
    ChunkingStrategy, SmartChunk, SmartChunkMetadata, SmartChunker, SmartChunkingOptions,
    smart_chunk,
};
pub use utf16::utf16_len;
