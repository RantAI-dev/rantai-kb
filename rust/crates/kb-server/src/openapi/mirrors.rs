//! Documented mirrors of the wire types that live outside `kb-server`.
//!
//! `utoipa` derives a schema from the Rust type it is given, and the types the
//! API serialises that live in the inner crates cannot derive one: `kb-core`
//! and `kb-ports` are pure-logic crates that must not depend on `utoipa` (the
//! boundary check forbids it, and a crate that knows nothing about HTTP has no
//! business knowing how the HTTP contract is written). So their shapes are
//! mirrored here, field for field.
//!
//! A mirror is only worth having if it cannot drift. Every mirror therefore
//! has a `From<&real>` conversion, and every conversion is tested by
//! serialising the real type and the mirror and comparing the two JSON
//! documents: a field added to `kb_ports::JobView` without its mirror fails
//! the build instead of quietly making the published spec wrong.

use kb_ports::{
    EntityView as PortEntityView, JobView as PortJobView, ProvenanceArm as PortProvenanceArm,
    RelationView as PortRelationView,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ─── Jobs ───────────────────────────────────────────────────────────────────

/// `kb_ports::JobView` — the ingest job as `GET /v1/jobs/{id}` returns it.
///
/// camelCase on the wire, like every other `/v1` shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    /// The job's id.
    pub id: String,
    /// `pending`, `processing`, `waiting_for_ocr`, `success` or `failed`.
    ///
    /// `waiting_for_ocr` is the state a job parks in while the backend it
    /// needs is not there (`#36`). The worker resumes it by itself.
    pub status: String,
    /// The step the job is on, when it reports one.
    pub step: Option<String>,
    /// Percent complete, 0–100.
    pub progress: u32,
    /// The step the job is on, 1-based.
    pub step_current: Option<u32>,
    /// How many steps the job has.
    pub step_total: Option<u32>,
    /// Seconds left, when the job can estimate it.
    pub eta_seconds: Option<i32>,
    /// How many times the job has been attempted.
    pub attempt: i32,
    /// The failure message, when the job failed.
    pub error: Option<String>,
    /// The document the job ingests, when it names one.
    pub document_id: Option<String>,
}

impl From<&PortJobView> for JobView {
    fn from(job: &PortJobView) -> Self {
        Self {
            id: job.id.clone(),
            status: job.status.clone(),
            step: job.step.clone(),
            progress: job.progress,
            step_current: job.step_current,
            step_total: job.step_total,
            eta_seconds: job.eta_seconds,
            attempt: job.attempt,
            error: job.error.clone(),
            document_id: job.document_id.clone(),
        }
    }
}

// ─── Intelligence ───────────────────────────────────────────────────────────

/// `kb_ports::EntityView` — one entity the graph extracted for a document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct EntityView {
    /// The entity's record id.
    pub id: String,
    /// The entity's name as it appears in the text.
    pub name: String,
    /// The entity's kind (`person`, `organisation`, `place`, …).
    #[serde(rename = "type")]
    pub entity_type: String,
    /// How sure the extractor is, 0–1.
    pub confidence: f32,
    /// Whatever else the extractor recorded.
    pub metadata: serde_json::Value,
}

impl From<&PortEntityView> for EntityView {
    fn from(entity: &PortEntityView) -> Self {
        Self {
            id: entity.id.clone(),
            name: entity.name.clone(),
            entity_type: entity.r#type.clone(),
            confidence: entity.confidence,
            metadata: entity.metadata.clone(),
        }
    }
}

/// `kb_ports::RelationView` — one edge of the document's entity graph.
///
/// `in` and `out` are the record ids of the edge's endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct RelationView {
    /// The edge's record id.
    pub id: String,
    /// The record id of the entity the edge starts at.
    #[serde(rename = "in")]
    pub from_id: String,
    /// The record id of the entity the edge ends at.
    pub out: String,
    /// The relation's type — the dynamic table the edge lives in.
    pub relation_type: String,
    /// How sure the extractor is, 0–1.
    pub confidence: f32,
    /// Whatever else the extractor recorded.
    pub metadata: serde_json::Value,
}

impl From<&PortRelationView> for RelationView {
    fn from(relation: &PortRelationView) -> Self {
        Self {
            id: relation.id.clone(),
            from_id: relation.from_id.clone(),
            out: relation.out.clone(),
            relation_type: relation.relation_type.clone(),
            confidence: relation.confidence,
            metadata: relation.metadata.clone(),
        }
    }
}

// ─── Retrieval provenance ───────────────────────────────────────────────────

/// `kb_ports::ProvenanceArm` — the arms that produced a search hit.
///
/// The wire vocabulary, not the store's: `dense` and `sparse` are the two
/// scored arms, the rest are the engine phases that add a chunk.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceArm {
    /// The dense/vector arm.
    Dense,
    /// The sparse/keyword arm (BM25).
    Sparse,
    /// Woven in by the neighbour expansion: adjacent chunks of a hit's
    /// document, index ±1.
    Neighbour,
    /// Retrieved as a figure co-retrieval of a hit's document.
    Figure,
    /// Ranked by the entity graph.
    Graph,
    /// Brought in by the page-image arm.
    Page,
    /// Brought in by the tree arm (`DEC-4`).
    Tree,
    /// Brought in by the global (graph community) arm (`DEC-5`).
    Global,
}

impl From<&PortProvenanceArm> for ProvenanceArm {
    fn from(arm: &PortProvenanceArm) -> Self {
        match arm {
            PortProvenanceArm::Dense => Self::Dense,
            PortProvenanceArm::Sparse => Self::Sparse,
            PortProvenanceArm::Neighbour => Self::Neighbour,
            PortProvenanceArm::Figure => Self::Figure,
            PortProvenanceArm::Graph => Self::Graph,
            PortProvenanceArm::Page => Self::Page,
            PortProvenanceArm::Tree => Self::Tree,
            PortProvenanceArm::Global => Self::Global,
        }
    }
}

// ─── Tables ─────────────────────────────────────────────────────────────────

/// `kb_core::tables::TableManifest` — what the manifest's own reading of a
/// table can be trusted to mean.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct TableManifest {
    /// One entry per column, in the table's own order.
    pub columns: Vec<ColumnInfo>,
    /// 0-based rows that are derived (subtotals, totals).
    pub derived_rows: Vec<u32>,
    /// True when the derived rows are proven by formulas rather than guessed.
    pub formulas_known: bool,
    /// How far the manifest can be trusted.
    pub confidence: Confidence,
    /// How many data rows the table has.
    pub n_rows: u32,
    /// How many columns the table has.
    pub n_cols: u32,
    /// True when rows were dropped to fit the extractor's row cap.
    pub truncated: bool,
    /// Extraction notes carried forward ("hidden sheet skipped", …).
    pub notes: Vec<String>,
}

impl From<&kb_core::tables::TableManifest> for TableManifest {
    fn from(manifest: &kb_core::tables::TableManifest) -> Self {
        Self {
            columns: manifest.columns.iter().map(ColumnInfo::from).collect(),
            derived_rows: manifest.derived_rows.clone(),
            formulas_known: manifest.formulas_known,
            confidence: Confidence::from(&manifest.confidence),
            n_rows: manifest.n_rows,
            n_cols: manifest.n_cols,
            truncated: manifest.truncated,
            notes: manifest.notes.clone(),
        }
    }
}

/// `kb_core::tables::ColumnInfo` — one column of a table's manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct ColumnInfo {
    /// The column's name.
    pub name: String,
    /// What the column holds.
    pub kind: ColumnKind,
    /// What the column means, which decides what may be computed over it.
    pub role: ColumnRole,
    /// The unit as written (`Rp juta`, `%`, `ton`); never applied to values.
    pub unit: Option<String>,
    /// Up to ten frequent distinct values, for non-quantity columns.
    pub sample_values: Vec<String>,
    /// How many distinct values the column has.
    pub distinct_count: u32,
}

impl From<&kb_core::tables::ColumnInfo> for ColumnInfo {
    fn from(column: &kb_core::tables::ColumnInfo) -> Self {
        Self {
            name: column.name.clone(),
            kind: ColumnKind::from(&column.kind),
            role: ColumnRole::from(&column.role),
            unit: column.unit.clone(),
            sample_values: column.sample_values.clone(),
            distinct_count: column.distinct_count,
        }
    }
}

/// `kb_core::tables::ColumnKind` — what a column holds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ColumnKind {
    /// Numbers.
    Number,
    /// Text.
    Text,
    /// A date or a period.
    Date,
    /// True/false.
    Bool,
    /// More than one kind in the same column.
    Mixed,
    /// Nothing in the column.
    Empty,
}

impl From<&kb_core::tables::ColumnKind> for ColumnKind {
    fn from(kind: &kb_core::tables::ColumnKind) -> Self {
        match kind {
            kb_core::tables::ColumnKind::Number => Self::Number,
            kb_core::tables::ColumnKind::Text => Self::Text,
            kb_core::tables::ColumnKind::Date => Self::Date,
            kb_core::tables::ColumnKind::Bool => Self::Bool,
            kb_core::tables::ColumnKind::Mixed => Self::Mixed,
            kb_core::tables::ColumnKind::Empty => Self::Empty,
        }
    }
}

/// `kb_core::tables::ColumnRole` — what a column *means*.
///
/// A phone number, a postcode or a year is numeric but never summed, so the
/// role decides what an aggregate may do with the column.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
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
    /// A date or a period.
    Date,
}

impl From<&kb_core::tables::ColumnRole> for ColumnRole {
    fn from(role: &kb_core::tables::ColumnRole) -> Self {
        match role {
            kb_core::tables::ColumnRole::Quantity => Self::Quantity,
            kb_core::tables::ColumnRole::Code => Self::Code,
            kb_core::tables::ColumnRole::Identifier => Self::Identifier,
            kb_core::tables::ColumnRole::Label => Self::Label,
            kb_core::tables::ColumnRole::Date => Self::Date,
        }
    }
}

/// `kb_core::tables::Confidence` — how far the manifest's reading of the table
/// can be trusted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Formulas known (xlsx/ods) or a clean flat grid.
    High,
    /// Derived rows guessed from labels ("Total", "Jumlah").
    Medium,
    /// Structure guessed (ragged rows, OCR'd table, stitched pages).
    Low,
}

impl From<&kb_core::tables::Confidence> for Confidence {
    fn from(confidence: &kb_core::tables::Confidence) -> Self {
        match confidence {
            kb_core::tables::Confidence::High => Self::High,
            kb_core::tables::Confidence::Medium => Self::Medium,
            kb_core::tables::Confidence::Low => Self::Low,
        }
    }
}

// ─── The typed table query ──────────────────────────────────────────────────

/// `kb_core::tables::TableQuery` — the typed query
/// `POST /v1/tables/{id}/query` accepts. Never SQL: columns are checked
/// against the manifest and values are bound as parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, ToSchema)]
pub struct TableQuery {
    /// The output columns, plain or aggregated.
    pub select: Vec<SelectItem>,
    /// The rows to keep.
    pub filter: Vec<Filter>,
    /// The columns to group by.
    pub group_by: Vec<String>,
    /// The output order.
    pub order_by: Vec<OrderBy>,
    /// The maximum number of rows to return.
    pub limit: Option<u32>,
    /// Count subtotal/total rows too (off by default: they double-count).
    pub include_derived: bool,
}

impl From<&kb_core::tables::TableQuery> for TableQuery {
    fn from(query: &kb_core::tables::TableQuery) -> Self {
        Self {
            select: query.select.iter().map(SelectItem::from).collect(),
            filter: query.filter.iter().map(Filter::from).collect(),
            group_by: query.group_by.clone(),
            order_by: query.order_by.iter().map(OrderBy::from).collect(),
            limit: query.limit,
            include_derived: query.include_derived,
        }
    }
}

/// `kb_core::tables::SelectItem` — one output column: a plain column, or an
/// aggregate.
///
/// JSON: `{"column":"Region"}` or `{"agg":"sum","column":"Revenue","as":"total"}`;
/// `{"agg":"count"}` with no column counts rows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(untagged)]
pub enum SelectItem {
    /// An aggregate over a column.
    Aggregate {
        /// The aggregate function.
        agg: Agg,
        /// The column to aggregate; absent for `count`.
        #[serde(default)]
        column: Option<String>,
        /// The output name for the aggregate.
        #[serde(default, rename = "as")]
        alias: Option<String>,
    },
    /// A plain column, which must be grouped or aggregated elsewhere.
    Column {
        /// The column's name.
        column: String,
        /// The output name for the column.
        #[serde(default, rename = "as")]
        alias: Option<String>,
    },
}

impl From<&kb_core::tables::SelectItem> for SelectItem {
    fn from(item: &kb_core::tables::SelectItem) -> Self {
        match item {
            kb_core::tables::SelectItem::Aggregate { agg, column, alias } => Self::Aggregate {
                agg: Agg::from(agg),
                column: column.clone(),
                alias: alias.clone(),
            },
            kb_core::tables::SelectItem::Column { column, alias } => Self::Column {
                column: column.clone(),
                alias: alias.clone(),
            },
        }
    }
}

/// `kb_core::tables::Agg` — the aggregate functions a query may use.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Agg {
    /// The sum of the column.
    Sum,
    /// The mean of the column.
    Avg,
    /// The smallest value.
    Min,
    /// The largest value.
    Max,
    /// The number of rows.
    Count,
    /// The number of distinct values.
    CountDistinct,
}

impl From<&kb_core::tables::Agg> for Agg {
    fn from(agg: &kb_core::tables::Agg) -> Self {
        match agg {
            kb_core::tables::Agg::Sum => Self::Sum,
            kb_core::tables::Agg::Avg => Self::Avg,
            kb_core::tables::Agg::Min => Self::Min,
            kb_core::tables::Agg::Max => Self::Max,
            kb_core::tables::Agg::Count => Self::Count,
            kb_core::tables::Agg::CountDistinct => Self::CountDistinct,
        }
    }
}

/// `kb_core::tables::Filter` — one predicate on a column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct Filter {
    /// The column the predicate is on.
    pub column: String,
    /// The comparison.
    pub op: FilterOp,
    /// The value to compare against; an array for `in`, `[low, high]` for
    /// `between`.
    pub value: serde_json::Value,
}

impl From<&kb_core::tables::Filter> for Filter {
    fn from(filter: &kb_core::tables::Filter) -> Self {
        Self {
            column: filter.column.clone(),
            op: FilterOp::from(&filter.op),
            value: filter.value.clone(),
        }
    }
}

/// `kb_core::tables::FilterOp` — the comparison a filter makes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub enum FilterOp {
    /// Equal.
    #[serde(rename = "=")]
    Eq,
    /// Not equal.
    #[serde(rename = "!=")]
    Ne,
    /// Greater than.
    #[serde(rename = ">")]
    Gt,
    /// Greater than or equal.
    #[serde(rename = ">=")]
    Ge,
    /// Less than.
    #[serde(rename = "<")]
    Lt,
    /// Less than or equal.
    #[serde(rename = "<=")]
    Le,
    /// Case-insensitive substring on the text form.
    #[serde(rename = "contains")]
    Contains,
    /// One of an array of values.
    #[serde(rename = "in")]
    In,
    /// Within an inclusive `[low, high]` range.
    #[serde(rename = "between")]
    Between,
}

impl From<&kb_core::tables::FilterOp> for FilterOp {
    fn from(op: &kb_core::tables::FilterOp) -> Self {
        match op {
            kb_core::tables::FilterOp::Eq => Self::Eq,
            kb_core::tables::FilterOp::Ne => Self::Ne,
            kb_core::tables::FilterOp::Gt => Self::Gt,
            kb_core::tables::FilterOp::Ge => Self::Ge,
            kb_core::tables::FilterOp::Lt => Self::Lt,
            kb_core::tables::FilterOp::Le => Self::Le,
            kb_core::tables::FilterOp::Contains => Self::Contains,
            kb_core::tables::FilterOp::In => Self::In,
            kb_core::tables::FilterOp::Between => Self::Between,
        }
    }
}

/// `kb_core::tables::OrderBy` — one output ordering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct OrderBy {
    /// An output column name or alias.
    pub key: String,
    /// Which way to sort.
    pub dir: SortDir,
}

impl From<&kb_core::tables::OrderBy> for OrderBy {
    fn from(order: &kb_core::tables::OrderBy) -> Self {
        Self {
            key: order.key.clone(),
            dir: SortDir::from(&order.dir),
        }
    }
}

/// `kb_core::tables::SortDir` — which way to sort.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

impl From<&kb_core::tables::SortDir> for SortDir {
    fn from(dir: &kb_core::tables::SortDir) -> Self {
        match dir {
            kb_core::tables::SortDir::Asc => Self::Asc,
            kb_core::tables::SortDir::Desc => Self::Desc,
        }
    }
}

// ─── The query result ───────────────────────────────────────────────────────

/// `kb_core::tables::QueryResult` — the rows a query returned, and the receipt
/// that lets a caller say exactly how the number was computed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct QueryResult {
    /// The output columns, in order.
    pub columns: Vec<String>,
    /// The rows, each with one value per output column.
    pub rows: Vec<Vec<serde_json::Value>>,
    /// The rows the query matched before grouping.
    pub rows_matched: u64,
    /// The derived rows left out of the computation.
    pub excluded_derived_rows: u32,
    /// The groups hidden by the document's privacy policy.
    pub suppressed_groups: u32,
    /// True when the row cap or the query's own limit cut the output.
    pub truncated: bool,
    /// Plain-language caveats about how the number was computed.
    pub warnings: Vec<String>,
}

impl From<&kb_core::tables::QueryResult> for QueryResult {
    fn from(result: &kb_core::tables::QueryResult) -> Self {
        Self {
            columns: result.columns.clone(),
            rows: result.rows.clone(),
            rows_matched: result.rows_matched,
            excluded_derived_rows: result.excluded_derived_rows,
            suppressed_groups: result.suppressed_groups,
            truncated: result.truncated,
            warnings: result.warnings.clone(),
        }
    }
}

/// `kb_core::tables::QueryError` — why a query was refused before it ran.
///
/// Every variant is a `400` at the API, and the JSON is the body, so a client
/// can react to `unknown_column` (and read `available`) instead of parsing a
/// prose message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum QueryError {
    /// The query named a column the table does not have.
    UnknownColumn {
        /// The column the query named.
        column: String,
        /// The columns the table does have.
        available: Vec<String>,
    },
    /// SUM/AVG over a column whose role is not Quantity.
    NotSummable {
        /// The column the query named.
        column: String,
        /// The column's role, which is why it was refused.
        role: ColumnRole,
    },
    /// A plain column in `select` that is neither grouped nor aggregated.
    NotGrouped {
        /// The column the query named.
        column: String,
    },
    /// The document's privacy policy forbids this (plain rows, MIN/MAX).
    Restricted {
        /// Why the query was refused.
        reason: String,
    },
    /// A value could not be compared with the column's kind.
    BadValue {
        /// The column the value was for.
        column: String,
        /// Why the value was refused.
        reason: String,
    },
    /// The query matched nothing.
    Empty {
        /// Why the query was refused.
        reason: String,
    },
}

impl From<&kb_core::tables::QueryError> for QueryError {
    fn from(error: &kb_core::tables::QueryError) -> Self {
        match error {
            kb_core::tables::QueryError::UnknownColumn { column, available } => {
                Self::UnknownColumn {
                    column: column.clone(),
                    available: available.clone(),
                }
            }
            kb_core::tables::QueryError::NotSummable { column, role } => Self::NotSummable {
                column: column.clone(),
                role: ColumnRole::from(role),
            },
            kb_core::tables::QueryError::NotGrouped { column } => Self::NotGrouped {
                column: column.clone(),
            },
            kb_core::tables::QueryError::Restricted { reason } => Self::Restricted {
                reason: reason.clone(),
            },
            kb_core::tables::QueryError::BadValue { column, reason } => Self::BadValue {
                column: column.clone(),
                reason: reason.clone(),
            },
            kb_core::tables::QueryError::Empty { reason } => Self::Empty {
                reason: reason.clone(),
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    //! The mirror's whole reason to exist: it must serialise exactly like the
    //! type it mirrors. A mirror that drifts is worse than no mirror, because
    //! it makes the published spec wrong in a way no client can detect.

    use super::*;
    use kb_ports::{RetrievalProvenance, SearchHit};

    /// Serialise the real type and its mirror and compare the two documents.
    fn same<T, M>(real: &T, mirror: &M)
    where
        T: Serialize,
        M: Serialize,
    {
        let real = serde_json::to_value(real).unwrap();
        let mirror = serde_json::to_value(mirror).unwrap();
        assert_eq!(real, mirror);
    }

    #[test]
    fn a_job_mirrors_its_port_type() {
        let job = PortJobView {
            id: "job-1".to_string(),
            status: "processing".to_string(),
            step: Some("extract".to_string()),
            progress: 40,
            step_current: Some(2),
            step_total: Some(5),
            eta_seconds: Some(12),
            attempt: 1,
            error: None,
            document_id: Some("doc-1".to_string()),
        };
        let mirror = JobView::from(&job);
        same(&job, &mirror);
        // The wire names are camelCase, like every other `/v1` shape.
        let value = serde_json::to_value(&mirror).unwrap();
        assert_eq!(value["stepCurrent"], 2);
        assert_eq!(value["etaSeconds"], 12);
        assert_eq!(value["documentId"], "doc-1");
    }

    #[test]
    fn an_entity_mirrors_its_port_type() {
        let entity = PortEntityView {
            id: "ent-1".to_string(),
            name: "Acme".to_string(),
            r#type: "organisation".to_string(),
            confidence: 0.9,
            metadata: serde_json::json!({"source": "text"}),
        };
        let mirror = EntityView::from(&entity);
        same(&entity, &mirror);
        assert_eq!(
            serde_json::to_value(&mirror).unwrap()["type"],
            "organisation"
        );
    }

    #[test]
    fn a_relation_mirrors_its_port_type() {
        let relation = PortRelationView {
            id: "rel-1".to_string(),
            from_id: "ent-1".to_string(),
            out: "ent-2".to_string(),
            relation_type: "supplies".to_string(),
            confidence: 0.8,
            metadata: serde_json::json!({}),
        };
        let mirror = RelationView::from(&relation);
        same(&relation, &mirror);
        let value = serde_json::to_value(&mirror).unwrap();
        assert_eq!(value["in"], "ent-1");
        assert_eq!(value["relation_type"], "supplies");
    }

    #[test]
    fn an_arm_mirrors_its_port_type() {
        for arm in [
            PortProvenanceArm::Dense,
            PortProvenanceArm::Sparse,
            PortProvenanceArm::Neighbour,
            PortProvenanceArm::Figure,
            PortProvenanceArm::Graph,
            PortProvenanceArm::Page,
            PortProvenanceArm::Tree,
            PortProvenanceArm::Global,
        ] {
            same(&arm, &ProvenanceArm::from(&arm));
        }
    }

    #[test]
    fn a_manifest_mirrors_its_core_type() {
        let manifest = kb_core::tables::TableManifest {
            columns: vec![kb_core::tables::ColumnInfo {
                name: "Revenue".to_string(),
                kind: kb_core::tables::ColumnKind::Number,
                role: kb_core::tables::ColumnRole::Quantity,
                unit: Some("Rp juta".to_string()),
                sample_values: vec!["1".to_string()],
                distinct_count: 3,
            }],
            derived_rows: vec![2],
            formulas_known: true,
            confidence: kb_core::tables::Confidence::High,
            n_rows: 4,
            n_cols: 1,
            truncated: false,
            notes: vec!["hidden sheet skipped".to_string()],
        };
        let mirror = TableManifest::from(&manifest);
        same(&manifest, &mirror);
        let value = serde_json::to_value(&mirror).unwrap();
        assert_eq!(value["columns"][0]["kind"], "number");
        assert_eq!(value["columns"][0]["role"], "quantity");
        assert_eq!(value["confidence"], "high");
        assert_eq!(value["derived_rows"], serde_json::json!([2]));
    }

    #[test]
    fn a_query_mirrors_its_core_type() {
        let query = kb_core::tables::TableQuery {
            select: vec![
                kb_core::tables::SelectItem::Column {
                    column: "Region".to_string(),
                    alias: Some("region".to_string()),
                },
                kb_core::tables::SelectItem::Aggregate {
                    agg: kb_core::tables::Agg::Sum,
                    column: Some("Revenue".to_string()),
                    alias: Some("total".to_string()),
                },
            ],
            filter: vec![kb_core::tables::Filter {
                column: "Revenue".to_string(),
                op: kb_core::tables::FilterOp::Ge,
                value: serde_json::json!(10),
            }],
            group_by: vec!["Region".to_string()],
            order_by: vec![kb_core::tables::OrderBy {
                key: "total".to_string(),
                dir: kb_core::tables::SortDir::Desc,
            }],
            limit: Some(10),
            include_derived: false,
        };
        let mirror = TableQuery::from(&query);
        same(&query, &mirror);
        let value = serde_json::to_value(&mirror).unwrap();
        assert_eq!(value["select"][0]["column"], "Region");
        assert_eq!(value["select"][1]["agg"], "sum");
        assert_eq!(value["select"][1]["as"], "total");
        assert_eq!(value["filter"][0]["op"], ">=");
        assert_eq!(value["order_by"][0]["dir"], "desc");
    }

    #[test]
    fn a_query_result_mirrors_its_core_type() {
        let result = kb_core::tables::QueryResult {
            columns: vec!["Region".to_string(), "total".to_string()],
            rows: vec![vec![serde_json::json!("East"), serde_json::json!(42)]],
            rows_matched: 12,
            excluded_derived_rows: 1,
            suppressed_groups: 0,
            truncated: false,
            warnings: vec!["subtotal rows were guessed".to_string()],
        };
        let mirror = QueryResult::from(&result);
        same(&result, &mirror);
    }

    #[test]
    fn every_query_error_mirrors_its_core_type() {
        let errors = [
            kb_core::tables::QueryError::UnknownColumn {
                column: "Revneue".to_string(),
                available: vec!["Revenue".to_string()],
            },
            kb_core::tables::QueryError::NotSummable {
                column: "Phone".to_string(),
                role: kb_core::tables::ColumnRole::Code,
            },
            kb_core::tables::QueryError::NotGrouped {
                column: "Region".to_string(),
            },
            kb_core::tables::QueryError::Restricted {
                reason: "plain rows are not allowed".to_string(),
            },
            kb_core::tables::QueryError::BadValue {
                column: "Revenue".to_string(),
                reason: "not a number".to_string(),
            },
            kb_core::tables::QueryError::Empty {
                reason: "no rows".to_string(),
            },
        ];
        for error in &errors {
            let mirror = QueryError::from(error);
            same(error, &mirror);
        }
        let value = serde_json::to_value(QueryError::from(&errors[0])).unwrap();
        assert_eq!(value["error"], "unknown_column");
        assert_eq!(value["available"], serde_json::json!(["Revenue"]));
    }

    #[test]
    fn the_provenance_arms_of_a_search_hit_use_the_mirrored_spelling() {
        // The one place a mirror is not a straight copy: the arms travel
        // through the DTO, so the mirror's spelling has to be the port's.
        let hit = SearchHit {
            chunk: kb_ports::Chunk {
                id: kb_ports::ChunkId("c-1".to_string()),
                document_id: kb_ports::DocumentId("d-1".to_string()),
                document_title: "Doc".to_string(),
                external_ref: None,
                content: "body".to_string(),
                chunk_type: kb_ports::ChunkType::Text,
                metadata: kb_ports::ChunkMetadata::default(),
                categories: vec![],
                subcategory: None,
            },
            similarity: 0.5,
            rank: 0,
            is_bm25: false,
            retrieval: Some(RetrievalProvenance {
                arms: vec![
                    PortProvenanceArm::Figure,
                    PortProvenanceArm::Dense,
                    PortProvenanceArm::Neighbour,
                ],
                dense_rank: Some(0),
                sparse_rank: None,
                graph_rank: None,
                fused_score: Some(0.5),
            }),
            dense_score: Some(0.5),
            rerank_score: None,
        };
        let real: Vec<ProvenanceArm> = hit
            .retrieval
            .as_ref()
            .unwrap()
            .arms
            .iter()
            .map(ProvenanceArm::from)
            .collect();
        let port: Vec<PortProvenanceArm> = hit.retrieval.unwrap().arms;
        same(&port, &real);
    }
}
