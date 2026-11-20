//! Pure domain logic — chunking, fusion, neighbour weave, figure policy,
//! text-layer signals, page map, ingest policy, MIME registry. No I/O, no
//! async, no environment reads.

#![allow(clippy::module_name_repetitions)]

pub mod boost;
pub mod chunk;
pub mod community;
mod context;
pub mod figure_assets;
pub mod figure_gate;
pub mod furniture;
mod fusion;
pub mod graph;
pub mod intelligence;
pub mod page_fusion;
pub mod page_map;
pub mod pagination;
pub mod policy;
mod progress;
pub mod query_class;
pub mod signals;
pub mod sparse;
pub mod tables;
pub mod title;
pub mod tree;
pub mod visuals;
mod weave;
pub mod xiaomi_ocr;

pub use boost::{
    AuthorityBoost, BoostConfig, MAX_METADATA_KEY_LEN, RecencyBoost, RecencyField,
    authority_factor, authority_value, base_from_rank, base_from_rerank, base_from_rerank_in,
    date_value, normalise_authority, parse_date, recency_decay, recency_factor,
    rerank_scores_are_probabilities, valid_metadata_key, weighted,
};
pub use chunk::{
    Chunk, ChunkMetadata, ChunkOptions, ChunkingStrategy, SmartChunk, SmartChunkMetadata,
    SmartChunkingOptions, chunk_document, prepare_chunk_for_embedding, smart_chunk,
    smart_chunk_document,
};
pub use community::{
    Community, DetectOptions, assign_chunks, bm25_scores, cooccurrence, detect, hub_entities,
    louvain, rank_summaries, summary_prompt,
};
pub use context::{
    ContextItem, NumberedSource, SourceDedupKey, SourceEntry, build_context_and_sources,
    numbered_source_list, source_dedupe_key,
};
pub use figure_assets::{
    build_page_text, context_for_page, derive_label, figure_kind_word, normalize_figure_type,
    resolve_figure_anchors,
};
pub use figure_gate::{GateConfig, PROMPT_STRICT, parse_verdict};
pub use fusion::{
    IdRef, RrfItem, RrfOptions, RrfResult, reciprocal_rank_fusion, reciprocal_rank_fusion_with,
};
pub use intelligence::{
    Entity, HeuristicConfig, PatternEntityExtractor, PatternExtractionConfig,
    PatternRelationConfig, PatternRelationExtractor, Relation, key_of, query_terms,
};
pub use page_map::{PageBlock, assign_chunk_pages};
pub use pagination::{
    Cursor, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, PageSize, created_at_timestamp, decode,
    format_timestamp, resolve_page_size,
};
pub use policy::{
    FigureMode, IngestPolicy, MimeRegistry, parse_figure_mode, resolve_ingest_policy,
};
pub use progress::{
    IngestStep, ProgressFlags, StepProgress, compute_eta_seconds, compute_overall_progress,
};
pub use query_class::{Arm, ArmPlan, QueryClass, QuerySignature, SkippedArm, classify_query};
pub use signals::{RouterOpts, has_columnar_lines, has_dense_currency, is_unpdf_sufficient};
pub use sparse::{
    BM25_AVGDL, BM25_B, BM25_K1, SparseVector, encode_document, encode_document_avgdl,
    encode_query, saturate_tf, term_id, tokenize,
};
pub use tree::{
    DEFAULT_MAX_PAGES, DEFAULT_NODES, DEFAULT_PAGES_PER_NODE, DEFAULT_RRF_K, DEFAULT_WEIGHT,
    Outline, OutlineMethod, OutlineNode, TreeDocument, TreeMatch, is_chapter_node, node_number,
    node_pages, numbering_level, query_numbers, score_tree, tokens as tree_tokens,
};
pub use weave::{WeaveItem, weave_neighbours, weave_neighbours_with};
