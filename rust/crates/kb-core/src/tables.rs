//! Tables as data, not text (`TBL-1`, `docs/core/specs/tbl-1.md`).
//!
//! The **contract** every table piece builds against: the grid an extractor
//! produces, the manifest that says what each column means and which rows are
//! derived (subtotals), the typed query a caller sends, and the result that
//! comes back. Pure data and pure functions only: this crate does no I/O. The
//! store (`kb-ports::TableStore`, DuckDB over Parquet) and the extractors fill
//! and read these types.
//!
//! Modelled on paragon `kb-bridge` (structured grids, a manifest with column
//! roles and formula-proven subtotal rows, a parameterised `tabel_query`
//! rather than SQL) and on TableRAG (retrieve the schema, compute the answer
//! in code). See `docs/architecture-target.md` §10.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod manifest;
mod markdown;

pub use manifest::{build_manifest, is_derived_label, locale_number};
pub use markdown::{parse_markdown_tables, render_table_markdown, stitch_and_drop_furniture};

/// Rows a query may return, whatever `limit` asks for.
pub const MAX_ROWS_RETURNED: u32 = 1000;
/// Wall-clock budget for one query, in milliseconds.
pub const QUERY_TIMEOUT_MS: u64 = 5_000;
/// Rows a stored table may hold; a larger source is truncated and the
/// manifest says so.
pub const MAX_TABLE_ROWS: u32 = 200_000;
/// Default smallest group a privacy-restricted table may report.
pub const DEFAULT_MIN_GROUP: u32 = 5;

/// Where a table came from. It decides how much the manifest can know: an
/// xlsx carries formulas, a PDF table's subtotals can only be guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TableSource {
    Xlsx,
    Xls,
    Ods,
    Csv,
    Docx,
    /// A markdown pipe table from an OCR or layout extractor (Mistral,
    /// MinerU, the OCR router's chat models).
    Markdown,
    /// A table found in a PDF text layer.
    Pdf,
}

/// One cell's value. Numbers are parsed at extraction (locale-aware: `1.234,5`
/// in an Indonesian sheet is 1234.5); the raw text is kept in `Text` when a
/// value is not cleanly a number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
pub enum Cell {
    #[default]
    Empty,
    Text(String),
    Number(f64),
    Bool(bool),
    /// ISO 8601 (`2024-03-31`, or with a time).
    Date(String),
}

/// A table as an extractor found it: a header and rows of cells.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TableGrid {
    pub source: Option<TableSource>,
    /// Sheet name for spreadsheets.
    #[serde(default)]
    pub sheet: Option<String>,
    /// 1-based first and last page for paged sources.
    #[serde(default)]
    pub page_span: Option<(u32, u32)>,
    /// The table's title: the text just above it, a sheet title row, or a
    /// caption such as "Tabel 3.1 …".
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    /// One name per column. A multi-level header is flattened with " / "
    /// (`2023 / Q1`). Names are unique within a table (duplicates get " (2)").
    pub header: Vec<String>,
    /// How many source rows the header was built from (1 for a flat header).
    #[serde(default)]
    pub header_rows: u32,
    /// Data rows; every row has `header.len()` cells.
    pub rows: Vec<Vec<Cell>>,
    /// 0-based indices of rows whose numeric cells are formulas (xlsx/ods),
    /// the strongest evidence a row is derived (a subtotal).
    #[serde(default)]
    pub formula_rows: Vec<u32>,
    /// Extraction notes the manifest carries forward: "hidden sheet skipped",
    /// "stitched across pages 4-5", "truncated at 200000 rows".
    #[serde(default)]
    pub notes: Vec<String>,
    /// True when rows were dropped to fit `MAX_TABLE_ROWS`.
    #[serde(default)]
    pub truncated: bool,
}

/// What a column holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnKind {
    Number,
    Text,
    Date,
    Bool,
    Mixed,
    Empty,
}

/// What a column *means*, which decides what may be computed over it. A
/// phone number, a postcode or a year is numeric but never summed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnRole {
    /// A measured amount: summable.
    Quantity,
    /// A code or number that names something (NIK, kode wilayah, phone).
    Code,
    /// The row's key (a name, a region).
    Identifier,
    /// Other descriptive text.
    Label,
    /// A date or a period (a year column is a Date, not a Quantity).
    Date,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub kind: ColumnKind,
    pub role: ColumnRole,
    /// Unit as written (`Rp juta`, `%`, `ton`), never applied to the values.
    #[serde(default)]
    pub unit: Option<String>,
    /// Up to 10 frequent distinct values (non-quantity columns), for schema
    /// retrieval and for value linking.
    #[serde(default)]
    pub sample_values: Vec<String>,
    #[serde(default)]
    pub distinct_count: u32,
}

/// How far the manifest's own reading of the table can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Formulas known (xlsx/ods) or a clean flat grid.
    High,
    /// Derived rows guessed from labels ("Total", "Jumlah").
    Medium,
    /// Structure guessed (ragged rows, OCR'd table, stitched pages).
    Low,
}

/// What a table means, built from its grid (`kb_core::tables` heuristics).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableManifest {
    pub columns: Vec<ColumnInfo>,
    /// 0-based rows that are derived (subtotals/totals). Excluded from every
    /// aggregate unless the query sets `include_derived`.
    pub derived_rows: Vec<u32>,
    /// Derived rows proven by formulas rather than guessed from labels.
    pub formulas_known: bool,
    pub confidence: Confidence,
    pub n_rows: u32,
    pub n_cols: u32,
    pub truncated: bool,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Aggregate functions a query may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agg {
    Sum,
    Avg,
    Min,
    Max,
    Count,
    CountDistinct,
}

/// One output column: a plain column, or an aggregate.
///
/// JSON: `{"column":"Region"}` or `{"agg":"sum","column":"Revenue","as":"total"}`;
/// `{"agg":"count"}` with no column counts rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SelectItem {
    Aggregate {
        agg: Agg,
        #[serde(default)]
        column: Option<String>,
        #[serde(default, rename = "as")]
        alias: Option<String>,
    },
    Column {
        column: String,
        #[serde(default, rename = "as")]
        alias: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterOp {
    #[serde(rename = "=")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    /// Case-insensitive substring on the text form.
    #[serde(rename = "contains")]
    Contains,
    /// `value` is an array.
    #[serde(rename = "in")]
    In,
    /// `value` is `[low, high]`, inclusive.
    #[serde(rename = "between")]
    Between,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBy {
    /// An output column name or alias.
    pub key: String,
    #[serde(default)]
    pub dir: SortDir,
}

/// The typed query `POST /v1/tables/{id}/query` accepts. Never SQL: columns
/// are checked against the manifest, values are bound as parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TableQuery {
    #[serde(default)]
    pub select: Vec<SelectItem>,
    #[serde(default)]
    pub filter: Vec<Filter>,
    #[serde(default)]
    pub group_by: Vec<String>,
    #[serde(default)]
    pub order_by: Vec<OrderBy>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Count subtotal/total rows too (off by default: they double-count).
    #[serde(default)]
    pub include_derived: bool,
}

/// Privacy rules for one query, from the document's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPolicy {
    /// Only aggregates may be returned: no plain-column rows, no MIN/MAX, and
    /// groups smaller than `min_group` are suppressed (with a complementary
    /// suppression when exactly one group would be hidden).
    pub aggregate_only: bool,
    pub min_group: u32,
}

impl Default for QueryPolicy {
    fn default() -> Self {
        Self {
            aggregate_only: false,
            min_group: DEFAULT_MIN_GROUP,
        }
    }
}

/// What a query returns: the rows, and the receipt that lets a caller say
/// exactly how the number was computed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// Rows the query matched before grouping.
    pub rows_matched: u64,
    /// Derived rows left out of the computation (`include_derived` false).
    pub excluded_derived_rows: u32,
    /// Groups hidden by the privacy policy.
    pub suppressed_groups: u32,
    /// True when `MAX_ROWS_RETURNED` or `limit` cut the output.
    pub truncated: bool,
    /// Plain-language caveats: "computed from a PDF table; subtotal rows were
    /// guessed", "column Telepon is a code and was not summed".
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Why a query was refused before it ran. Every variant is a 400 at the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum QueryError {
    UnknownColumn {
        column: String,
        available: Vec<String>,
    },
    /// SUM/AVG over a column whose role is not Quantity.
    NotSummable {
        column: String,
        role: ColumnRole,
    },
    /// A plain column in `select` that is neither grouped nor aggregated.
    NotGrouped {
        column: String,
    },
    /// The privacy policy forbids this (plain rows, MIN/MAX).
    Restricted {
        reason: String,
    },
    BadValue {
        column: String,
        reason: String,
    },
    Empty {
        reason: String,
    },
}
