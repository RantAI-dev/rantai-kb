//! Pure domain logic — chunking, fusion, neighbour weave, figure policy,
//! text-layer signals, page map, ingest policy, MIME registry. No I/O, no
//! async, no environment reads.

#![allow(clippy::module_name_repetitions)]

mod chunk;
mod context;
mod fusion;
mod page_map;
mod policy;
mod signals;

pub use chunk::{
    chunk_document, prepare_chunk_for_embedding, smart_chunk, smart_chunk_document, ChunkOptions,
    ChunkingStrategy, SmartChunk, SmartChunkMetadata, SmartChunkingOptions,
};
pub use context::{numbered_source_list, source_dedupe_key, SourceDedupKey};
pub use fusion::{reciprocal_rank_fusion, RrfItem, RrfOptions, RrfResult};
pub use page_map::{assign_chunk_pages, PageBlock};
pub use policy::{
    parse_figure_mode, resolve_ingest_policy, FigureMode, IngestPolicy, MimeRegistry,
};
pub use signals::{has_columnar_lines, has_dense_currency, is_unpdf_sufficient, RouterOpts};