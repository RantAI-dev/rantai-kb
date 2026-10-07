//! Postgres / sqlx adapter for documents, knowledge bases, API keys.
//! The only place a `DocumentScope` is built.

#![allow(clippy::module_name_repetitions)]

mod auth;
mod documents;
mod queries;
mod store;

pub use auth::PgAuth;
pub use documents::PgDocumentStore;
pub use store::PgStore;