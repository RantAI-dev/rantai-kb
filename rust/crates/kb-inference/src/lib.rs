//! Embedding and rerank HTTP clients. One shared `reqwest::Client`, a timeout
//! on every call, structured tracing on every port call.

#![allow(clippy::module_name_repetitions)]

mod embedder;
mod query_expansion;
mod rerankers;

pub use embedder::{EmbedderImpl, EmbedderSettings};
pub use query_expansion::{expand_query, ExpansionSettings};
pub use rerankers::{
    CohereReranker, Provider, Rerankers, TeiReranker, VllmReranker,
};