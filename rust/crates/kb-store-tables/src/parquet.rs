//! The grid as Parquet, written and read by DuckDB.
//!
//! A stored grid is one typed column per header plus `_row` (0-based, matching
//! the manifest's `derived_rows`) and `_derived` (the subtotal flag). Writing
//! goes through DuckDB's own `COPY … (FORMAT PARQUET)` rather than an Arrow
//! writer: the query path needs DuckDB anyway, and this keeps the write and
//! read type mappings in one file.
//!
//! Typing follows the manifest: `Number` columns are `DOUBLE`, `Bool` is
//! `BOOLEAN`, and everything else (text, date, mixed) is `VARCHAR`. A cell that
//! does not fit its column's type becomes `NULL` rather than failing the whole
//! document: one bad cell must not lose a table.

use std::collections::HashMap;
use std::path::Path;

use duckdb::types::Value as DuckValue;
use duckdb::{Connection, params_from_iter};
use kb_core::tables::{Cell, ColumnKind, TableGrid, TableManifest};
use kb_ports::PortError;

/// Statement that creates the staging table for a grid.
pub(crate) const CREATE_STAGING: &str = "CREATE TABLE \"grid\" (";

/// The two housekeeping columns, always last.
pub(crate) const ROW_COLUMN: &str = "_row";
pub(crate) const DERIVED_COLUMN: &str = "_derived";

/// A column's DuckDB type.
fn sql_type(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Number => "DOUBLE",
        ColumnKind::Bool => "BOOLEAN",
        ColumnKind::Text | ColumnKind::Date | ColumnKind::Mixed | ColumnKind::Empty => "VARCHAR",
    }
}

/// Quote an identifier for DuckDB.
fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Quote a filesystem path for a SQL literal.
fn quote_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// The type of each grid column, from the manifest when it agrees with the
/// header, else from the header alone (all text). The manifest is the contract;
/// a mismatch means the caller built it from a different grid, and a text
/// column is the safe fallback.
fn column_kinds(grid: &TableGrid, manifest: Option<&TableManifest>) -> Vec<ColumnKind> {
    let mut kinds: Vec<ColumnKind> = grid.header.iter().map(|_| ColumnKind::Text).collect();
    if let Some(manifest) = manifest
        && manifest.columns.len() == grid.header.len()
    {
        for (index, column) in manifest.columns.iter().enumerate() {
            kinds[index] = column.kind;
        }
    }
    kinds
}

/// One cell as a typed DuckDB value for its column.
fn cell_value(cell: &Cell, kind: ColumnKind) -> DuckValue {
    match kind {
        ColumnKind::Number => match cell {
            Cell::Number(n) if n.is_finite() => DuckValue::Double(*n),
            _ => DuckValue::Null,
        },
        ColumnKind::Bool => match cell {
            Cell::Bool(b) => DuckValue::Boolean(*b),
            Cell::Text(s) if s.eq_ignore_ascii_case("true") => DuckValue::Boolean(true),
            Cell::Text(s) if s.eq_ignore_ascii_case("false") => DuckValue::Boolean(false),
            _ => DuckValue::Null,
        },
        ColumnKind::Text | ColumnKind::Date | ColumnKind::Mixed | ColumnKind::Empty => match cell {
            Cell::Empty => DuckValue::Null,
            Cell::Text(s) | Cell::Date(s) => DuckValue::Text(s.clone()),
            Cell::Number(n) => DuckValue::Text(format_number(*n)),
            Cell::Bool(b) => DuckValue::Text(b.to_string()),
        },
    }
}

/// `1.0` prints as `1`, `1.5` as `1.5`: a table cell is read, not computed, and
/// a number that came from an integer column should not grow a decimal point.
#[must_use]
pub fn format_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

/// Write one grid to Parquet and return the file's bytes.
///
/// The staging table is built in an in-memory DuckDB, filled with bound
/// parameters (a cell value is never SQL text), copied to a temporary file and
/// read back. The file is removed before returning, whatever happened.
///
/// # Errors
///
/// A DuckDB or filesystem failure. The caller treats that as a store failure.
pub fn write_grid(
    grid: &TableGrid,
    manifest: Option<&TableManifest>,
) -> Result<Vec<u8>, PortError> {
    let kinds = column_kinds(grid, manifest);
    let derived: std::collections::HashSet<u32> = manifest
        .map(|m| m.derived_rows.iter().copied().collect())
        .unwrap_or_default();

    let conn = Connection::open_in_memory()
        .map_err(|e| PortError::Unavailable(format!("duckdb open: {e}")))?;
    let mut definition = String::from(CREATE_STAGING);
    for (index, name) in grid.header.iter().enumerate() {
        definition.push_str(&format!("{} {}, ", quote(name), sql_type(kinds[index])));
    }
    definition.push_str(&format!("{ROW_COLUMN} INTEGER, {DERIVED_COLUMN} BOOLEAN)"));
    conn.execute_batch(&definition)
        .map_err(|e| PortError::Unavailable(format!("duckdb create grid: {e}")))?;

    let placeholders = vec!["?"; grid.header.len() + 2].join(", ");
    let insert_sql = format!("INSERT INTO \"grid\" VALUES ({placeholders})");
    {
        let mut statement = conn
            .prepare(&insert_sql)
            .map_err(|e| PortError::Unavailable(format!("duckdb prepare insert: {e}")))?;
        for (row_index, row) in grid.rows.iter().enumerate() {
            let mut values: Vec<DuckValue> = Vec::with_capacity(grid.header.len() + 2);
            for (column, kind) in kinds.iter().enumerate() {
                values.push(
                    row.get(column)
                        .map_or(DuckValue::Null, |cell| cell_value(cell, *kind)),
                );
            }
            let row_u32 = u32::try_from(row_index).unwrap_or(u32::MAX);
            values.push(DuckValue::Int(i32::try_from(row_u32).unwrap_or(i32::MAX)));
            values.push(DuckValue::Boolean(derived.contains(&row_u32)));
            statement
                .execute(params_from_iter(values))
                .map_err(|e| PortError::Unavailable(format!("duckdb insert grid: {e}")))?;
        }
    }

    // A unique temporary file, removed on every path out of this function.
    let path = std::env::temp_dir().join(format!("kb-table-{}.parquet", uuid::Uuid::new_v4()));
    let copy = format!(
        "COPY (SELECT * FROM \"grid\") TO {} (FORMAT PARQUET, COMPRESSION ZSTD)",
        quote_path(&path)
    );
    let result = conn
        .execute_batch(&copy)
        .map_err(|e| PortError::Unavailable(format!("duckdb copy parquet: {e}")))
        .and_then(|()| {
            std::fs::read(&path)
                .map_err(|e| PortError::Unavailable(format!("parquet read back: {e}")))
        });
    let _ = std::fs::remove_file(&path);
    result
}

/// One engine run's output.
#[derive(Debug, Default)]
pub struct RunOutput {
    /// The main query's rows, already converted to JSON.
    pub rows: Vec<Vec<serde_json::Value>>,
    /// `rows_matched` before grouping.
    pub rows_matched: u64,
    /// Rows the derived-row exclusion left out.
    pub excluded_derived_rows: u32,
    /// Group key → row count, for the privacy suppression. Empty when the
    /// query is ungrouped or the caller does not need it.
    pub group_counts: HashMap<String, u64>,
}

/// Run a compiled query over one local Parquet file.
///
/// Blocking: the caller runs it on a blocking task. `timeout_ms` is enforced
/// with DuckDB's interrupt handle and a watchdog thread, so a runaway query
/// (a pathological filter over a large table) cannot hold the request open.
///
/// # Errors
///
/// `Err(true)` for a timeout, `Err(false)` for any other DuckDB failure.
pub fn run(
    query: &crate::compiler::CompiledQuery,
    path: &Path,
    timeout_ms: u64,
) -> Result<RunOutput, (bool, String)> {
    let conn = Connection::open_in_memory().map_err(|e| (false, format!("duckdb open: {e}")))?;
    let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let interrupt = conn.interrupt_handle();
    let flag = std::sync::Arc::clone(&interrupted);
    let watchdog = std::thread::spawn(move || {
        if stopped
            .recv_timeout(std::time::Duration::from_millis(timeout_ms))
            .is_err()
        {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            interrupt.interrupt();
        }
    });

    let result = run_inner(&conn, query, path, &interrupted);
    let _ = stop.send(());
    let _ = watchdog.join();
    result.map_err(|(timed_out, message)| {
        if timed_out || interrupted.load(std::sync::atomic::Ordering::SeqCst) {
            (true, format!("query timed out after {timeout_ms} ms"))
        } else {
            (false, message)
        }
    })
}

fn run_inner(
    conn: &Connection,
    query: &crate::compiler::CompiledQuery,
    path: &Path,
    interrupted: &std::sync::atomic::AtomicBool,
) -> Result<RunOutput, (bool, String)> {
    let binds = query_binds(query, path);
    let rows = run_rows(conn, &query.main, &binds, &query.columns)?;
    if interrupted.load(std::sync::atomic::Ordering::SeqCst) {
        return Err((true, "interrupted".to_string()));
    }
    let matched = count(conn, &query.rows_matched, &binds)?;
    let excluded = match &query.derived_matched {
        Some(sql) => count(conn, sql, &binds)?,
        None => 0,
    };
    let group_counts = match &query.group_counts {
        Some(sql) => counts_by_group(conn, sql, &binds, &query.group_positions)?,
        None => HashMap::new(),
    };
    Ok(RunOutput {
        rows,
        rows_matched: matched,
        excluded_derived_rows: u32::try_from(excluded).unwrap_or(u32::MAX),
        group_counts,
    })
}

fn query_binds(query: &crate::compiler::CompiledQuery, path: &Path) -> Vec<DuckValue> {
    query
        .binds
        .iter()
        .map(|param| match param {
            crate::compiler::Param::Grid(_) => DuckValue::Text(path.to_string_lossy().to_string()),
            crate::compiler::Param::Text(s) => DuckValue::Text(s.clone()),
            crate::compiler::Param::Number(n) => DuckValue::Double(*n),
            crate::compiler::Param::Bool(b) => DuckValue::Boolean(*b),
            crate::compiler::Param::Null => DuckValue::Null,
        })
        .collect()
}

fn run_rows(
    conn: &Connection,
    sql: &str,
    binds: &[DuckValue],
    columns: &[String],
) -> Result<Vec<Vec<serde_json::Value>>, (bool, String)> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|e| (false, format!("duckdb prepare: {e}")))?;
    let mut rows = statement
        .query(params_from_iter(binds.iter().cloned()))
        .map_err(|e| (false, format!("duckdb query: {e}")))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| (false, format!("duckdb row: {e}")))?
    {
        let mut values = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            let value: DuckValue = row
                .get(index)
                .map_err(|e| (false, format!("duckdb column {index}: {e}")))?;
            values.push(json_value(value));
        }
        out.push(values);
    }
    Ok(out)
}

fn count(conn: &Connection, sql: &str, binds: &[DuckValue]) -> Result<u64, (bool, String)> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|e| (false, format!("duckdb prepare count: {e}")))?;
    let mut rows = statement
        .query(params_from_iter(binds.iter().cloned()))
        .map_err(|e| (false, format!("duckdb count: {e}")))?;
    let row = rows
        .next()
        .map_err(|e| (false, format!("duckdb count row: {e}")))?
        .ok_or_else(|| (false, "duckdb count returned no row".to_string()))?;
    let value: DuckValue = row
        .get(0)
        .map_err(|e| (false, format!("duckdb count value: {e}")))?;
    Ok(match value {
        DuckValue::BigInt(n) => u64::try_from(n).unwrap_or(0),
        DuckValue::Int(n) => u64::try_from(n).unwrap_or(0),
        DuckValue::UBigInt(n) => n,
        DuckValue::Double(n) if n >= 0.0 => n as u64,
        _ => 0,
    })
}

/// Group key → count, keyed by the JSON text of the group values so it matches
/// the main query's rows exactly.
fn counts_by_group(
    conn: &Connection,
    sql: &str,
    binds: &[DuckValue],
    group_positions: &[usize],
) -> Result<HashMap<String, u64>, (bool, String)> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|e| (false, format!("duckdb prepare groups: {e}")))?;
    let mut rows = statement
        .query(params_from_iter(binds.iter().cloned()))
        .map_err(|e| (false, format!("duckdb groups: {e}")))?;
    let mut out = HashMap::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| (false, format!("duckdb group row: {e}")))?
    {
        let mut values = Vec::with_capacity(group_positions.len());
        for index in 0..group_positions.len() {
            let value: DuckValue = row
                .get(index)
                .map_err(|e| (false, format!("duckdb group column {index}: {e}")))?;
            values.push(json_value(value));
        }
        let count: DuckValue = row
            .get(group_positions.len())
            .map_err(|e| (false, format!("duckdb group count: {e}")))?;
        let count = match count {
            DuckValue::BigInt(n) => u64::try_from(n).unwrap_or(0),
            DuckValue::Int(n) => u64::try_from(n).unwrap_or(0),
            DuckValue::UBigInt(n) => n,
            _ => 0,
        };
        out.insert(group_key(&values), count);
    }
    Ok(out)
}

/// The key both the main rows and the group-count rows hash to.
#[must_use]
pub fn group_key(values: &[serde_json::Value]) -> String {
    serde_json::to_string(values).unwrap_or_default()
}

/// One DuckDB value as JSON. Unsigned and date values are rendered the way a
/// caller expects to read them, never as an opaque integer.
#[must_use]
pub fn json_value(value: DuckValue) -> serde_json::Value {
    use serde_json::Value as Json;
    match value {
        DuckValue::Null => Json::Null,
        DuckValue::Boolean(b) => Json::Bool(b),
        DuckValue::TinyInt(n) => Json::from(n),
        DuckValue::SmallInt(n) => Json::from(n),
        DuckValue::Int(n) => Json::from(n),
        DuckValue::BigInt(n) => Json::from(n),
        // Beyond i64: JSON has no 128-bit integer, so the exact decimal text is
        // the honest rendering rather than a lossy float.
        DuckValue::HugeInt(n) => Json::String(n.to_string()),
        DuckValue::UTinyInt(n) => Json::from(n),
        DuckValue::USmallInt(n) => Json::from(n),
        DuckValue::UInt(n) => Json::from(n),
        DuckValue::UBigInt(n) => Json::from(n),
        DuckValue::UHugeInt(n) => Json::String(n.to_string()),
        DuckValue::Float(n) => number_or_null(f64::from(n)),
        DuckValue::Double(n) => number_or_null(n),
        DuckValue::Text(s) | DuckValue::Enum(s) => Json::String(s),
        DuckValue::Date32(days) => {
            // Days since 1970-01-01, as ISO 8601.
            let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1);
            match epoch
                .zip(chrono::TimeDelta::try_days(i64::from(days)))
                .and_then(|(epoch, offset)| epoch.checked_add_signed(offset))
            {
                Some(date) => Json::String(date.format("%Y-%m-%d").to_string()),
                None => Json::Null,
            }
        }
        other => Json::String(format!("{other:?}")),
    }
}

fn number_or_null(n: f64) -> serde_json::Value {
    serde_json::Number::from_f64(n).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use kb_core::tables::{ColumnInfo, ColumnKind, ColumnRole, Confidence, TableGrid};

    fn grid() -> TableGrid {
        TableGrid {
            header: vec!["Region".into(), "Revenue".into(), "Tahun".into()],
            rows: vec![
                vec![
                    Cell::Text("Jawa Timur".into()),
                    Cell::Number(1.5),
                    Cell::Date("2023-01-01".into()),
                ],
                vec![
                    Cell::Text("Bali".into()),
                    Cell::Number(2.0),
                    Cell::Date("2023-01-01".into()),
                ],
                vec![
                    Cell::Text("Jumlah".into()),
                    Cell::Number(3.5),
                    Cell::Date("2023-01-01".into()),
                ],
            ],
            ..TableGrid::default()
        }
    }

    fn manifest() -> TableManifest {
        let col = |name: &str, kind: ColumnKind, role: ColumnRole| ColumnInfo {
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
                col("Tahun", ColumnKind::Date, ColumnRole::Date),
            ],
            derived_rows: vec![2],
            formulas_known: false,
            confidence: Confidence::Medium,
            n_rows: 3,
            n_cols: 3,
            truncated: false,
            notes: Vec::new(),
        }
    }

    #[test]
    fn a_grid_round_trips_through_parquet_with_types_and_flags() {
        let bytes = write_grid(&grid(), Some(&manifest())).unwrap();
        assert!(
            bytes.starts_with(b"PAR1"),
            "a Parquet file starts with PAR1"
        );
        let path = std::env::temp_dir().join(format!("kb-test-{}.parquet", uuid::Uuid::new_v4()));
        std::fs::write(&path, &bytes).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        let mut statement = conn
            .prepare("SELECT \"Region\", \"Revenue\", \"_row\", \"_derived\" FROM read_parquet(?) ORDER BY \"_row\"")
            .unwrap();
        let binds = [DuckValue::Text(path.to_string_lossy().to_string())];
        let mut rows = statement.query(params_from_iter(binds)).unwrap();
        let mut seen = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let region: String = row.get(0).unwrap();
            let revenue: Option<f64> = row.get(1).unwrap();
            let index: i32 = row.get(2).unwrap();
            let derived: bool = row.get(3).unwrap();
            seen.push((region, revenue, index, derived));
        }
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            seen,
            vec![
                ("Jawa Timur".to_string(), Some(1.5), 0, false),
                ("Bali".to_string(), Some(2.0), 1, false),
                ("Jumlah".to_string(), Some(3.5), 2, true),
            ]
        );
    }

    #[test]
    fn an_empty_grid_still_writes_a_readable_file() {
        let empty = TableGrid {
            header: vec!["A".into(), "B".into()],
            rows: Vec::new(),
            ..TableGrid::default()
        };
        let bytes = write_grid(&empty, None).unwrap();
        assert!(bytes.starts_with(b"PAR1"));
        assert!(bytes.len() > 12);
    }

    #[test]
    fn integer_values_keep_their_integer_shape() {
        assert_eq!(format_number(2.0), "2");
        assert_eq!(format_number(2.5), "2.5");
        assert_eq!(format_number(-3.0), "-3");
        assert_eq!(format_number(1e20), "100000000000000000000");
    }

    #[test]
    fn cells_that_do_not_fit_become_null_rather_than_failing() {
        assert_eq!(
            cell_value(&Cell::Text("n/a".into()), ColumnKind::Number),
            DuckValue::Null
        );
        assert_eq!(
            cell_value(&Cell::Text("true".into()), ColumnKind::Bool),
            DuckValue::Boolean(true)
        );
        assert_eq!(
            cell_value(&Cell::Number(3.0), ColumnKind::Text),
            DuckValue::Text("3".into())
        );
    }
}
