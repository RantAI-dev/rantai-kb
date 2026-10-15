//! SurrealDB-backed `ChunkStore`.
//!
//! Exposes vector search, BM25 full-text search, neighbour retrieval and
//! figure co-retrieval over the `document_chunk` SurrealDB table.
//!
//! Query parameters like `$q` are passed via HTTP query arguments to avoid
//! string interpolation of user search text. Document ids are strictly validated
//! against `^[A-Za-z0-9_-]{1,64}$` before being rendered into query templates.
//! Numeric literals and vector arrays are sanitized and inlined.

mod bm25;
mod chunks_meta;
mod chunks_write;
mod client;
mod figures;
mod neighbours;
mod queries;
mod store;
mod vector;

pub use client::{SurrealChunkStore, SurrealSettings};
