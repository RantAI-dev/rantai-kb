//! Retrieval orchestration through ports. Never names an adapter.

#![allow(clippy::module_name_repetitions)]

mod fakes;
mod retrieval;

pub use fakes::{FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker};
pub use retrieval::{retrieve, RetrieveRequest, RetrievalResult};