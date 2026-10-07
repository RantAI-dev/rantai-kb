//! SurrealDB HTTP `/sql` adapter for chunks (vector, BM25, neighbours, figures).

#![allow(clippy::module_name_repetitions)]

mod client;
mod figures;
mod neighbours;
mod queries;
mod store;
mod vector;
mod bm25;

pub use store::SurrealChunkStore;