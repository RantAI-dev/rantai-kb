//! The table store port (`TBL-1`, `docs/core/specs/tbl-1.md`).
//!
//! The data types are in [`kb_core::tables`]; this is the I/O boundary the
//! engine and the server talk to. The adapter (`kb-store-tables`) keeps each
//! table's grid as Parquet in the blob store and its record in Postgres, and
//! answers [`TableStore::query`] with DuckDB.

use async_trait::async_trait;
use kb_core::tables::{
    QueryError, QueryPolicy, QueryResult, TableGrid, TableManifest, TableQuery, TableSource,
};
use serde::{Deserialize, Serialize};

use crate::error::PortError;
use crate::ids::{DocumentId, TenantId};

/// A stored table's id: `{documentId}_t{index}` (index in document order), so
/// it is stable across re-ingest of the same document and readable in logs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TableId(pub String);

impl TableId {
    /// The id of the `index`-th table of `document`.
    #[must_use]
    pub fn of(document: &DocumentId, index: u32) -> Self {
        Self(format!("{}_t{index}", document.as_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TableId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One stored table: where it came from, what it means, where its grid is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRecord {
    pub id: TableId,
    pub tenant_id: TenantId,
    pub document_id: DocumentId,
    /// 0-based position among the document's tables.
    pub index: u32,
    /// Bumped on every re-ingest; the previous version is kept (so "what
    /// changed?" stays answerable), older ones are deleted.
    pub version: u32,
    pub source: TableSource,
    pub sheet: Option<String>,
    pub page_span: Option<(u32, u32)>,
    pub title: Option<String>,
    pub caption: Option<String>,
    pub header: Vec<String>,
    pub manifest: TableManifest,
    /// Blob-store key of the grid (Parquet).
    pub grid_key: String,
    pub created_at: String,
}

/// A query refused or failed: a caller error (400) or a store failure (5xx).
#[derive(Debug)]
pub enum TableQueryFailure {
    Invalid(QueryError),
    Store(PortError),
}

impl From<PortError> for TableQueryFailure {
    fn from(e: PortError) -> Self {
        Self::Store(e)
    }
}

#[async_trait]
pub trait TableStore: Send + Sync {
    /// Replace a document's tables with `tables` (grid + manifest, document
    /// order). The previous version is kept, older versions are deleted.
    async fn put_tables(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
        tables: Vec<(TableGrid, TableManifest)>,
    ) -> Result<Vec<TableRecord>, PortError>;

    /// Current tables, of one document or of the whole tenant.
    async fn list_tables(
        &self,
        tenant: &TenantId,
        document: Option<&DocumentId>,
    ) -> Result<Vec<TableRecord>, PortError>;

    /// One table, `None` when it does not exist **in this tenant**.
    async fn get_table(
        &self,
        tenant: &TenantId,
        id: &TableId,
    ) -> Result<Option<TableRecord>, PortError>;

    /// Run a typed query over the current version of a table in this tenant.
    async fn query(
        &self,
        tenant: &TenantId,
        id: &TableId,
        query: &TableQuery,
        policy: &QueryPolicy,
    ) -> Result<QueryResult, TableQueryFailure>;

    /// Delete every version of a document's tables (hard delete of the document).
    async fn delete_document_tables(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
    ) -> Result<(), PortError>;
}
