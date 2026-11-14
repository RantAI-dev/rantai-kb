//! A placeholder manifest builder for `TBL-1` work package B.
//!
//! **TBL-1 A replaces this** with the real heuristics (roles, codes, dates,
//! units, sample values, derived rows from formulas and labels). Until that
//! lands, ingest still has to be able to call
//! `kb_core::tables::build_manifest` and get a manifest good enough to store a
//! grid and answer a query: kinds come from the cells, every numeric column is
//! a `Quantity`, nothing is derived and the confidence is `Low` because the
//! structure really was not read.
//!
//! The signature is part of the contract A will keep:
//! `pub fn build_manifest(grid: &TableGrid) -> TableManifest`.

use crate::tables::{
    Cell, ColumnInfo, ColumnKind, ColumnRole, Confidence, TableGrid, TableManifest,
};

/// Build a minimal manifest from a grid.
///
/// Every column's kind is the one kind all its non-empty cells agree on;
/// anything else is `Mixed`, and an all-empty column is `Empty`. Numeric
/// columns become `Quantity` (there is no code detection here), dated columns
/// `Date`, and text columns `Label`. No row is marked derived.
#[must_use]
pub fn build_manifest(grid: &TableGrid) -> TableManifest {
    let n_cols = grid.header.len();
    let mut columns = Vec::with_capacity(n_cols);
    for (index, name) in grid.header.iter().enumerate() {
        let kind = column_kind(grid, index);
        columns.push(ColumnInfo {
            name: name.clone(),
            kind,
            role: match kind {
                ColumnKind::Number => ColumnRole::Quantity,
                ColumnKind::Date => ColumnRole::Date,
                ColumnKind::Bool => ColumnRole::Label,
                ColumnKind::Text | ColumnKind::Mixed | ColumnKind::Empty => ColumnRole::Label,
            },
            unit: None,
            sample_values: Vec::new(),
            distinct_count: 0,
        });
    }
    TableManifest {
        columns,
        derived_rows: Vec::new(),
        formulas_known: false,
        confidence: Confidence::Low,
        n_rows: u32::try_from(grid.rows.len()).unwrap_or(u32::MAX),
        n_cols: u32::try_from(n_cols).unwrap_or(u32::MAX),
        truncated: grid.truncated,
        notes: grid.notes.clone(),
    }
}

/// The one kind every non-empty cell of a column agrees on, or `Mixed`.
fn column_kind(grid: &TableGrid, index: usize) -> ColumnKind {
    let mut seen = ColumnKind::Empty;
    for row in &grid.rows {
        let kind = match row.get(index).unwrap_or(&Cell::Empty) {
            Cell::Empty => continue,
            Cell::Text(_) => ColumnKind::Text,
            Cell::Number(_) => ColumnKind::Number,
            // A bool column is still read as text until A's heuristics decide
            // otherwise; a "yes/no" column is not a number to sum.
            Cell::Bool(_) => ColumnKind::Text,
            Cell::Date(_) => ColumnKind::Date,
        };
        if seen == ColumnKind::Empty {
            seen = kind;
        } else if seen != kind {
            return ColumnKind::Mixed;
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(c: Cell) -> Cell {
        c
    }

    #[test]
    fn a_flat_numeric_column_is_a_quantity() {
        let grid = TableGrid {
            header: vec!["Region".into(), "Revenue".into()],
            rows: vec![
                vec![cell(Cell::Text("Jawa Timur".into())), cell(Cell::Number(1.5))],
                vec![cell(Cell::Text("Bali".into())), cell(Cell::Number(2.0))],
            ],
            ..TableGrid::default()
        };
        let manifest = build_manifest(&grid);
        assert_eq!(manifest.n_rows, 2);
        assert_eq!(manifest.n_cols, 2);
        assert_eq!(manifest.columns[0].kind, ColumnKind::Text);
        assert_eq!(manifest.columns[0].role, ColumnRole::Label);
        assert_eq!(manifest.columns[1].kind, ColumnKind::Number);
        assert_eq!(manifest.columns[1].role, ColumnRole::Quantity);
        assert!(manifest.derived_rows.is_empty());
        assert_eq!(manifest.confidence, Confidence::Low);
    }

    #[test]
    fn a_mixed_column_is_mixed_and_not_a_quantity() {
        let grid = TableGrid {
            header: vec!["Value".into()],
            rows: vec![
                vec![cell(Cell::Number(1.0))],
                vec![cell(Cell::Text("n/a".into()))],
            ],
            ..TableGrid::default()
        };
        let manifest = build_manifest(&grid);
        assert_eq!(manifest.columns[0].kind, ColumnKind::Mixed);
        assert_ne!(manifest.columns[0].role, ColumnRole::Quantity);
    }

    #[test]
    fn a_ragged_row_does_not_panic() {
        let grid = TableGrid {
            header: vec!["A".into(), "B".into()],
            rows: vec![vec![cell(Cell::Number(1.0))]],
            ..TableGrid::default()
        };
        let manifest = build_manifest(&grid);
        assert_eq!(manifest.columns[1].kind, ColumnKind::Empty);
    }
}
