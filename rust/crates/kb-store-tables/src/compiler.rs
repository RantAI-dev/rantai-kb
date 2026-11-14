//! Validate a typed [`TableQuery`] against a manifest and compile it to SQL.
//!
//! This is the security boundary of the table feature, so it is a pure module
//! with no I/O and no database:
//!
//! * **Every identifier** in the emitted SQL comes from the manifest's column
//!   whitelist, matched by exact string equality and double-quoted (with `"`
//!   escaped), never from the caller's text. A column name the manifest does
//!   not have is a [`QueryError::UnknownColumn`], whether the caller meant it
//!   as a column or as an output alias.
//! * **Every value** is a [`Param`], bound by the DuckDB driver. A filter value
//!   is never interpolated, so `"x' OR 1=1 --"` is one string parameter.
//! * **Every aggregate** is checked against the manifest: SUM/AVG demand a
//!   `Quantity` column, and an `aggregate_only` policy forbids plain rows,
//!   grouping and MIN/MAX.
//!
//! Derived rows (subtotals) are excluded in SQL unless `include_derived`, so a
//! caller cannot double-count them by accident.

use std::collections::BTreeMap;

use kb_core::tables::{
    Agg, ColumnInfo, ColumnKind, ColumnRole, Filter, FilterOp, MAX_ROWS_RETURNED, QueryError,
    QueryPolicy, SelectItem, TableManifest, TableQuery,
};
use serde_json::Value;

/// The table function's local Parquet path, as a bind.
///
/// Kept as a distinct variant so the store can substitute the downloaded file
/// path while the rest of the compiler stays free of filesystem knowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridParam;

/// One bound value. Deliberately a closed set of scalar shapes: a caller
/// cannot smuggle SQL through it.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    /// The local Parquet path.
    Grid(GridParam),
    Text(String),
    Number(f64),
    Bool(bool),
    Null,
}

/// A compiled query: the main statement, its binds, and the companion
/// statements the receipt needs.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledQuery {
    /// `SELECT … FROM read_parquet(?) WHERE … GROUP BY … ORDER BY … LIMIT …`.
    pub main: String,
    /// Binds for `main` (and every companion), in order.
    pub binds: Vec<Param>,
    /// Output column names, in select order.
    pub columns: Vec<String>,
    /// Positions in `columns` that are group keys (empty when ungrouped).
    pub group_positions: Vec<usize>,
    /// `SELECT count(*) …` over the same filter, derived rows excluded as in
    /// the main query. Not subject to the limit.
    pub rows_matched: String,
    /// `SELECT count(*) …` over the filter with only derived rows, when the
    /// main query excludes them.
    pub derived_matched: Option<String>,
    /// `SELECT <group keys>, count(*) … GROUP BY <group keys>`: the group sizes
    /// the privacy suppression needs. `None` when the query is ungrouped.
    pub group_counts: Option<String>,
    /// The effective row limit (after `MAX_ROWS_RETURNED`).
    pub limit: u32,
    /// The DB fetches one more than `limit`, so "there was more" is knowable.
    pub fetch: u32,
    /// Whether the query groups.
    pub grouped: bool,
}

/// Compile `query` against `manifest`.
///
/// # Errors
///
/// Any [`QueryError`] the query deserves: unknown columns, a SUM/AVG over a
/// column that is not a quantity, a plain column that is neither grouped nor
/// aggregated, a policy restriction, or a value the column cannot compare to.
pub fn compile(
    manifest: &TableManifest,
    query: &TableQuery,
    policy: &QueryPolicy,
) -> Result<CompiledQuery, QueryError> {
    let columns = Columns::of(manifest);
    if query.select.is_empty() {
        return Err(QueryError::Empty {
            reason: "select at least one column or aggregate".to_string(),
        });
    }
    if policy.aggregate_only && !query.group_by.is_empty() {
        return Err(QueryError::Restricted {
            reason: "this table only answers aggregates: groupBy is not allowed".to_string(),
        });
    }

    let mut select_sql: Vec<String> = Vec::with_capacity(query.select.len());
    let mut output_names: Vec<String> = Vec::with_capacity(query.select.len());
    // Where each selected plain column landed, by exact manifest name. The
    // group positions are read out of this in `group_by` order, which is the
    // order the group-count statement returns its keys in.
    let mut selected_columns: BTreeMap<String, usize> = BTreeMap::new();
    for item in &query.select {
        match item {
            SelectItem::Column { column, alias } => {
                let info = columns.get(column)?;
                if policy.aggregate_only {
                    return Err(QueryError::Restricted {
                        reason: format!(
                            "this table only answers aggregates: plain column '{column}' is not allowed"
                        ),
                    });
                }
                if !query.group_by.is_empty() && !query.group_by.iter().any(|g| g == column) {
                    return Err(QueryError::NotGrouped {
                        column: column.clone(),
                    });
                }
                let name = alias.clone().unwrap_or_else(|| info.name.clone());
                ensure_unique(&output_names, &name)?;
                select_sql.push(quote(&info.name));
                output_names.push(name);
                selected_columns.insert(info.name.clone(), output_names.len() - 1);
            }
            SelectItem::Aggregate { agg, column, alias } => {
                let expression = aggregate_sql(*agg, column.as_deref(), &columns, policy)?;
                let name = alias.clone().unwrap_or_else(|| {
                    column.as_deref().map_or_else(
                        || agg_sql(*agg).to_string(),
                        |c| format!("{}({c})", agg_sql(*agg)),
                    )
                });
                ensure_unique(&output_names, &name)?;
                select_sql.push(expression);
                output_names.push(name);
            }
        }
    }

    // Every group_by column must be one of the output columns: a group key the
    // caller cannot see is a group key they cannot cite. And every grouped
    // plain column is already checked above, so the two sets agree.
    let mut group_positions: Vec<usize> = Vec::with_capacity(query.group_by.len());
    for group in &query.group_by {
        let info = columns.get(group)?;
        match selected_columns.get(&info.name) {
            Some(position) => group_positions.push(*position),
            None => {
                return Err(QueryError::BadValue {
                    column: group.clone(),
                    reason: "a groupBy column must also appear in select".to_string(),
                });
            }
        }
    }

    // The caller's predicates, in bind order. Every statement below uses this
    // same list and this same bind order, which is why the companions need no
    // separate parameter bookkeeping.
    let mut caller_parts: Vec<String> = Vec::new();
    let mut binds: Vec<Param> = vec![Param::Grid(GridParam)];
    for filter in &query.filter {
        let info = columns.get(&filter.column)?;
        let (sql, mut values) = filter_sql(info, filter)?;
        caller_parts.push(sql);
        binds.append(&mut values);
    }
    let excludes_derived = !query.include_derived && !manifest.derived_rows.is_empty();
    let mut where_parts = caller_parts.clone();
    if excludes_derived {
        where_parts.insert(0, "(\"_derived\" = FALSE)".to_string());
    }
    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };

    let mut main = format!(
        "SELECT {} FROM read_parquet(?){}",
        join_raw(&select_sql),
        where_sql
    );
    if !query.group_by.is_empty() {
        let keys: Vec<String> = query
            .group_by
            .iter()
            .map(|g| columns.get(g).map(|info| quote(&info.name)))
            .collect::<Result<_, _>>()?;
        main.push_str(&format!(" GROUP BY {}", join_raw(&keys)));
    }
    if !query.order_by.is_empty() {
        let mut order: Vec<String> = Vec::with_capacity(query.order_by.len());
        for order_by in &query.order_by {
            let expression = output_expression(&query.select, &output_names, &order_by.key)?;
            order.push(format!(
                "{expression} {}",
                if order_by.dir == kb_core::tables::SortDir::Desc {
                    "DESC"
                } else {
                    "ASC"
                }
            ));
        }
        main.push_str(&format!(" ORDER BY {}", join_raw(&order)));
    }
    let limit = query
        .limit
        .unwrap_or(MAX_ROWS_RETURNED)
        .clamp(1, MAX_ROWS_RETURNED);
    let fetch = limit.saturating_add(1);
    main.push_str(&format!(" LIMIT {fetch}"));

    let rows_matched = format!("SELECT count(*) FROM read_parquet(?){where_sql}");

    let derived_matched = if excludes_derived {
        let mut parts = vec!["(\"_derived\" = TRUE)".to_string()];
        parts.extend(caller_parts.clone());
        Some(format!(
            "SELECT count(*) FROM read_parquet(?) WHERE {}",
            parts.join(" AND ")
        ))
    } else {
        None
    };

    let group_counts = if query.group_by.is_empty() {
        None
    } else {
        let mut parts = where_parts.clone();
        parts.push("(\"_row\" >= 0)".to_string());
        let keys: Vec<String> = query
            .group_by
            .iter()
            .map(|g| columns.get(g).map(|info| quote(&info.name)))
            .collect::<Result<_, _>>()?;
        Some(format!(
            "SELECT {}, count(*) FROM read_parquet(?) WHERE {} GROUP BY {}",
            join_raw(&keys),
            parts.join(" AND "),
            join_raw(&keys)
        ))
    };

    // The companions share the main statement's binds (path + filters), which
    // is exactly why filters never carry an inline value.
    Ok(CompiledQuery {
        main,
        binds,
        columns: output_names,
        group_positions,
        rows_matched,
        derived_matched,
        group_counts,
        limit,
        fetch,
        grouped: !query.group_by.is_empty(),
    })
}

/// The manifest's columns, addressable by exact name.
struct Columns<'a> {
    by_name: BTreeMap<&'a str, &'a ColumnInfo>,
    manifest: &'a TableManifest,
}

impl<'a> Columns<'a> {
    fn of(manifest: &'a TableManifest) -> Self {
        Self {
            by_name: manifest
                .columns
                .iter()
                .map(|c| (c.name.as_str(), c))
                .collect(),
            manifest,
        }
    }

    fn get(&self, name: &str) -> Result<&'a ColumnInfo, QueryError> {
        self.by_name
            .get(name)
            .copied()
            .ok_or_else(|| QueryError::UnknownColumn {
                column: name.to_string(),
                available: self
                    .manifest
                    .columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect(),
            })
    }
}

fn aggregate_sql(
    agg: Agg,
    column: Option<&str>,
    columns: &Columns<'_>,
    policy: &QueryPolicy,
) -> Result<String, QueryError> {
    let Some(column) = column else {
        return match agg {
            Agg::Count => Ok("count(*)".to_string()),
            _ => Err(QueryError::Empty {
                reason: format!("{} needs a column", agg_sql(agg).to_uppercase()),
            }),
        };
    };
    let info = columns.get(column)?;
    match agg {
        Agg::Sum | Agg::Avg => {
            if info.role != ColumnRole::Quantity {
                return Err(QueryError::NotSummable {
                    column: info.name.clone(),
                    role: info.role,
                });
            }
            Ok(format!("{}({})", agg_sql(agg), quote(&info.name)))
        }
        Agg::Min | Agg::Max => {
            if policy.aggregate_only {
                return Err(QueryError::Restricted {
                    reason: format!(
                        "this table only answers aggregates: {} on '{}' could identify one row",
                        agg_sql(agg).to_uppercase(),
                        info.name
                    ),
                });
            }
            Ok(format!("{}({})", agg_sql(agg), quote(&info.name)))
        }
        Agg::Count => Ok(format!("count({})", quote(&info.name))),
        Agg::CountDistinct => Ok(format!("count(DISTINCT {})", quote(&info.name))),
    }
}

/// The SQL function name for an aggregate. A free function because `Agg` is
/// `kb-core`'s type and an inherent impl here would be an orphan.
fn agg_sql(agg: Agg) -> &'static str {
    match agg {
        Agg::Sum => "sum",
        Agg::Avg => "avg",
        Agg::Min => "min",
        Agg::Max => "max",
        Agg::Count => "count",
        Agg::CountDistinct => "count_distinct",
    }
}

/// The SQL for one filter, and its binds.
fn filter_sql(info: &ColumnInfo, filter: &Filter) -> Result<(String, Vec<Param>), QueryError> {
    let name = &info.name;
    let quoted = quote(name);
    match filter.op {
        FilterOp::Contains => {
            let value = scalar_text(info, &filter.value)?;
            Ok((
                format!("(lower(CAST({quoted} AS VARCHAR)) LIKE '%' || lower(?) || '%')"),
                vec![Param::Text(value)],
            ))
        }
        FilterOp::In => {
            let Value::Array(items) = &filter.value else {
                return Err(bad_value(name, "op 'in' needs an array of values"));
            };
            if items.is_empty() {
                // An empty `in` matches nothing rather than everything: a
                // caller who sends it asked for no row.
                return Ok(("(FALSE)".to_string(), Vec::new()));
            }
            let mut params = Vec::with_capacity(items.len());
            for item in items {
                params.push(scalar(info, item)?);
            }
            let placeholders = vec!["?"; params.len()].join(", ");
            Ok((format!("({quoted} IN ({placeholders}))"), params))
        }
        FilterOp::Between => {
            let Value::Array(items) = &filter.value else {
                return Err(bad_value(name, "op 'between' needs [low, high]"));
            };
            if items.len() != 2 {
                return Err(bad_value(name, "op 'between' needs exactly [low, high]"));
            }
            let low = scalar(info, &items[0])?;
            let high = scalar(info, &items[1])?;
            Ok((format!("({quoted} BETWEEN ? AND ?)"), vec![low, high]))
        }
        FilterOp::Eq | FilterOp::Ne => {
            if filter.value.is_null() {
                let operator = if filter.op == FilterOp::Eq {
                    "IS NULL"
                } else {
                    "IS NOT NULL"
                };
                return Ok((format!("({quoted} {operator})"), Vec::new()));
            }
            let param = scalar(info, &filter.value)?;
            let operator = if filter.op == FilterOp::Eq { "=" } else { "<>" };
            Ok((format!("({quoted} {operator} ?)"), vec![param]))
        }
        FilterOp::Gt | FilterOp::Ge | FilterOp::Lt | FilterOp::Le => {
            let param = scalar(info, &filter.value)?;
            let operator = match filter.op {
                FilterOp::Ge => ">=",
                FilterOp::Lt => "<",
                FilterOp::Le => "<=",
                // The outer match only reaches this arm for these four, and
                // `Gt` is the default so an added future operator cannot
                // silently become ">=".
                FilterOp::Eq
                | FilterOp::Ne
                | FilterOp::Contains
                | FilterOp::In
                | FilterOp::Between
                | FilterOp::Gt => ">",
            };
            Ok((format!("({quoted} {operator} ?)"), vec![param]))
        }
    }
}

/// One scalar value, coerced for the column it is compared against.
fn scalar(info: &ColumnInfo, value: &Value) -> Result<Param, QueryError> {
    match value {
        Value::Null => Ok(Param::Null),
        Value::Bool(b) => match info.kind {
            ColumnKind::Number => Err(bad_value(
                &info.name,
                "a boolean cannot be compared with a numeric column",
            )),
            _ => Ok(Param::Text(b.to_string())),
        },
        Value::Number(n) => match info.kind {
            ColumnKind::Number => n
                .as_f64()
                .map(Param::Number)
                .ok_or_else(|| bad_value(&info.name, "number is not representable as a double")),
            _ => Ok(Param::Text(n.to_string())),
        },
        Value::String(s) => {
            if info.kind == ColumnKind::Number {
                s.trim()
                    .parse::<f64>()
                    .map(Param::Number)
                    .map_err(|_| bad_value(&info.name, "expected a number for this column"))
            } else {
                Ok(Param::Text(s.clone()))
            }
        }
        Value::Array(_) | Value::Object(_) => Err(bad_value(
            &info.name,
            "a filter value must be a string, number, boolean or null",
        )),
    }
}

/// The text form a `contains` comparison uses.
fn scalar_text(info: &ColumnInfo, value: &Value) -> Result<String, QueryError> {
    match scalar(info, value)? {
        Param::Text(s) => Ok(s),
        Param::Number(n) => Ok(n.to_string()),
        Param::Bool(b) => Ok(b.to_string()),
        Param::Null => Err(bad_value(&info.name, "contains cannot match null")),
        Param::Grid(_) => Err(bad_value(&info.name, "contains cannot match a grid path")),
    }
}

fn bad_value(column: &str, reason: &str) -> QueryError {
    QueryError::BadValue {
        column: column.to_string(),
        reason: reason.to_string(),
    }
}

fn ensure_unique(names: &[String], name: &str) -> Result<(), QueryError> {
    if names.iter().any(|n| n == name) {
        return Err(QueryError::BadValue {
            column: name.to_string(),
            reason: "duplicate output name: give one of them an 'as' alias".to_string(),
        });
    }
    Ok(())
}

/// The expression behind an output name, for ORDER BY. Only output names are
/// addressable, so an `ORDER BY` cannot name a column the caller did not select.
fn output_expression(
    select: &[SelectItem],
    names: &[String],
    key: &str,
) -> Result<String, QueryError> {
    let Some(index) = names.iter().position(|n| n == key) else {
        return Err(QueryError::UnknownColumn {
            column: key.to_string(),
            available: names.to_vec(),
        });
    };
    let expression = match select.get(index) {
        Some(SelectItem::Column { column, .. }) => quote(column),
        Some(SelectItem::Aggregate { agg, column, .. }) => match column {
            Some(c) => format!("{}({})", agg_sql(*agg), quote(c)),
            None => "count(*)".to_string(),
        },
        None => {
            return Err(QueryError::UnknownColumn {
                column: key.to_string(),
                available: names.to_vec(),
            });
        }
    };
    Ok(expression)
}

/// Quote an identifier that came from the manifest's whitelist.
fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn join_raw(parts: &[String]) -> String {
    parts.join(", ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use kb_core::tables::{ColumnInfo, ColumnKind, ColumnRole, Confidence, OrderBy, SortDir};

    fn manifest() -> TableManifest {
        TableManifest {
            columns: vec![
                col("Region", ColumnKind::Text, ColumnRole::Identifier),
                col("Revenue", ColumnKind::Number, ColumnRole::Quantity),
                col("Telepon", ColumnKind::Text, ColumnRole::Code),
            ],
            derived_rows: vec![3],
            formulas_known: false,
            confidence: Confidence::Medium,
            n_rows: 4,
            n_cols: 3,
            truncated: false,
            notes: Vec::new(),
        }
    }

    fn col(name: &str, kind: ColumnKind, role: ColumnRole) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            kind,
            role,
            unit: None,
            sample_values: Vec::new(),
            distinct_count: 0,
        }
    }

    fn open() -> QueryPolicy {
        QueryPolicy {
            aggregate_only: false,
            min_group: 0,
        }
    }

    fn sel(column: &str) -> SelectItem {
        SelectItem::Column {
            column: column.to_string(),
            alias: None,
        }
    }

    fn agg(agg: Agg, column: Option<&str>) -> SelectItem {
        SelectItem::Aggregate {
            agg,
            column: column.map(str::to_string),
            alias: None,
        }
    }

    #[test]
    fn a_sum_is_quoted_and_excludes_derived_rows() {
        let q = TableQuery {
            select: vec![sel("Region"), agg(Agg::Sum, Some("Revenue"))],
            group_by: vec!["Region".to_string()],
            ..TableQuery::default()
        };
        let compiled = compile(&manifest(), &q, &open()).unwrap();
        assert_eq!(
            compiled.main,
            "SELECT \"Region\", sum(\"Revenue\") FROM read_parquet(?) WHERE (\"_derived\" = FALSE) \
             GROUP BY \"Region\" LIMIT 1001"
        );
        assert_eq!(compiled.binds, vec![Param::Grid(GridParam)]);
        assert_eq!(compiled.columns, vec!["Region", "sum(Revenue)"]);
        assert_eq!(compiled.group_positions, vec![0]);
        assert!(compiled.derived_matched.is_some());
        assert!(compiled.group_counts.is_some());
    }

    #[test]
    fn include_derived_drops_the_exclusion_but_keeps_the_receipt_possible() {
        let q = TableQuery {
            select: vec![agg(Agg::Sum, Some("Revenue"))],
            include_derived: true,
            ..TableQuery::default()
        };
        let compiled = compile(&manifest(), &q, &open()).unwrap();
        assert!(!compiled.main.contains("_derived"));
        assert!(compiled.derived_matched.is_none());
    }

    #[test]
    fn unknown_columns_name_the_available_list() {
        let q = TableQuery {
            select: vec![sel("Profit")],
            ..TableQuery::default()
        };
        match compile(&manifest(), &q, &open()) {
            Err(QueryError::UnknownColumn { column, available }) => {
                assert_eq!(column, "Profit");
                assert_eq!(available, vec!["Region", "Revenue", "Telepon"]);
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn sum_over_a_code_or_text_column_is_refused() {
        for column in ["Telepon", "Region"] {
            let q = TableQuery {
                select: vec![agg(Agg::Sum, Some(column))],
                ..TableQuery::default()
            };
            match compile(&manifest(), &q, &open()) {
                Err(QueryError::NotSummable { column: c, role }) => {
                    assert_eq!(c, column);
                    assert_ne!(role, ColumnRole::Quantity);
                }
                other => panic!("expected NotSummable for {column}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_plain_column_beside_a_group_is_not_grouped() {
        let q = TableQuery {
            select: vec![sel("Region"), sel("Telepon")],
            group_by: vec!["Region".to_string()],
            ..TableQuery::default()
        };
        assert!(matches!(
            compile(&manifest(), &q, &open()),
            Err(QueryError::NotGrouped { column }) if column == "Telepon"
        ));
    }

    #[test]
    fn a_group_key_that_is_not_selected_is_refused() {
        let q = TableQuery {
            select: vec![agg(Agg::Sum, Some("Revenue"))],
            group_by: vec!["Region".to_string()],
            ..TableQuery::default()
        };
        assert!(matches!(
            compile(&manifest(), &q, &open()),
            Err(QueryError::BadValue { column, .. }) if column == "Region"
        ));
    }

    #[test]
    fn aggregate_only_forbids_plain_rows_grouping_and_min_max() {
        let policy = QueryPolicy {
            aggregate_only: true,
            min_group: 5,
        };
        let plain = TableQuery {
            select: vec![sel("Region")],
            ..TableQuery::default()
        };
        assert!(matches!(
            compile(&manifest(), &plain, &policy),
            Err(QueryError::Restricted { .. })
        ));
        let grouped = TableQuery {
            select: vec![sel("Region"), agg(Agg::Count, None)],
            group_by: vec!["Region".to_string()],
            ..TableQuery::default()
        };
        assert!(matches!(
            compile(&manifest(), &grouped, &policy),
            Err(QueryError::Restricted { .. })
        ));
        for function in [Agg::Min, Agg::Max] {
            let q = TableQuery {
                select: vec![agg(function, Some("Revenue"))],
                ..TableQuery::default()
            };
            assert!(matches!(
                compile(&manifest(), &q, &policy),
                Err(QueryError::Restricted { .. })
            ));
        }
        // A count over everything is exactly what the policy is for.
        let count = TableQuery {
            select: vec![agg(Agg::Count, None)],
            ..TableQuery::default()
        };
        assert!(compile(&manifest(), &count, &policy).is_ok());
    }

    #[test]
    fn values_are_bound_never_interpolated() {
        let hostile = "x' OR 1=1; DROP TABLE \"KbTable\"; --";
        let q = TableQuery {
            select: vec![sel("Region")],
            filter: vec![Filter {
                column: "Region".to_string(),
                op: FilterOp::Eq,
                value: Value::String(hostile.to_string()),
            }],
            ..TableQuery::default()
        };
        let compiled = compile(&manifest(), &q, &open()).unwrap();
        assert!(!compiled.main.contains(hostile), "{}", compiled.main);
        assert!(!compiled.main.contains("DROP"), "{}", compiled.main);
        assert_eq!(
            compiled.binds,
            vec![Param::Grid(GridParam), Param::Text(hostile.to_string())]
        );
    }

    #[test]
    fn a_manifest_column_name_cannot_break_out_of_its_quotes() {
        let mut m = manifest();
        m.columns[0].name = "Region\" ; DROP TABLE x; --".to_string();
        let q = TableQuery {
            select: vec![sel("Region\" ; DROP TABLE x; --")],
            ..TableQuery::default()
        };
        let compiled = compile(&m, &q, &open()).unwrap();
        // The identifier is quoted with the embedded quote doubled, so it
        // stays one identifier; the statement still has exactly one FROM.
        assert_eq!(compiled.main.matches("FROM read_parquet").count(), 1);
        assert!(
            compiled.main.contains("\"Region\"\" ; DROP TABLE x; --\""),
            "{}",
            compiled.main
        );
    }

    #[test]
    fn a_numeric_column_rejects_a_word() {
        let q = TableQuery {
            select: vec![agg(Agg::Sum, Some("Revenue"))],
            filter: vec![Filter {
                column: "Revenue".to_string(),
                op: FilterOp::Gt,
                value: Value::String("banyak".to_string()),
            }],
            ..TableQuery::default()
        };
        assert!(matches!(
            compile(&manifest(), &q, &open()),
            Err(QueryError::BadValue { column, .. }) if column == "Revenue"
        ));
    }

    #[test]
    fn contains_is_case_insensitive_and_parameterised() {
        let q = TableQuery {
            select: vec![sel("Region")],
            filter: vec![Filter {
                column: "Region".to_string(),
                op: FilterOp::Contains,
                value: Value::String("timur".to_string()),
            }],
            ..TableQuery::default()
        };
        let compiled = compile(&manifest(), &q, &open()).unwrap();
        assert!(compiled.main.contains("lower(CAST(\"Region\" AS VARCHAR))"));
        assert_eq!(compiled.binds[1], Param::Text("timur".to_string()));
    }

    #[test]
    fn in_and_between_expand_to_placeholders() {
        let q = TableQuery {
            select: vec![sel("Region")],
            filter: vec![
                Filter {
                    column: "Region".to_string(),
                    op: FilterOp::In,
                    value: serde_json::json!(["Bali", "Jawa Timur"]),
                },
                Filter {
                    column: "Revenue".to_string(),
                    op: FilterOp::Between,
                    value: serde_json::json!([1, 10.5]),
                },
            ],
            ..TableQuery::default()
        };
        let compiled = compile(&manifest(), &q, &open()).unwrap();
        assert!(compiled.main.contains("\"Region\" IN (?, ?)"));
        assert!(compiled.main.contains("\"Revenue\" BETWEEN ? AND ?"));
        assert_eq!(
            compiled.binds,
            vec![
                Param::Grid(GridParam),
                Param::Text("Bali".to_string()),
                Param::Text("Jawa Timur".to_string()),
                Param::Number(1.0),
                Param::Number(10.5),
            ]
        );
    }

    #[test]
    fn an_empty_in_matches_nothing_and_an_empty_select_is_refused() {
        let q = TableQuery {
            select: vec![sel("Region")],
            filter: vec![Filter {
                column: "Region".to_string(),
                op: FilterOp::In,
                value: serde_json::json!([]),
            }],
            ..TableQuery::default()
        };
        assert!(
            compile(&manifest(), &q, &open())
                .unwrap()
                .main
                .contains("(FALSE)")
        );
        assert!(matches!(
            compile(&manifest(), &TableQuery::default(), &open()),
            Err(QueryError::Empty { .. })
        ));
    }

    #[test]
    fn order_by_only_names_output_columns() {
        let q = TableQuery {
            select: vec![agg(Agg::Sum, Some("Revenue"))],
            order_by: vec![OrderBy {
                key: "Telepon".to_string(),
                dir: SortDir::Desc,
            }],
            ..TableQuery::default()
        };
        assert!(matches!(
            compile(&manifest(), &q, &open()),
            Err(QueryError::UnknownColumn { column, .. }) if column == "Telepon"
        ));

        let ok = TableQuery {
            select: vec![
                sel("Region"),
                SelectItem::Aggregate {
                    agg: Agg::Sum,
                    column: Some("Revenue".to_string()),
                    alias: Some("total".to_string()),
                },
            ],
            group_by: vec!["Region".to_string()],
            order_by: vec![OrderBy {
                key: "total".to_string(),
                dir: SortDir::Desc,
            }],
            ..TableQuery::default()
        };
        let compiled = compile(&manifest(), &ok, &open()).unwrap();
        assert!(
            compiled
                .main
                .ends_with("ORDER BY sum(\"Revenue\") DESC LIMIT 1001")
        );
    }

    #[test]
    fn limit_is_capped_at_the_contract_and_fetches_one_more() {
        let q = TableQuery {
            select: vec![sel("Region")],
            limit: Some(1_000_000),
            ..TableQuery::default()
        };
        let compiled = compile(&manifest(), &q, &open()).unwrap();
        assert_eq!(compiled.limit, MAX_ROWS_RETURNED);
        assert_eq!(compiled.fetch, MAX_ROWS_RETURNED + 1);

        let q = TableQuery {
            select: vec![sel("Region")],
            limit: Some(3),
            ..TableQuery::default()
        };
        let compiled = compile(&manifest(), &q, &open()).unwrap();
        assert_eq!(compiled.limit, 3);
        assert_eq!(compiled.fetch, 4);
    }

    #[test]
    fn a_duplicate_output_name_is_refused_rather_than_shadowed() {
        let q = TableQuery {
            select: vec![
                SelectItem::Aggregate {
                    agg: Agg::Sum,
                    column: Some("Revenue".to_string()),
                    alias: Some("x".to_string()),
                },
                SelectItem::Aggregate {
                    agg: Agg::Avg,
                    column: Some("Revenue".to_string()),
                    alias: Some("x".to_string()),
                },
            ],
            ..TableQuery::default()
        };
        assert!(matches!(
            compile(&manifest(), &q, &open()),
            Err(QueryError::BadValue { column, .. }) if column == "x"
        ));
    }
}
