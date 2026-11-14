//! Reading a grid: what its columns mean and which rows are derived (`TBL-1`).
//!
//! Pure heuristics, no I/O. The rules are the ones in
//! `docs/core/specs/tbl-1.md` work package A: a phone column is a `Code`, a
//! year column is a `Date`, a formula-proven subtotal is `High` confidence and
//! a "Jumlah" row guessed from its label is `Medium`.

use std::collections::HashMap;

use super::{
    Cell, ColumnInfo, ColumnKind, ColumnRole, Confidence, MAX_TABLE_ROWS, TableGrid, TableManifest,
    TableSource,
};

/// The longest run of sample values a manifest keeps.
const MAX_SAMPLES: usize = 10;

/// Build the manifest of one grid: column kinds and roles, units, samples and
/// the rows that are derived (subtotals) rather than data.
#[must_use]
pub fn build_manifest(grid: &TableGrid) -> TableManifest {
    let n_cols = grid.header.len();
    let n_rows = grid.rows.len();
    let mut columns: Vec<ColumnInfo> = Vec::with_capacity(n_cols);
    let mut roles: Vec<ColumnRole> = Vec::with_capacity(n_cols);

    for index in 0..n_cols {
        let values = column_values(grid, index);
        let kind = column_kind(&values);
        let role = column_role(&grid.header[index], kind, &values);
        roles.push(role);
        columns.push(ColumnInfo {
            name: grid.header[index].clone(),
            kind,
            role,
            unit: unit_from_header(&grid.header[index]),
            sample_values: if role == ColumnRole::Quantity {
                Vec::new()
            } else {
                sample_values(&values)
            },
            distinct_count: distinct_count(&values),
        });
    }

    // The first text column with high distinctness is the row's key. Roles are
    // decided per column, then this pass picks at most one identifier.
    let mut has_identifier = roles.contains(&ColumnRole::Identifier);
    for index in 0..n_cols {
        if has_identifier {
            break;
        }
        if matches!(roles[index], ColumnRole::Label) {
            let values = column_values(grid, index);
            let kind = column_kind(&values);
            if matches!(kind, ColumnKind::Text | ColumnKind::Mixed)
                && high_distinctness(&values, columns[index].distinct_count)
            {
                roles[index] = ColumnRole::Identifier;
                columns[index].role = ColumnRole::Identifier;
                has_identifier = true;
            }
        }
    }

    let mut derived_rows = derived_rows(grid, &roles);
    if grid.truncated || n_rows as u32 > MAX_TABLE_ROWS {
        derived_rows.retain(|row| *row < MAX_TABLE_ROWS);
    }
    let formulas_known = !grid.formula_rows.is_empty();
    let notes = manifest_notes(grid);

    TableManifest {
        columns,
        derived_rows,
        formulas_known,
        confidence: confidence(grid, formulas_known, &notes),
        n_rows: n_rows.min(MAX_TABLE_ROWS as usize) as u32,
        n_cols: n_cols as u32,
        truncated: grid.truncated || n_rows as u32 > MAX_TABLE_ROWS,
        notes,
    }
}

/// Parse a number as a spreadsheet in Indonesia or an English one writes it.
///
/// `1.234,5` and `1,234.5` are both 1234.5; `(1.234)` is -1234; a trailing `%`
/// and a leading `Rp` are units, not part of the value. `None` when the text is
/// not a number at all.
#[must_use]
pub fn locale_number(text: &str) -> Option<f64> {
    let mut value = text.trim();
    if value.is_empty() {
        return None;
    }

    // Parentheses are accounting's minus sign.
    let negative = value.starts_with('(') && value.ends_with(')');
    if negative {
        value = &value[1..value.len() - 1];
    }

    let mut cleaned = String::with_capacity(value.len());
    let mut had_percent = false;
    for ch in value.chars() {
        match ch {
            '%' => had_percent = true,
            '\u{a0}' | ' ' | '\t' | '\'' | '\u{202f}' => {}
            'R' | 'r' | 'p' | 'P' | '$' | '\u{20ac}' | '\u{00a3}' | '\u{00a5}' => {}
            _ => cleaned.push(ch),
        }
    }
    let mut cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        return None;
    }
    if had_percent {
        // The unit is in the header; the stored value keeps the written scale.
        cleaned = cleaned.trim_end_matches('%').to_string();
    }

    let has_dot = cleaned.matches('.').count();
    let has_comma = cleaned.matches(',').count();
    let normalised = match (has_dot, has_comma) {
        (0, 0) => cleaned.clone(),
        (_, 0) => {
            // Only dots: the last group of exactly three digits after a dot is
            // a thousands separator (`1.234`), otherwise a decimal point.
            if has_dot > 1 || is_thousands_group(&cleaned, '.') {
                cleaned.replace('.', "")
            } else {
                cleaned
            }
        }
        (0, _) => {
            if has_comma > 1 || is_thousands_group(&cleaned, ',') {
                cleaned.replace(',', "")
            } else {
                cleaned.replace(',', ".")
            }
        }
        (_, _) => {
            let last_dot = cleaned.rfind('.').unwrap_or(0);
            let last_comma = cleaned.rfind(',').unwrap_or(0);
            if last_comma > last_dot {
                // `1.234,5`: dots group, comma is the decimal mark.
                cleaned.replace('.', "").replace(',', ".")
            } else {
                // `1,234.5`: commas group, dot is the decimal mark.
                cleaned.replace(',', "")
            }
        }
    };

    let normalised = normalised.trim();
    if normalised.is_empty() {
        return None;
    }
    let parsed: f64 = normalised.parse().ok()?;
    if !parsed.is_finite() {
        return None;
    }
    Some(if negative { -parsed } else { parsed })
}

/// True when the separator is a thousands separator: exactly three digits
/// follow it and the text has more than one digit before it (`1.234`), or it
/// groups several times (`1.234.567`).
fn is_thousands_group(text: &str, separator: char) -> bool {
    let mut parts = text.split(separator);
    let first = parts.next().unwrap_or("");
    let all_three = parts.clone().all(|group| group.len() == 3);
    let groups = parts.count();
    groups >= 1 && all_three && !first.is_empty() && first.chars().all(|c| c.is_ascii_digit())
}

fn cell_text(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Empty => None,
        Cell::Text(text) => Some(text.clone()),
        Cell::Number(number) => Some(format_number(*number)),
        Cell::Bool(value) => Some(if *value { "true" } else { "false" }.to_string()),
        Cell::Date(date) => Some(date.clone()),
    }
}

fn format_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn column_values(grid: &TableGrid, index: usize) -> Vec<Cell> {
    grid.rows
        .iter()
        .filter_map(|row| row.get(index).cloned())
        .filter(|cell| !matches!(cell, Cell::Empty))
        .collect()
}

fn column_kind(values: &[Cell]) -> ColumnKind {
    let mut has_number = false;
    let mut has_text = false;
    let mut has_date = false;
    let mut has_bool = false;
    for cell in values {
        match cell {
            Cell::Empty => {}
            Cell::Number(_) => has_number = true,
            Cell::Text(_) => has_text = true,
            Cell::Date(_) => has_date = true,
            Cell::Bool(_) => has_bool = true,
        }
    }
    let kinds = [has_number, has_text, has_date, has_bool]
        .iter()
        .filter(|seen| **seen)
        .count();
    match kinds {
        0 => ColumnKind::Empty,
        1 if has_number => ColumnKind::Number,
        1 if has_text => ColumnKind::Text,
        1 if has_date => ColumnKind::Date,
        1 => ColumnKind::Bool,
        _ => ColumnKind::Mixed,
    }
}

fn column_role(name: &str, kind: ColumnKind, values: &[Cell]) -> ColumnRole {
    if kind == ColumnKind::Date {
        return ColumnRole::Date;
    }
    if header_looks_like_code(name) || values_look_like_code(values) {
        return ColumnRole::Code;
    }
    if kind == ColumnKind::Number {
        if header_looks_like_period(name) && is_year_column(values) {
            return ColumnRole::Date;
        }
        return ColumnRole::Quantity;
    }
    ColumnRole::Label
}

fn header_tokens(name: &str) -> impl Iterator<Item = String> + '_ {
    name.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .into_iter()
}

fn header_looks_like_code(name: &str) -> bool {
    header_tokens(name).any(|token| {
        matches!(
            token.as_str(),
            "kode" | "code" | "nik" | "npwp" | "telepon" | "telp" | "phone" | "hp" | "nomor" | "no"
        )
    }) || name.to_lowercase().contains("no.")
}

fn header_looks_like_period(name: &str) -> bool {
    header_tokens(name).any(|token| {
        matches!(
            token.as_str(),
            "tahun"
                | "year"
                | "tanggal"
                | "date"
                | "periode"
                | "period"
                | "bulan"
                | "month"
                | "kuartal"
                | "quarter"
                | "triwulan"
                | "semester"
                | "minggu"
                | "week"
        )
    })
}

fn values_look_like_code(values: &[Cell]) -> bool {
    // Only text cells can be a code by their shape: a numeric column of
    // six-digit revenue values is a quantity, not a phone list. Codes arrive
    // as text because of their leading zeros; a header that says "kode" or
    // "telepon" is handled above even when the value is numeric.
    if values.is_empty() || !values.iter().all(|cell| matches!(cell, Cell::Text(_))) {
        return false;
    }
    let texts: Vec<String> = values.iter().filter_map(cell_text).collect();
    if texts.is_empty() {
        return false;
    }
    let all_digits = texts.iter().all(|text| {
        let text = text.trim();
        !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
    });
    if !all_digits {
        return false;
    }
    let leading_zero = texts
        .iter()
        .any(|text| text.len() >= 2 && text.starts_with('0'));
    let lengths: Vec<usize> = texts.iter().map(String::len).collect();
    let fixed_length =
        lengths.iter().all(|len| *len >= 6) && lengths.windows(2).all(|w| w[0] == w[1]);
    leading_zero || fixed_length
}

fn is_year_column(values: &[Cell]) -> bool {
    !values.is_empty()
        && values.iter().all(|cell| match cell {
            Cell::Number(number) => number.fract() == 0.0 && (1900.0..=2100.0).contains(number),
            Cell::Date(_) => true,
            _ => false,
        })
}

fn high_distinctness(values: &[Cell], distinct: u32) -> bool {
    if values.is_empty() {
        return false;
    }
    let ratio = f64::from(distinct) / values.len() as f64;
    ratio >= 0.7
}

fn distinct_count(values: &[Cell]) -> u32 {
    let mut set: Vec<String> = values.iter().filter_map(cell_text).collect();
    set.sort();
    set.dedup();
    set.len() as u32
}

fn sample_values(values: &[Cell]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for text in values.iter().filter_map(cell_text) {
        if !counts.contains_key(&text) {
            order.push(text.clone());
        }
        *counts.entry(text).or_insert(0) += 1;
    }
    let mut samples: Vec<(usize, String)> = order
        .into_iter()
        .map(|text| (counts.get(&text).copied().unwrap_or(0), text))
        .collect();
    samples.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
    samples
        .into_iter()
        .take(MAX_SAMPLES)
        .map(|(_, text)| text)
        .collect()
}

fn unit_from_header(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if let Some(inner) = trimmed.strip_suffix(')') {
        if let Some(open) = inner.rfind('(') {
            let unit = inner[open + 1..].trim();
            if !unit.is_empty()
                && (unit.contains('%')
                    || unit.chars().any(|c| c.is_alphabetic())
                    || unit
                        .chars()
                        .any(|c| matches!(c, '$' | '\u{20ac}' | '\u{00a3}' | '\u{00a5}')))
            {
                return Some(unit.to_string());
            }
        }
    }
    if trimmed.ends_with('%') {
        return Some("%".to_string());
    }
    None
}

/// Read a text cell into the typed contract: booleans, ISO dates, numbers and
/// the rest as text. A zero-padded digit string (`00123`, a NIK) stays text so
/// its leading zeros survive.
#[must_use]
pub fn cell_from_text(text: &str) -> Cell {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Cell::Empty;
    }
    if trimmed.eq_ignore_ascii_case("true") {
        return Cell::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return Cell::Bool(false);
    }
    if is_zero_padded_digits(trimmed) {
        return Cell::Text(trimmed.to_string());
    }
    if looks_like_iso_date(trimmed) {
        return Cell::Date(trimmed.to_string());
    }
    match locale_number(trimmed) {
        Some(number) => Cell::Number(number),
        None => Cell::Text(trimmed.to_string()),
    }
}

fn is_zero_padded_digits(text: &str) -> bool {
    text.len() > 1 && text.starts_with('0') && text.chars().all(|ch| ch.is_ascii_digit())
}

fn looks_like_iso_date(text: &str) -> bool {
    match text.get(..10) {
        Some(head) => chrono::NaiveDate::parse_from_str(head, "%Y-%m-%d").is_ok(),
        None => false,
    }
}

/// True for a label that names a derived row: "Jumlah", "Total", "Sub total",
/// "Grand total", "Rata-rata", "Average". The match is on a word boundary, so
/// "Total revenue" counts and "Totally unrelated" does not.
#[must_use]
pub fn is_derived_label(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    for prefix in [
        "grand total",
        "sub total",
        "subtotal",
        "rata-rata",
        "rata rata",
        "average",
        "jumlah",
        "total",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            if rest.is_empty() || !rest.chars().next().is_some_and(char::is_alphanumeric) {
                return true;
            }
        }
    }
    false
}

fn derived_rows(grid: &TableGrid, roles: &[ColumnRole]) -> Vec<u32> {
    let mut rows: Vec<u32> = if grid.formula_rows.is_empty() {
        grid.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.iter().enumerate().any(|(column, cell)| {
                    roles.get(column).is_some_and(|role| {
                        matches!(
                            role,
                            ColumnRole::Identifier | ColumnRole::Label | ColumnRole::Code
                        )
                    }) && matches!(cell, Cell::Text(text) if is_derived_label(text))
                })
            })
            .map(|(index, _)| index as u32)
            .collect()
    } else {
        grid.formula_rows.clone()
    };
    rows.sort_unstable();
    rows.dedup();
    rows.retain(|row| (*row as usize) < grid.rows.len());
    rows
}

fn manifest_notes(grid: &TableGrid) -> Vec<String> {
    let mut notes = grid.notes.clone();
    if grid.truncated || grid.rows.len() as u32 > MAX_TABLE_ROWS {
        notes.push(format!(
            "truncated at {MAX_TABLE_ROWS} rows; derived rows past the limit were dropped"
        ));
    }
    notes
}

fn confidence(grid: &TableGrid, formulas_known: bool, notes: &[String]) -> Confidence {
    if formulas_known {
        return Confidence::High;
    }
    let guessed_structure = matches!(
        grid.source,
        Some(TableSource::Markdown) | Some(TableSource::Pdf)
    );
    let ragged = grid.rows.iter().any(|row| row.len() != grid.header.len());
    let stitched = notes.iter().any(|note| note.contains("stitched"));
    if guessed_structure || ragged || stitched {
        return Confidence::Low;
    }
    let label_derived = grid.rows.iter().any(|row| {
        row.iter()
            .any(|cell| matches!(cell, Cell::Text(text) if is_derived_label(text)))
    });
    if label_derived {
        Confidence::Medium
    } else {
        Confidence::High
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::tables::{Cell, ColumnKind, ColumnRole, TableGrid, TableSource};

    fn text(value: &str) -> Cell {
        Cell::Text(value.to_string())
    }

    fn number(value: f64) -> Cell {
        Cell::Number(value)
    }

    fn grid(header: &[&str], rows: Vec<Vec<Cell>>) -> TableGrid {
        TableGrid {
            source: Some(TableSource::Xlsx),
            header: header.iter().map(|name| (*name).to_string()).collect(),
            header_rows: 1,
            rows,
            ..TableGrid::default()
        }
    }

    #[test]
    fn locale_numbers_read_both_conventions() {
        assert_eq!(locale_number("1.234,5"), Some(1234.5));
        assert_eq!(locale_number("1,234.5"), Some(1234.5));
        assert_eq!(locale_number("(1.234)"), Some(-1234.0));
        assert_eq!(locale_number("1.234"), Some(1234.0));
        assert_eq!(locale_number("1,5"), Some(1.5));
        assert_eq!(locale_number("-0,25"), Some(-0.25));
        assert_eq!(locale_number("12,5%"), Some(12.5));
        assert_eq!(locale_number("Rp 1.500.000"), Some(1_500_000.0));
        assert_eq!(locale_number("Rp 1.234,5"), Some(1234.5));
        assert_eq!(locale_number("2023"), Some(2023.0));
        assert_eq!(locale_number("(Rp 2.000)"), Some(-2000.0));
        assert_eq!(locale_number("abc"), None);
        assert_eq!(locale_number("Rp juta"), None);
        assert_eq!(locale_number(""), None);
    }

    #[test]
    fn roles_tell_a_phone_number_from_a_quantity() {
        let manifest = build_manifest(&grid(
            &[
                "Provinsi",
                "Pendapatan (Rp juta)",
                "Tahun",
                "Telepon",
                "NIK",
            ],
            vec![
                vec![
                    text("Jawa Timur"),
                    number(120.5),
                    number(2023.0),
                    text("0812345678"),
                    text("0123456789012345"),
                ],
                vec![
                    text("Jawa Barat"),
                    number(90.0),
                    number(2023.0),
                    text("0812987654"),
                    text("0123456789012346"),
                ],
            ],
        ));

        let roles: Vec<ColumnRole> = manifest.columns.iter().map(|c| c.role).collect();
        assert_eq!(
            roles,
            vec![
                ColumnRole::Identifier,
                ColumnRole::Quantity,
                ColumnRole::Date,
                ColumnRole::Code,
                ColumnRole::Code,
            ]
        );
        assert_eq!(manifest.columns[1].unit.as_deref(), Some("Rp juta"));
        assert_eq!(manifest.columns[1].sample_values, Vec::<String>::new());
        assert!(
            manifest.columns[3]
                .sample_values
                .contains(&"0812345678".to_string())
        );
        assert_eq!(manifest.columns[0].distinct_count, 2);
        assert_eq!(manifest.n_rows, 2);
        assert_eq!(manifest.n_cols, 5);
    }

    #[test]
    fn a_jumlah_row_is_medium_confidence_and_excluded() {
        let manifest = build_manifest(&grid(
            &["Provinsi", "Pendapatan"],
            vec![
                vec![text("Jawa Timur"), number(120.5)],
                vec![text("Jawa Barat"), number(90.0)],
                vec![text("Jumlah"), number(210.5)],
            ],
        ));
        assert_eq!(manifest.derived_rows, vec![2]);
        assert!(!manifest.formulas_known);
        assert_eq!(manifest.confidence, Confidence::Medium);
    }

    #[test]
    fn formula_rows_are_high_confidence() {
        let mut sheet = grid(
            &["Provinsi", "Pendapatan"],
            vec![
                vec![text("Jawa Timur"), number(120.5)],
                vec![text("Jumlah"), number(120.5)],
            ],
        );
        sheet.formula_rows = vec![1];
        let manifest = build_manifest(&sheet);
        assert_eq!(manifest.derived_rows, vec![1]);
        assert!(manifest.formulas_known);
        assert_eq!(manifest.confidence, Confidence::High);
    }

    #[test]
    fn a_year_column_is_a_date_not_a_quantity() {
        let manifest = build_manifest(&grid(
            &["Year", "Population"],
            vec![
                vec![number(2022.0), number(10.0)],
                vec![number(2023.0), number(12.0)],
            ],
        ));
        assert_eq!(manifest.columns[0].kind, ColumnKind::Number);
        assert_eq!(manifest.columns[0].role, ColumnRole::Date);
        assert_eq!(manifest.columns[1].role, ColumnRole::Quantity);
    }

    #[test]
    fn a_markdown_table_is_guessed_structure() {
        let mut guessed = grid(&["Region", "Value"], vec![vec![text("A"), number(1.0)]]);
        guessed.source = Some(TableSource::Markdown);
        assert_eq!(build_manifest(&guessed).confidence, Confidence::Low);
    }

    #[test]
    fn a_six_digit_numeric_column_is_a_quantity_unless_the_header_says_code() {
        let manifest = build_manifest(&grid(
            &["Pendapatan", "Kode"],
            vec![
                vec![number(100_000.0), text("123456")],
                vec![number(200_000.0), text("654321")],
            ],
        ));
        assert_eq!(manifest.columns[0].role, ColumnRole::Quantity);
        assert_eq!(manifest.columns[1].role, ColumnRole::Code);
    }

    #[test]
    fn labels_are_recognised_on_a_word_boundary() {
        for label in [
            "Jumlah",
            "JUMLAH ",
            "Total revenue",
            "Sub total",
            "Grand total",
            "Rata-rata",
            "Average",
        ] {
            assert!(is_derived_label(label), "{label}");
        }
        for label in ["Totally unrelated", "Jumlahkan", "Substantial"] {
            assert!(!is_derived_label(label), "{label}");
        }
    }
}
