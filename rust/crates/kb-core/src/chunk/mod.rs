//! Smart document chunker and compatibility layers.

mod document;
mod smart;
mod utf16;

#[cfg(test)]
mod tests;

pub use document::{
    Chunk, ChunkMetadata, ChunkOptions, chunk_document, prepare_chunk_for_embedding,
    smart_chunk_document, smart_chunk_pages,
};
pub use smart::{
    ChunkingStrategy, SmartChunk, SmartChunkMetadata, SmartChunker, SmartChunkingOptions,
    smart_chunk,
};
pub use utf16::utf16_len;
