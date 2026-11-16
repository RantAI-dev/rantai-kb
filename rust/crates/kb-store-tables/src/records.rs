//! The record half of the table store: one row per (table, version) in
//! Postgres, table `KbTable` (additive migration `20261114120000_kb_tables`).
//!
//! Every read is filtered by `"tenantId"`, and a document's current tables are
//! the rows at that document's highest `version`. Two versions are kept (the
//! version a query may still be reading, and the one it replaced); the store
//! deletes older rows and their grid blobs together.

use async_trait::async_trait;
use kb_core::tables::{TableManifest, TableSource};
use kb_ports::tables::{TableId, TableRecord};
use kb_ports::{DocumentId, PortError, TenantId};
use sqlx::{PgPool, Row};

/// Persistence for table records. Split from [`crate::TablesStore`] so the
/// query and versioning logic can be tested without Postgres.
#[async_trait]
pub trait TableRecords: Send + Sync {
    /// Insert rows. Every row carries its version; a version bump inserts a
    /// whole new set.
    async fn insert_rows(&self, rows: &[TableRecord]) -> Result<(), PortError>;

    /// The current tables of one document, or of the whole tenant (latest
    /// version per document), ordered by document then index.
    async fn current_tables(
        &self,
        tenant: &TenantId,
        document: Option<&DocumentId>,
    ) -> Result<Vec<TableRecord>, PortError>;

    /// One current table, when it belongs to `tenant`.
    async fn current_table(
        &self,
        tenant: &TenantId,
        id: &TableId,
    ) -> Result<Option<TableRecord>, PortError>;

    /// The document's current version, `0` when it has no table rows.
    async fn latest_version(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
    ) -> Result<u32, PortError>;

    /// Delete rows older than `keep_from`, returning the grid keys that went
    /// with them so the caller can remove the blobs.
    async fn delete_older_than(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
        keep_from: u32,
    ) -> Result<Vec<String>, PortError>;

    /// Delete every version of a document's tables, returning its grid keys.
    async fn delete_all(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
    ) -> Result<Vec<String>, PortError>;
}

/// `KbTable` on Postgres.
#[derive(Clone)]
pub struct PgTableRecords {
    pool: PgPool,
}

impl PgTableRecords {
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }
}

pub(crate) const CURRENT_FOR_DOCUMENT: &str = r#"
    SELECT
        t.id AS id,
        t."tenantId" AS tenant_id,
        t."documentId" AS document_id,
        t.idx AS idx,
        t.version AS version,
        t.source AS source,
        t.sheet AS sheet,
        t."pageSpan" AS page_span,
        t.title AS title,
        t.caption AS caption,
        t.header AS header,
        t.manifest AS manifest,
        t."gridKey" AS grid_key,
        to_char(t."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at
      FROM "KbTable" t
     WHERE t."tenantId" = $1
       AND t."documentId" = $2
       AND t.version = (
           SELECT max(t2.version) FROM "KbTable" t2
            WHERE t2."tenantId" = $1 AND t2."documentId" = $2
       )
     ORDER BY t.idx
"#;

pub(crate) const CURRENT_FOR_TENANT: &str = r#"
    SELECT
        t.id AS id,
        t."tenantId" AS tenant_id,
        t."documentId" AS document_id,
        t.idx AS idx,
        t.version AS version,
        t.source AS source,
        t.sheet AS sheet,
        t."pageSpan" AS page_span,
        t.title AS title,
        t.caption AS caption,
        t.header AS header,
        t.manifest AS manifest,
        t."gridKey" AS grid_key,
        to_char(t."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at
      FROM "KbTable" t
     WHERE t."tenantId" = $1
       AND t.version = (
           SELECT max(t2.version) FROM "KbTable" t2
            WHERE t2."tenantId" = t."tenantId" AND t2."documentId" = t."documentId"
       )
     ORDER BY t."documentId", t.idx
"#;

pub(crate) const CURRENT_ONE: &str = r#"
    SELECT
        t.id AS id,
        t."tenantId" AS tenant_id,
        t."documentId" AS document_id,
        t.idx AS idx,
        t.version AS version,
        t.source AS source,
        t.sheet AS sheet,
        t."pageSpan" AS page_span,
        t.title AS title,
        t.caption AS caption,
        t.header AS header,
        t.manifest AS manifest,
        t."gridKey" AS grid_key,
        to_char(t."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at
      FROM "KbTable" t
     WHERE t."tenantId" = $1
       AND t.id = $2
       AND t.version = (
           SELECT max(t2.version) FROM "KbTable" t2
            WHERE t2."tenantId" = $1 AND t2.id = $2
       )
"#;

pub(crate) const INSERT_ROW: &str = r#"
    INSERT INTO "KbTable" (
        id, "tenantId", "documentId", idx, version, source, sheet,
        "pageSpan", title, caption, header, manifest, "gridKey", "createdAt"
    ) VALUES (
        $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, now()
    )
"#;

pub(crate) const LATEST_VERSION: &str = r#"
    SELECT COALESCE(max(version), 0) AS version
      FROM "KbTable"
     WHERE "tenantId" = $1 AND "documentId" = $2
"#;

pub(crate) const DELETE_OLDER: &str = r#"
    DELETE FROM "KbTable"
     WHERE "tenantId" = $1 AND "documentId" = $2 AND version <= $3
    RETURNING "gridKey"
"#;

pub(crate) const DELETE_ALL_FOR_DOCUMENT: &str = r#"
    DELETE FROM "KbTable"
     WHERE "tenantId" = $1 AND "documentId" = $2
    RETURNING "gridKey"
"#;

#[async_trait]
impl TableRecords for PgTableRecords {
    async fn insert_rows(&self, rows: &[TableRecord]) -> Result<(), PortError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg begin: {e}")))?;
        for row in rows {
            let page_span = row
                .page_span
                .map(|(first, last)| serde_json::json!([first, last]));
            let manifest = serde_json::to_value(&row.manifest)
                .map_err(|e| PortError::Storage(format!("manifest json: {e}")))?;
            sqlx::query(INSERT_ROW)
                .bind(row.id.as_str())
                .bind(row.tenant_id.as_str())
                .bind(row.document_id.as_str())
                .bind(
                    i32::try_from(row.index)
                        .map_err(|_| PortError::Storage("table index out of range".into()))?,
                )
                .bind(
                    i32::try_from(row.version)
                        .map_err(|_| PortError::Storage("table version out of range".into()))?,
                )
                .bind(source_str(row.source))
                .bind(row.sheet.as_deref())
                .bind(page_span)
                .bind(row.title.as_deref())
                .bind(row.caption.as_deref())
                .bind(&row.header)
                .bind(manifest)
                .bind(&row.grid_key)
                .execute(&mut *tx)
                .await
                .map_err(|e| PortError::Unavailable(format!("pg insert table: {e}")))?;
        }
        tx.commit()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg commit: {e}")))
    }

    async fn current_tables(
        &self,
        tenant: &TenantId,
        document: Option<&DocumentId>,
    ) -> Result<Vec<TableRecord>, PortError> {
        let rows = match document {
            Some(document) => {
                sqlx::query(CURRENT_FOR_DOCUMENT)
                    .bind(tenant.as_str())
                    .bind(document.as_str())
                    .fetch_all(&self.pool)
                    .await
            }
            None => {
                sqlx::query(CURRENT_FOR_TENANT)
                    .bind(tenant.as_str())
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .map_err(|e| PortError::Unavailable(format!("pg select tables: {e}")))?;
        rows.iter().map(row_to_record).collect()
    }

    async fn current_table(
        &self,
        tenant: &TenantId,
        id: &TableId,
    ) -> Result<Option<TableRecord>, PortError> {
        let row = sqlx::query(CURRENT_ONE)
            .bind(tenant.as_str())
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg select table: {e}")))?;
        row.as_ref().map(row_to_record).transpose()
    }

    async fn latest_version(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
    ) -> Result<u32, PortError> {
        let row = sqlx::query(LATEST_VERSION)
            .bind(tenant.as_str())
            .bind(document.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg latest version: {e}")))?;
        // "version" is INTEGER (INT4) in the migration; decoding it as i64
        // failed every ingest against a live Postgres.
        let version: i32 = row
            .try_get("version")
            .map_err(|e| PortError::Unavailable(format!("pg version: {e}")))?;
        Ok(u32::try_from(version).unwrap_or(0))
    }

    async fn delete_older_than(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
        keep_from: u32,
    ) -> Result<Vec<String>, PortError> {
        let rows = sqlx::query(DELETE_OLDER)
            .bind(tenant.as_str())
            .bind(document.as_str())
            .bind(
                i32::try_from(keep_from)
                    .map_err(|_| PortError::Storage("table version out of range".into()))?,
            )
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg delete old tables: {e}")))?;
        Ok(rows
            .iter()
            .filter_map(|row| row.try_get::<String, _>("gridKey").ok())
            .collect())
    }

    async fn delete_all(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
    ) -> Result<Vec<String>, PortError> {
        let rows = sqlx::query(DELETE_ALL_FOR_DOCUMENT)
            .bind(tenant.as_str())
            .bind(document.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg delete tables: {e}")))?;
        Ok(rows
            .iter()
            .filter_map(|row| row.try_get::<String, _>("gridKey").ok())
            .collect())
    }
}

fn row_to_record(row: &sqlx::postgres::PgRow) -> Result<TableRecord, PortError> {
    let id: String = row
        .try_get("id")
        .map_err(|e| PortError::Unavailable(format!("pg table id: {e}")))?;
    let tenant_id: String = row
        .try_get("tenant_id")
        .map_err(|e| PortError::Unavailable(format!("pg table tenant: {e}")))?;
    let document_id: String = row
        .try_get("document_id")
        .map_err(|e| PortError::Unavailable(format!("pg table document: {e}")))?;
    // Required columns fail loudly: a defaulted decode turned a type mismatch
    // into every table being index 0, version 1.
    let index: i32 = row
        .try_get("idx")
        .map_err(|e| PortError::Unavailable(format!("pg table idx: {e}")))?;
    let version: i32 = row
        .try_get("version")
        .map_err(|e| PortError::Unavailable(format!("pg table version: {e}")))?;
    let source: String = row
        .try_get("source")
        .map_err(|e| PortError::Unavailable(format!("pg table source: {e}")))?;
    let sheet: Option<String> = row.try_get("sheet").ok().flatten();
    let page_span: Option<serde_json::Value> = row.try_get("page_span").ok().flatten();
    let title: Option<String> = row.try_get("title").ok().flatten();
    let caption: Option<String> = row.try_get("caption").ok().flatten();
    let header: Vec<String> = row
        .try_get::<Option<Vec<String>>, _>("header")
        .map_err(|e| PortError::Unavailable(format!("pg table header: {e}")))?
        .unwrap_or_default();
    let manifest: serde_json::Value = row.try_get("manifest").unwrap_or(serde_json::Value::Null);
    let manifest: TableManifest = serde_json::from_value(manifest)
        .map_err(|e| PortError::Malformed(format!("table manifest: {e}")))?;
    let grid_key: String = row
        .try_get("grid_key")
        .map_err(|e| PortError::Unavailable(format!("pg table grid key: {e}")))?;
    let created_at: String = row.try_get("created_at").unwrap_or_default();
    let page_span = page_span.and_then(|value| {
        let pair: Option<(u32, u32)> = serde_json::from_value(value).ok();
        pair
    });
    Ok(TableRecord {
        id: TableId(id),
        tenant_id: TenantId::new_unchecked(tenant_id),
        document_id: DocumentId::new_unchecked(document_id),
        index: u32::try_from(index).unwrap_or(0),
        version: u32::try_from(version).unwrap_or(1),
        source: parse_source(&source),
        sheet,
        page_span,
        title,
        caption,
        header,
        manifest,
        grid_key,
        created_at,
    })
}

/// `TableSource` as it is stored: the serde lowercase name, so a value the
/// contract adds later round-trips through this pair.
pub(crate) fn source_str(source: TableSource) -> String {
    serde_json::to_value(source)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "csv".to_string())
}

fn parse_source(source: &str) -> TableSource {
    serde_json::from_value(serde_json::Value::String(source.to_string()))
        .unwrap_or(TableSource::Csv)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn every_read_is_tenant_scoped() {
        for sql in [
            CURRENT_FOR_DOCUMENT,
            CURRENT_FOR_TENANT,
            CURRENT_ONE,
            LATEST_VERSION,
            DELETE_OLDER,
            DELETE_ALL_FOR_DOCUMENT,
        ] {
            assert!(
                sql.contains("\"tenantId\" = $1"),
                "a table read or write without the tenant is a cross-tenant leak: {sql}"
            );
        }
    }

    #[test]
    fn the_document_queries_bind_the_document_never_interpolate_it() {
        for sql in [CURRENT_FOR_DOCUMENT, LATEST_VERSION, DELETE_OLDER] {
            assert!(sql.contains("\"documentId\" = $2"), "{sql}");
            assert!(!sql.contains("doc-"), "{sql}");
        }
    }

    #[test]
    fn a_source_round_trips() {
        for source in [
            TableSource::Xlsx,
            TableSource::Xls,
            TableSource::Ods,
            TableSource::Csv,
            TableSource::Docx,
            TableSource::Markdown,
            TableSource::Pdf,
        ] {
            assert_eq!(parse_source(&source_str(source)), source);
        }
    }
}

/// Live-Postgres checks. Off by default (`cargo test --workspace` needs no
/// database), and skipped — not passed — unless `KB_TABLES_TEST_DATABASE_URL`
/// names a database with the Prisma migrations applied.
#[cfg(all(test, feature = "integration"))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod integration {
    use super::*;
    use kb_core::tables::{ColumnInfo, ColumnKind, ColumnRole, Confidence, TableManifest};

    fn manifest() -> TableManifest {
        TableManifest {
            columns: vec![ColumnInfo {
                name: "Revenue".to_string(),
                kind: ColumnKind::Number,
                role: ColumnRole::Quantity,
                unit: Some("Rp juta".to_string()),
                sample_values: Vec::new(),
                distinct_count: 1,
            }],
            derived_rows: vec![1],
            formulas_known: true,
            confidence: Confidence::High,
            n_rows: 2,
            n_cols: 1,
            truncated: false,
            notes: vec!["a note".to_string()],
        }
    }

    fn record(tenant: &str, document: &str, version: u32) -> TableRecord {
        let id = TableId::of(&DocumentId::new_unchecked(document), 0);
        TableRecord {
            id: id.clone(),
            tenant_id: TenantId::new_unchecked(tenant),
            document_id: DocumentId::new_unchecked(document),
            index: 0,
            version,
            source: TableSource::Xlsx,
            sheet: Some("Sheet1".to_string()),
            page_span: Some((2, 3)),
            title: Some("Revenue".to_string()),
            caption: None,
            header: vec!["Revenue".to_string()],
            manifest: manifest(),
            grid_key: format!("tables/{tenant}/{document}/{id}/v{version}.parquet"),
            created_at: String::new(),
        }
    }

    #[tokio::test]
    async fn pg_records_round_trip_keep_two_versions_and_stay_in_their_tenant() {
        let Ok(url) = std::env::var("KB_TABLES_TEST_DATABASE_URL") else {
            eprintln!("skipped: KB_TABLES_TEST_DATABASE_URL is not set");
            return;
        };
        let pool = sqlx::PgPool::connect(&url).await.expect("test database");
        let tenant = TenantId::new_unchecked(format!("tbl-test-{}", uuid::Uuid::new_v4()));
        let other = TenantId::new_unchecked(format!("tbl-test-other-{}", uuid::Uuid::new_v4()));
        let document = DocumentId::new_unchecked(format!("doc-{}", uuid::Uuid::new_v4()));
        // The table rows reference their document (ON DELETE CASCADE), so the
        // document has to exist first, as it does in ingest.
        sqlx::query(
            r#"INSERT INTO "Document" (id, "tenantId", title, content, "updatedAt")
               VALUES ($1, $2, 'tbl test', '', CURRENT_TIMESTAMP)"#,
        )
        .bind(document.as_str())
        .bind(tenant.as_str())
        .execute(&pool)
        .await
        .expect("test document");
        let records = PgTableRecords::from_pool(pool.clone());

        records
            .insert_rows(&[record(tenant.as_str(), document.as_str(), 1)])
            .await
            .unwrap();
        records
            .insert_rows(&[record(tenant.as_str(), document.as_str(), 2)])
            .await
            .unwrap();
        let current = records
            .current_tables(&tenant, Some(&document))
            .await
            .unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].version, 2);
        assert_eq!(current[0].page_span, Some((2, 3)));
        assert_eq!(
            current[0].manifest.columns[0].unit.as_deref(),
            Some("Rp juta")
        );

        assert!(
            records
                .current_table(&other, &current[0].id)
                .await
                .unwrap()
                .is_none(),
            "a table must never be readable under another tenant"
        );

        let removed = records
            .delete_older_than(&tenant, &document, 1)
            .await
            .unwrap();
        assert_eq!(removed.len(), 1, "the v1 grid key comes back for deletion");
        let keys = records.delete_all(&tenant, &document).await.unwrap();
        assert_eq!(keys.len(), 1);
        assert!(
            records
                .current_tables(&tenant, Some(&document))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
