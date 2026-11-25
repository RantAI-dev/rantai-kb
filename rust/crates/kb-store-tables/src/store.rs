//! `TableStore` over Postgres records, blob-stored Parquet and DuckDB.
//!
//! The flow of a query is deliberately boring:
//!
//! 1. read the record (tenant-scoped, current version);
//! 2. validate and compile the typed query against the manifest
//!    ([`crate::compiler`]), which is where every caller error is decided;
//! 3. bring the grid's Parquet to a local cache file (downloaded once per
//!    version key);
//! 4. run it in DuckDB with a timeout, then apply the privacy suppression;
//! 5. fill the receipt.
//!
//! A version bump uploads a complete new set of grids at `v{n}` keys and keeps
//! two versions, so a query that started on the old one never reads a rewritten
//! file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kb_core::tables::{
    QUERY_TIMEOUT_MS, QueryPolicy, QueryResult, TableGrid, TableManifest, TableQuery,
};
use kb_ports::tables::{TableId, TableQueryFailure, TableRecord, TableStore};
use kb_ports::{BlobStore, PortError, table_grid_path};
use kb_ports::{DocumentId, TenantId};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::compiler;
use crate::parquet::{self, RunOutput};
use crate::records::{PgTableRecords, TableRecords};

/// Adapter settings.
#[derive(Debug, Clone)]
pub struct TableStoreSettings {
    /// Where downloaded grids are cached, keyed by blob key. `KB_CACHE_DIR`
    /// when the process has one.
    pub cache_dir: PathBuf,
    /// Wall-clock budget for one query (`QUERY_TIMEOUT_MS` unless set).
    pub query_timeout_ms: u64,
}

impl Default for TableStoreSettings {
    fn default() -> Self {
        Self {
            cache_dir: std::env::temp_dir().join("rantai-kb-tables"),
            query_timeout_ms: QUERY_TIMEOUT_MS,
        }
    }
}

/// The table store.
#[derive(Clone)]
pub struct TablesStore {
    records: Arc<dyn TableRecords>,
    blobs: Arc<dyn BlobStore>,
    settings: TableStoreSettings,
}

impl TablesStore {
    #[must_use]
    pub fn new(
        records: Arc<dyn TableRecords>,
        blobs: Arc<dyn BlobStore>,
        settings: TableStoreSettings,
    ) -> Self {
        Self {
            records,
            blobs,
            settings,
        }
    }

    /// A store on the process's existing Postgres pool.
    #[must_use]
    pub fn from_pool(
        pool: PgPool,
        blobs: Arc<dyn BlobStore>,
        settings: TableStoreSettings,
    ) -> Self {
        Self::new(Arc::new(PgTableRecords::from_pool(pool)), blobs, settings)
    }

    /// The local file for one grid, downloading it once.
    async fn local_grid(&self, key: &str) -> Result<PathBuf, PortError> {
        let path = cache_file(&self.settings.cache_dir, key);
        if path.is_file() {
            return Ok(path);
        }
        let dir = self.settings.cache_dir.join("tables");
        let bytes = self.blobs.download(key).await?;
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| PortError::Unavailable(format!("table cache dir: {e}")))?;
        let temporary = dir.join(format!("tmp-{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&temporary, &bytes)
            .await
            .map_err(|e| PortError::Unavailable(format!("table cache write: {e}")))?;
        tokio::fs::rename(&temporary, &path)
            .await
            .map_err(|e| PortError::Unavailable(format!("table cache rename: {e}")))?;
        Ok(path)
    }

    async fn delete_blobs(&self, keys: &[String]) {
        for key in keys {
            if let Err(err) = self.blobs.delete(key).await {
                // The row is gone either way; a leaked blob is an operator
                // problem, not a request failure.
                tracing::warn!(error = %err, %key, "table grid blob delete failed");
            }
        }
    }
}

#[async_trait]
impl TableStore for TablesStore {
    async fn put_tables(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
        tables: Vec<(TableGrid, TableManifest)>,
    ) -> Result<Vec<TableRecord>, PortError> {
        if tables.is_empty() {
            let keys = self.records.delete_all(tenant, document).await?;
            self.delete_blobs(&keys).await;
            return Ok(Vec::new());
        }
        let version = self
            .records
            .latest_version(tenant, document)
            .await?
            .saturating_add(1);
        let mut rows = Vec::with_capacity(tables.len());
        let mut uploaded: Vec<String> = Vec::with_capacity(tables.len());
        for (index, (grid, manifest)) in tables.into_iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let id = TableId::of(document, index);
            // Capture what the record needs before the grid is moved into the
            // blocking task: a 200k-row grid is not cloned.
            let source = grid.source.unwrap_or(kb_core::tables::TableSource::Csv);
            let sheet = grid.sheet.clone();
            let page_span = grid.page_span;
            let title = grid.title.clone();
            let caption = grid.caption.clone();
            let header = grid.header.clone();
            // Writing a 200k-row grid is CPU work: keep it off the async
            // threads the rest of the worker is using. The manifest comes back
            // with the bytes, so the record needs no second copy.
            let (bytes, manifest) = tokio::task::spawn_blocking(move || {
                parquet::write_grid(&grid, Some(&manifest)).map(|bytes| (bytes, manifest))
            })
            .await
            .map_err(|e| PortError::Unavailable(format!("table grid task: {e}")))??;
            let grid_key = table_grid_path(
                Some(tenant.as_str()),
                document.as_str(),
                id.as_str(),
                version,
            );
            if let Err(err) = self
                .blobs
                .upload(&grid_key, bytes, "application/vnd.apache.parquet")
                .await
            {
                self.delete_blobs(&uploaded).await;
                return Err(err);
            }
            uploaded.push(grid_key.clone());
            rows.push(TableRecord {
                id,
                tenant_id: tenant.clone(),
                document_id: document.clone(),
                index,
                version,
                source,
                sheet,
                page_span,
                title,
                caption,
                header,
                manifest,
                grid_key,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            });
        }
        if let Err(err) = self.records.insert_rows(&rows).await {
            // The rows never became visible, so their blobs are garbage.
            self.delete_blobs(&uploaded).await;
            return Err(err);
        }
        // Two versions kept: the one just replaced may still be in a query.
        if version > 2 {
            let old = self
                .records
                .delete_older_than(tenant, document, version - 2)
                .await?;
            self.delete_blobs(&old).await;
        }
        Ok(rows)
    }

    async fn list_tables(
        &self,
        tenant: &TenantId,
        document: Option<&DocumentId>,
    ) -> Result<Vec<TableRecord>, PortError> {
        self.records.current_tables(tenant, document).await
    }

    async fn get_table(
        &self,
        tenant: &TenantId,
        id: &TableId,
    ) -> Result<Option<TableRecord>, PortError> {
        self.records.current_table(tenant, id).await
    }

    async fn query(
        &self,
        tenant: &TenantId,
        id: &TableId,
        query: &TableQuery,
        policy: &QueryPolicy,
    ) -> Result<QueryResult, TableQueryFailure> {
        let record = self
            .records
            .current_table(tenant, id)
            .await?
            .ok_or_else(|| {
                PortError::Invalid(format!("table {id} does not exist in this tenant"))
            })?;
        let compiled = compiler::compile(&record.manifest, query, policy)
            .map_err(TableQueryFailure::Invalid)?;
        let path = self.local_grid(&record.grid_key).await?;
        let timeout = self.settings.query_timeout_ms;
        let output = {
            let compiled = compiled.clone();
            let path = path.clone();
            tokio::time::timeout(
                Duration::from_millis(timeout.saturating_mul(4).saturating_add(1_000)),
                tokio::task::spawn_blocking(move || parquet::run(&compiled, &path, timeout)),
            )
            .await
            .map_err(|_| PortError::Timeout)?
            .map_err(|e| PortError::Unavailable(format!("table query task: {e}")))?
            .map_err(|(timed_out, message)| {
                if timed_out {
                    PortError::Timeout
                } else {
                    PortError::Unavailable(message)
                }
            })?
        };

        let mut result = receipt(&record, &compiled, output, policy);
        result.columns = compiled.columns.clone();
        Ok(result)
    }

    async fn delete_document_tables(
        &self,
        tenant: &TenantId,
        document: &DocumentId,
    ) -> Result<(), PortError> {
        let keys = self.records.delete_all(tenant, document).await?;
        self.delete_blobs(&keys).await;
        Ok(())
    }
}

/// Turn an engine run into the receipt, applying suppression and warnings.
fn receipt(
    record: &TableRecord,
    compiled: &compiler::CompiledQuery,
    output: RunOutput,
    policy: &QueryPolicy,
) -> QueryResult {
    let RunOutput {
        mut rows,
        rows_matched,
        excluded_derived_rows,
        group_counts,
    } = output;
    // Measured before suppression: the fetch asked for one row more than the
    // limit, so "there was more" is a fact about the query, not about what the
    // privacy rule happened to hide.
    let truncated = u32::try_from(rows.len()).unwrap_or(u32::MAX) > compiled.limit;
    if truncated {
        rows.truncate(compiled.limit as usize);
    }
    let (suppressed_groups, mut warnings) =
        suppress(&mut rows, compiled, &group_counts, rows_matched, policy);
    let mut warnings_all = Vec::new();
    warnings_all.append(&mut warnings);
    warnings_all.push(format!(
        "computed from a {} table{}",
        source_word(record.source),
        record
            .sheet
            .as_deref()
            .map_or(String::new(), |sheet| format!(", sheet \"{sheet}\""))
    ));
    if excluded_derived_rows > 0 {
        warnings_all.push(format!(
            "{excluded_derived_rows} subtotal/total row(s) were excluded; set includeDerived to count them"
        ));
    }
    match record.manifest.confidence {
        kb_core::tables::Confidence::Low => warnings_all.push(
            "the table's structure was guessed (OCR or an irregular grid); check the columns before relying on a total"
                .to_string(),
        ),
        kb_core::tables::Confidence::Medium => warnings_all.push(
            "subtotal rows were identified by their labels, not by formulas".to_string(),
        ),
        kb_core::tables::Confidence::High => {}
    }
    for note in &record.manifest.notes {
        warnings_all.push(format!("extraction note: {note}"));
    }
    if record.manifest.truncated {
        warnings_all.push(format!(
            "the source table was truncated at {} rows when it was stored",
            record.manifest.n_rows
        ));
    }
    if truncated {
        warnings_all.push(format!(
            "output truncated at {} row(s); add a filter or a smaller limit",
            compiled.limit
        ));
    }
    QueryResult {
        columns: compiled.columns.clone(),
        rows,
        rows_matched,
        excluded_derived_rows,
        suppressed_groups,
        truncated,
        warnings: warnings_all,
    }
}

fn source_word(source: kb_core::tables::TableSource) -> &'static str {
    match source {
        kb_core::tables::TableSource::Xlsx => "xlsx",
        kb_core::tables::TableSource::Xls => "xls",
        kb_core::tables::TableSource::Ods => "ods",
        kb_core::tables::TableSource::Csv => "csv",
        kb_core::tables::TableSource::Docx => "docx",
        kb_core::tables::TableSource::Markdown => "markdown",
        kb_core::tables::TableSource::Pdf => "pdf",
    }
}

/// Apply `min_group` privacy suppression.
///
/// Returns how many groups were hidden and the warning that says so. When
/// exactly one group would be hidden, the next-smallest visible group is hidden
/// too: otherwise the one hidden group's size is recoverable by subtracting the
/// visible ones from the total.
fn suppress(
    rows: &mut Vec<Vec<Value>>,
    compiled: &compiler::CompiledQuery,
    group_counts: &std::collections::HashMap<String, u64>,
    rows_matched: u64,
    policy: &QueryPolicy,
) -> (u32, Vec<String>) {
    let min_group = u64::from(policy.min_group);
    if min_group < 2 {
        return (0, Vec::new());
    }
    if !compiled.grouped {
        // One implicit group. An aggregate over fewer rows than the policy
        // allows is not reportable at all.
        if policy.aggregate_only && rows_matched > 0 && rows_matched < min_group {
            rows.clear();
            return (
                1,
                vec![format!(
                    "the result was suppressed: it covers {rows_matched} row(s), fewer than the {min_group} the privacy policy allows"
                )],
            );
        }
        return (0, Vec::new());
    }

    let key_of = |row: &Vec<Value>| -> String {
        let values: Vec<Value> = compiled
            .group_positions
            .iter()
            .map(|position| row.get(*position).cloned().unwrap_or(Value::Null))
            .collect();
        parquet::group_key(&values)
    };
    let mut hidden: Vec<String> = rows
        .iter()
        .map(key_of)
        .filter(|key| group_counts.get(key).copied().unwrap_or(0) < min_group)
        .collect();
    hidden.sort();
    hidden.dedup();
    if hidden.len() == 1 {
        // Complementary suppression: hide the next-smallest group that is not
        // already hidden, so the hidden one cannot be derived by subtraction.
        let pivot = hidden[0].clone();
        let next = rows
            .iter()
            .map(key_of)
            .filter(|key| key != &pivot && !hidden.contains(key))
            .min_by_key(|key| (group_counts.get(key).copied().unwrap_or(0), key.clone()));
        if let Some(next) = next {
            hidden.push(next);
        }
    }
    if hidden.is_empty() {
        return (0, Vec::new());
    }
    rows.retain(|row| !hidden.contains(&key_of(row)));
    let hidden_count = u32::try_from(hidden.len()).unwrap_or(u32::MAX);
    (
        hidden_count,
        vec![format!(
            "{hidden_count} group(s) were suppressed: fewer than {min_group} row(s) each (complementary suppression included)"
        )],
    )
}

/// The cache file a blob key maps to. Exposed for tests and operators who want
/// to see what is on disk.
#[must_use]
pub fn cache_file(cache_dir: &Path, key: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    cache_dir
        .join("tables")
        .join(format!("{}.parquet", hex::encode(hasher.finalize())))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kb_core::tables::TableSource;
    use kb_core::tables::{
        Cell, ColumnInfo, ColumnKind, ColumnRole, Confidence, DEFAULT_MIN_GROUP, QueryError,
    };
    use std::collections::HashMap as Map;
    use std::sync::Mutex;

    fn manifest() -> TableManifest {
        let col = |name: &str, kind, role| ColumnInfo {
            name: name.to_string(),
            kind,
            role,
            unit: None,
            sample_values: Vec::new(),
            distinct_count: 0,
        };
        TableManifest {
            columns: vec![
                col("Region", ColumnKind::Text, ColumnRole::Identifier),
                col("Revenue", ColumnKind::Number, ColumnRole::Quantity),
            ],
            derived_rows: vec![3],
            formulas_known: false,
            confidence: Confidence::Medium,
            n_rows: 4,
            n_cols: 2,
            truncated: false,
            notes: Vec::new(),
        }
    }

    fn grid() -> TableGrid {
        TableGrid {
            source: Some(TableSource::Csv),
            header: vec!["Region".into(), "Revenue".into()],
            rows: vec![
                vec![Cell::Text("Bali".into()), Cell::Number(10.0)],
                vec![Cell::Text("Bali".into()), Cell::Number(10.0)],
                vec![Cell::Text("Jawa".into()), Cell::Number(10.0)],
                vec![Cell::Text("Jumlah".into()), Cell::Number(20.0)],
            ],
            ..TableGrid::default()
        }
    }

    /// In-memory records with the same versioning rule as the Postgres queries.
    #[derive(Default, Clone)]
    struct MemoryRecords {
        rows: Arc<Mutex<Vec<TableRecord>>>,
    }

    #[async_trait]
    impl TableRecords for MemoryRecords {
        async fn insert_rows(&self, rows: &[TableRecord]) -> Result<(), PortError> {
            self.rows.lock().unwrap().extend_from_slice(rows);
            Ok(())
        }
        async fn current_tables(
            &self,
            tenant: &TenantId,
            document: Option<&DocumentId>,
        ) -> Result<Vec<TableRecord>, PortError> {
            let rows = self.rows.lock().unwrap();
            let mut out: Vec<TableRecord> = rows
                .iter()
                .filter(|r| &r.tenant_id == tenant)
                .filter(|r| document.is_none_or(|d| &r.document_id == d))
                .filter(|r| {
                    let latest = rows
                        .iter()
                        .filter(|o| o.tenant_id == r.tenant_id && o.document_id == r.document_id)
                        .map(|o| o.version)
                        .max()
                        .unwrap_or(0);
                    r.version == latest
                })
                .cloned()
                .collect();
            out.sort_by(|a, b| (&a.document_id.0, a.index).cmp(&(&b.document_id.0, b.index)));
            Ok(out)
        }
        async fn current_table(
            &self,
            tenant: &TenantId,
            id: &TableId,
        ) -> Result<Option<TableRecord>, PortError> {
            Ok(self
                .current_tables(tenant, None)
                .await?
                .into_iter()
                .find(|r| &r.id == id))
        }
        async fn latest_version(
            &self,
            tenant: &TenantId,
            document: &DocumentId,
        ) -> Result<u32, PortError> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|r| &r.tenant_id == tenant && &r.document_id == document)
                .map(|r| r.version)
                .max()
                .unwrap_or(0))
        }
        async fn delete_older_than(
            &self,
            tenant: &TenantId,
            document: &DocumentId,
            keep_from: u32,
        ) -> Result<Vec<String>, PortError> {
            let mut rows = self.rows.lock().unwrap();
            let removed: Vec<String> = rows
                .iter()
                .filter(|r| {
                    &r.tenant_id == tenant && &r.document_id == document && r.version <= keep_from
                })
                .map(|r| r.grid_key.clone())
                .collect();
            rows.retain(|r| {
                !(&r.tenant_id == tenant && &r.document_id == document && r.version <= keep_from)
            });
            Ok(removed)
        }
        async fn delete_all(
            &self,
            tenant: &TenantId,
            document: &DocumentId,
        ) -> Result<Vec<String>, PortError> {
            let mut rows = self.rows.lock().unwrap();
            let removed: Vec<String> = rows
                .iter()
                .filter(|r| &r.tenant_id == tenant && &r.document_id == document)
                .map(|r| r.grid_key.clone())
                .collect();
            rows.retain(|r| !(&r.tenant_id == tenant && &r.document_id == document));
            Ok(removed)
        }
    }

    /// A blob store in a map.
    #[derive(Default, Clone)]
    struct MemoryBlobs {
        objects: Arc<Mutex<Map<String, Vec<u8>>>>,
    }

    #[async_trait]
    impl BlobStore for MemoryBlobs {
        async fn upload(
            &self,
            key: &str,
            body: Vec<u8>,
            _content_type: &str,
        ) -> Result<u64, PortError> {
            let len = body.len() as u64;
            self.objects.lock().unwrap().insert(key.to_string(), body);
            Ok(len)
        }
        async fn download(&self, key: &str) -> Result<Vec<u8>, PortError> {
            self.objects
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| PortError::Unavailable(format!("no blob {key}")))
        }
        async fn delete(&self, key: &str) -> Result<(), PortError> {
            self.objects.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kb-tables-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store() -> (TablesStore, MemoryRecords, MemoryBlobs, PathBuf) {
        let records = MemoryRecords::default();
        let blobs = MemoryBlobs::default();
        let dir = temp_dir();
        let store = TablesStore::new(
            Arc::new(records.clone()),
            Arc::new(blobs.clone()),
            TableStoreSettings {
                cache_dir: dir.clone(),
                query_timeout_ms: 5_000,
            },
        );
        (store, records, blobs, dir)
    }

    fn tenant() -> TenantId {
        TenantId::new_unchecked("tenant-a")
    }

    fn document() -> DocumentId {
        DocumentId::new_unchecked("doc-1")
    }

    async fn put_one(store: &TablesStore) -> Vec<TableRecord> {
        store
            .put_tables(&tenant(), &document(), vec![(grid(), manifest())])
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_sum_excludes_the_subtotal_and_reports_the_receipt() {
        let (store, _records, _blobs, dir) = store();
        let records = put_one(&store).await;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, TableId::of(&document(), 0));
        assert_eq!(records[0].version, 1);

        let result = store
            .query(
                &tenant(),
                &TableId::of(&document(), 0),
                &TableQuery {
                    select: vec![kb_core::tables::SelectItem::Aggregate {
                        agg: kb_core::tables::Agg::Sum,
                        column: Some("Revenue".to_string()),
                        alias: Some("total".to_string()),
                    }],
                    ..TableQuery::default()
                },
                &QueryPolicy {
                    aggregate_only: false,
                    min_group: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(result.columns, vec!["total"]);
        // Whole results are JSON integers, not `30.0` (TBL-2).
        assert_eq!(result.rows, vec![vec![Value::from(30)]]);
        assert_eq!(result.rows_matched, 3);
        assert_eq!(result.excluded_derived_rows, 1);
        assert!(result.warnings.iter().any(|w| w.contains("subtotal")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_second_version_replaces_the_first_and_the_first_grid_is_gone() {
        let (store, records, blobs, dir) = store();
        let first = put_one(&store).await;
        let first_key = first[0].grid_key.clone();
        let second = put_one(&store).await;
        assert_eq!(second[0].version, 2);
        assert_ne!(first_key, second[0].grid_key);
        assert_eq!(
            store.list_tables(&tenant(), None).await.unwrap().len(),
            1,
            "only the current version is listed"
        );
        // Two versions are kept, and a third drops the first row and blob.
        let third = put_one(&store).await;
        assert_eq!(third[0].version, 3);
        assert!(!blobs.objects.lock().unwrap().contains_key(&first_key));
        let _ = records;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn another_tenant_sees_nothing_and_the_query_is_refused() {
        let (store, _records, _blobs, dir) = store();
        put_one(&store).await;
        let other = TenantId::new_unchecked("tenant-b");
        assert!(store.list_tables(&other, None).await.unwrap().is_empty());
        assert!(
            store
                .get_table(&other, &TableId::of(&document(), 0))
                .await
                .unwrap()
                .is_none()
        );
        let failure = store
            .query(
                &other,
                &TableId::of(&document(), 0),
                &TableQuery {
                    select: vec![kb_core::tables::SelectItem::Aggregate {
                        agg: kb_core::tables::Agg::Count,
                        column: None,
                        alias: None,
                    }],
                    ..TableQuery::default()
                },
                &QueryPolicy::default(),
            )
            .await;
        assert!(matches!(
            failure,
            Err(TableQueryFailure::Store(PortError::Invalid(_)))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn group_suppression_hides_small_groups_and_one_complement() {
        let (mut store, _records, _blobs, dir) = store();
        let mut grid = grid();
        // Four groups: Bali (3 rows), Jawa (2), Sunda (1), Papua (1). With
        // min_group 2 the two single-row groups are hidden; exactly two is not
        // the "exactly one" case, so no complement is added.
        grid.rows = vec![
            vec![Cell::Text("Bali".into()), Cell::Number(1.0)],
            vec![Cell::Text("Bali".into()), Cell::Number(1.0)],
            vec![Cell::Text("Bali".into()), Cell::Number(1.0)],
            vec![Cell::Text("Jawa".into()), Cell::Number(1.0)],
            vec![Cell::Text("Jawa".into()), Cell::Number(1.0)],
            vec![Cell::Text("Sunda".into()), Cell::Number(1.0)],
            vec![Cell::Text("Papua".into()), Cell::Number(1.0)],
        ];
        let mut manifest = manifest();
        manifest.derived_rows.clear();
        manifest.n_rows = 7;
        store = TablesStore::new(
            store.records.clone(),
            store.blobs.clone(),
            store.settings.clone(),
        );
        store
            .put_tables(&tenant(), &document(), vec![(grid, manifest)])
            .await
            .unwrap();
        let result = store
            .query(
                &tenant(),
                &TableId::of(&document(), 0),
                &TableQuery {
                    select: vec![kb_core::tables::SelectItem::Column {
                        column: "Region".to_string(),
                        alias: None,
                    }],
                    group_by: vec!["Region".to_string()],
                    ..TableQuery::default()
                },
                &QueryPolicy {
                    aggregate_only: false,
                    min_group: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(result.suppressed_groups, 2);
        let regions: Vec<&str> = result
            .rows
            .iter()
            .filter_map(|row| row.first().and_then(Value::as_str))
            .collect();
        assert_eq!(regions, vec!["Bali", "Jawa"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn aggregate_only_suppresses_an_ungrouped_result_over_too_few_rows() {
        let (store, _records, _blobs, dir) = store();
        put_one(&store).await;
        let result = store
            .query(
                &tenant(),
                &TableId::of(&document(), 0),
                &TableQuery {
                    select: vec![kb_core::tables::SelectItem::Aggregate {
                        agg: kb_core::tables::Agg::Count,
                        column: None,
                        alias: None,
                    }],
                    ..TableQuery::default()
                },
                &QueryPolicy {
                    aggregate_only: true,
                    min_group: DEFAULT_MIN_GROUP,
                },
            )
            .await
            .unwrap();
        assert!(result.rows.is_empty());
        assert_eq!(result.suppressed_groups, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn hard_delete_removes_rows_and_grid_blobs() {
        let (store, records, blobs, dir) = store();
        put_one(&store).await;
        assert!(!blobs.objects.lock().unwrap().is_empty());
        store
            .delete_document_tables(&tenant(), &document())
            .await
            .unwrap();
        assert!(records.rows.lock().unwrap().is_empty());
        assert!(blobs.objects.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_empty_replace_clears_the_document() {
        let (store, records, blobs, dir) = store();
        put_one(&store).await;
        store
            .put_tables(&tenant(), &document(), Vec::new())
            .await
            .unwrap();
        assert!(records.rows.lock().unwrap().is_empty());
        assert!(blobs.objects.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_invalid_query_is_a_caller_error_not_a_store_error() {
        let (store, _records, _blobs, dir) = store();
        put_one(&store).await;
        let failure = store
            .query(
                &tenant(),
                &TableId::of(&document(), 0),
                &TableQuery {
                    select: vec![kb_core::tables::SelectItem::Aggregate {
                        agg: kb_core::tables::Agg::Sum,
                        column: Some("Region".to_string()),
                        alias: None,
                    }],
                    ..TableQuery::default()
                },
                &QueryPolicy::default(),
            )
            .await;
        assert!(matches!(
            failure,
            Err(TableQueryFailure::Invalid(QueryError::NotSummable { .. }))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
