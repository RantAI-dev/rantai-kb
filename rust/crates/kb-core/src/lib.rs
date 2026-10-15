//! Pure domain logic — chunking, fusion, neighbour weave, figure policy,
//! text-layer signals, page map, ingest policy, MIME registry. No I/O, no
//! async, no environment reads.

#![allow(clippy::module_name_repetitions)]

pub mod chunk;
mod context;
mod fusion;
pub mod page_map;
pub mod policy;
mod progress;
pub mod signals;
mod weave;

pub use chunk::{
    Chunk, ChunkMetadata, ChunkOptions, ChunkingStrategy, SmartChunk, SmartChunkMetadata,
    SmartChunkingOptions, chunk_document, prepare_chunk_for_embedding, smart_chunk,
    smart_chunk_document,
};
pub use context::{
    ContextItem, NumberedSource, SourceDedupKey, SourceEntry, build_context_and_sources,
    numbered_source_list, source_dedupe_key,
};
pub use fusion::{
    IdRef, RrfItem, RrfOptions, RrfResult, reciprocal_rank_fusion, reciprocal_rank_fusion_with,
};
pub use page_map::{PageBlock, assign_chunk_pages};
pub use policy::{
    FigureMode, IngestPolicy, MimeRegistry, parse_figure_mode, resolve_ingest_policy,
};
pub use progress::{
    IngestStep, ProgressFlags, StepProgress, compute_eta_seconds, compute_overall_progress,
};
pub use signals::{RouterOpts, has_columnar_lines, has_dense_currency, is_unpdf_sufficient};
pub use weave::{WeaveItem, weave_neighbours, weave_neighbours_with};
