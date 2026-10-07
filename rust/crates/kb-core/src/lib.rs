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
    ChunkOptions, ChunkingStrategy, SmartChunk, SmartChunkMetadata, SmartChunkingOptions,
    chunk_document, prepare_chunk_for_embedding, smart_chunk, smart_chunk_document,
};
pub use context::{NumberedSource, SourceDedupKey, numbered_source_list, source_dedupe_key};
pub use fusion::{
    IdRef, RrfItem, RrfOptions, RrfResult, reciprocal_rank_fusion, reciprocal_rank_fusion_with,
};
pub use page_map::{PageBlock, assign_chunk_pages};
pub use policy::{
    FigureMode, IngestPolicy, MimeRegistry, parse_figure_mode, resolve_ingest_policy,
};
pub use signals::{RouterOpts, has_columnar_lines, has_dense_currency, is_unpdf_sufficient};
