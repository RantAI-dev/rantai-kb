//! Table store adapter (`TBL-1`, `docs/core/specs/tbl-1.md`).
//!
//! The grid lives as Parquet in the blob store, the record in Postgres
//! (`KbTable`), and a query is a typed [`kb_core::tables::TableQuery`] compiled
//! against the manifest into SQL with bound parameters and run by DuckDB over
//! a local copy of the one Parquet file.

pub mod compiler;
pub mod parquet;
mod records;
mod store;

pub use records::{PgTableRecords, TableRecords};
pub use store::{TableStoreSettings, TablesStore};
