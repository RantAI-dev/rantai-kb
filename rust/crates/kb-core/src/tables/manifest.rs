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

    let seeded = derived_rows(grid, &roles);
    let (derived_rows, arithmetic) = arithmetic_derived_rows(grid, &roles, seeded);
    let mut derived_rows = derived_rows;
    if grid.truncated || n_rows as u32 > MAX_TABLE_ROWS {
        derived_rows.retain(|row| *row < MAX_TABLE_ROWS);
    }
    let formulas_known = !grid.formula_rows.is_empty();
    let mut notes = manifest_notes(grid);
    if let Some(note) = arithmetic_note(&arithmetic) {
        notes.push(note);
    }

    TableManifest {
        columns,
        derived_rows,
        formulas_known,
        confidence: confidence(grid, formulas_known, !arithmetic.is_empty(), &notes),
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
    if value.is_empty() || is_placeholder(value) {
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
            // Only dots: a single dot inside a long number (`913140741332.687`)
            // is the English decimal mark, not a thousands separator. Short
            // `1.234` and repeated groups `1.234.567` are grouped thousands.
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
    let rest: Vec<&str> = parts.collect();
    let groups = rest.len();
    let all_three = rest.iter().all(|group| group.len() == 3);
    groups >= 1
        && all_three
        && !first.is_empty()
        && first.chars().all(|c| c.is_ascii_digit())
        // `913140741332.687` is a decimal, not `913140741332687`.
        && !(groups == 1 && first.len() > 3)
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

/// True for the placeholders an Indonesian statistical table (BPS) writes in a
/// cell that has no value: `-`, `–`, `—`, `...`, `…`, `x`, `X`, `NA`, `N/A`,
/// `n.a.`, `*`.
///
/// The match is exact after trimming, so `-0,25` (a negative number) and
/// `n.a. something` are not placeholders. Such a cell is read as
/// [`Cell::Empty`], which keeps a year column of numbers `Number`/`Quantity`
/// instead of "mixed" and stops it sorting as text.
#[must_use]
pub fn is_placeholder(text: &str) -> bool {
    matches!(
        text.trim(),
        "-" | "\u{2013}"
            | "\u{2014}"
            | "..."
            | "\u{2026}"
            | "x"
            | "X"
            | "NA"
            | "N/A"
            | "n.a."
            | "*"
    )
}

/// The manifest note for placeholder cells read as empty, or `None` when a grid
/// has none.
#[must_use]
pub fn placeholder_note(count: usize) -> Option<String> {
    (count > 0).then(|| format!("{count} placeholder cells treated as empty"))
}

/// Read a text cell into the typed contract: booleans, ISO dates, numbers and
/// the rest as text. A zero-padded digit string (`00123`, a NIK) stays text so
/// its leading zeros survive, and so does a long fixed-width digit string
/// (`197011281990031002`, an NIP): an `f64` cannot hold 18 digits exactly, and
/// a sum over an identifier is never a meaningful answer. A statistical
/// placeholder (`-`, `...`, `NA`) is empty rather than a label.
#[must_use]
pub fn cell_from_text(text: &str) -> Cell {
    let trimmed = text.trim();
    if trimmed.is_empty() || is_placeholder(trimmed) {
        return Cell::Empty;
    }
    if trimmed.eq_ignore_ascii_case("true") {
        return Cell::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return Cell::Bool(false);
    }
    if is_zero_padded_digits(trimmed) || is_long_identifier(trimmed) {
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

/// A government identifier (NIP, NIK, a long account number) is written as a
/// run of digits, but it names a person or an account: it must keep every
/// digit and never be summed.
fn is_long_identifier(text: &str) -> bool {
    // 16/18-digit government IDs; a World Bank GDP is at most ~15 digits and
    // stays a number.
    text.len() >= 16 && text.chars().all(|ch| ch.is_ascii_digit())
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

/// A bare total word (`Jumlah`, `Total`, `Grand total`) is a derived row on
/// its own. `Jumlah Posyandu aktif` is an ordinary data row whose label merely
/// starts with a total word: it must be proven by arithmetic, never dropped
/// just because of the first word.
fn is_bare_total_label(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "jumlah"
            | "total"
            | "sub total"
            | "subtotal"
            | "grand total"
            | "grandtotal"
            | "rata-rata"
            | "rata rata"
            | "average"
    )
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
                    }) && matches!(cell, Cell::Text(text) if is_bare_total_label(text))
                })
            })
            .map(|(index, _)| index as u32)
            .collect()
    } else {
        // A formula in a year-keyed block computes a rate for that year; it
        // does not make the year row a subtotal of the table above it.
        grid.formula_rows
            .iter()
            .copied()
            .filter(|row| !row_is_year_keyed(grid, *row as usize))
            .collect()
    };
    rows.sort_unstable();
    rows.dedup();
    rows.retain(|row| (*row as usize) < grid.rows.len());
    rows
}

/// Derived rows proven by arithmetic rather than by a label or a formula.
///
/// BPS total rows are often labelled with the region (`Kabupaten Klaten`,
/// `Kota X`, `Provinsi X`, `Indonesia`), and a DOCX or CSV copy of the same
/// table carries no formulas. Language-independent rule: a row whose Quantity
/// cells each equal the sum of the data rows above it is a subtotal, and one
/// that equals the sum of every other data row is a grand total.
///
/// `(a)` is checked on the last row and on the first (a total placed on top);
/// `(b)` walks the rows and takes a row that sums the contiguous block since the
/// previous derived row. A row needs at least two rows to sum before arithmetic
/// may call it derived, so a `[100, 100]` table never loses its last row.
fn arithmetic_derived_rows(
    grid: &TableGrid,
    roles: &[ColumnRole],
    seeded: Vec<u32>,
) -> (Vec<u32>, Vec<u32>) {
    let quantity: Vec<usize> = roles
        .iter()
        .enumerate()
        .filter(|(_, role)| **role == ColumnRole::Quantity)
        .map(|(index, _)| index)
        .collect();
    let mut derived = seeded;
    if quantity.is_empty() || grid.rows.len() < 3 {
        return (derived, Vec::new());
    }
    derived.sort_unstable();
    derived.dedup();
    let mut found: Vec<u32> = Vec::new();
    // One quantity column is weak evidence: in [1, 2, 3] the 3 "sums" the rows
    // above by coincidence. Several columns all matching is not a coincidence.
    // A single-column table needs three summed rows, or two when the row is
    // labelled like a total ("Kabupaten Klaten", "Indonesia", "Jumlah").
    let single = quantity.len() == 1;
    let min_rows_for = |row: usize| {
        if single && !row_has_total_label(grid, row) {
            3
        } else {
            2
        }
    };

    // (b) A subtotal sums the contiguous block since the last derived row.
    let mut block_start = 0usize;
    for row in 0..grid.rows.len() {
        if derived.contains(&(row as u32)) {
            block_start = row + 1;
            continue;
        }
        if row_is_year_keyed(grid, row) {
            continue;
        }
        if row >= block_start + min_rows_for(row)
            && row_equals_sum(grid, row, block_start..row, &quantity)
        {
            found.push(row as u32);
            derived.push(row as u32);
            derived.sort_unstable();
            block_start = row + 1;
        }
    }

    // (a) A grand total equals every other non-derived data row, whether it sits
    // at the bottom or (as some BPS sheets do) at the top.
    for candidate in [grid.rows.len() - 1, 0] {
        if derived.contains(&(candidate as u32)) || row_is_year_keyed(grid, candidate) {
            continue;
        }
        let others: Vec<usize> = (0..grid.rows.len())
            .filter(|row| *row != candidate && !derived.contains(&(*row as u32)))
            .collect();
        if others.len() < min_rows_for(candidate) {
            continue;
        }
        if row_equals_sum(grid, candidate, others.iter().copied(), &quantity) {
            found.push(candidate as u32);
            derived.push(candidate as u32);
        }
    }

    found.sort_unstable();
    found.dedup();
    (derived, found)
}

/// A stacked year block (`2019`, `2020` … below a kecamatan table) uses a year
/// as its row key. Such a row can coincidentally equal the sum of the rows
/// above it, but it is not a subtotal and must not be excluded.
fn row_is_year_keyed(grid: &TableGrid, row: usize) -> bool {
    grid.rows.get(row).is_some_and(|cells| {
        cells.iter().find(|cell| **cell != Cell::Empty).is_some_and(|cell| {
            matches!(cell, Cell::Number(number) if number.fract() == 0.0 && (1900.0..=2100.0).contains(number))
                || matches!(cell, Cell::Text(text) if text.trim().len() == 4 && text.trim().chars().all(|c| c.is_ascii_digit()) && text.trim().parse::<i32>().is_ok_and(|year| (1900..=2100).contains(&year)))
        })
    })
}

/// A label that names an aggregate: a region level (BPS labels its total row
/// with the regency, city, province or the country) or a total word.
fn row_has_total_label(grid: &TableGrid, row: usize) -> bool {
    let Some(cells) = grid.rows.get(row) else {
        return false;
    };
    cells.iter().any(|cell| match cell {
        Cell::Text(text) => {
            let t = text.trim().to_lowercase();
            is_derived_label(&t)
                || [
                    "kabupaten",
                    "kab.",
                    "kota ",
                    "provinsi",
                    "prov.",
                    "indonesia",
                    "nasional",
                    "national",
                    "total",
                    "jumlah",
                ]
                .iter()
                .any(|p| t.starts_with(p))
        }
        _ => false,
    })
}

/// True when every non-empty Quantity cell of `row` equals the sum of the same
/// column over `rows`, within `abs ≤ 0.5` or `rel ≤ 0.1%`.
///
/// A row with more than one quantity column needs at least two filled quantity
/// cells to be a total, and every checked column needs at least two contributing
/// rows: a single row above is a coincidence, not a subtotal.
fn row_equals_sum(
    grid: &TableGrid,
    row: usize,
    rows: impl Iterator<Item = usize>,
    quantity: &[usize],
) -> bool {
    let Some(cells) = grid.rows.get(row) else {
        return false;
    };
    let actual: Vec<(usize, f64)> = quantity
        .iter()
        .filter_map(|column| match cells.get(*column) {
            Some(Cell::Number(value)) if value.is_finite() => Some((*column, *value)),
            _ => None,
        })
        .collect();
    if actual.is_empty() || (quantity.len() > 1 && actual.len() < 2) {
        return false;
    }
    let rows: Vec<usize> = rows.collect();
    if rows.len() < 2 {
        return false;
    }
    actual.into_iter().all(|(column, value)| {
        let mut expected = 0.0;
        let mut contributions = 0usize;
        for other in &rows {
            if let Some(Cell::Number(number)) =
                grid.rows.get(*other).and_then(|row| row.get(column))
            {
                if number.is_finite() {
                    expected += number;
                    contributions += 1;
                }
            }
        }
        contributions >= 2 && close_enough(value, expected)
    })
}

/// Equal as a total: exactly, when both are whole numbers (a count table's
/// total is its exact sum), else within `rel ≤ 0.1%` (a rounded decimal
/// table). An absolute slack would make small decimals "sum" by accident:
/// 0.3 is within 0.5 of 0.2 + 0.2.
fn close_enough(actual: f64, expected: f64) -> bool {
    let difference = (actual - expected).abs();
    let whole = |v: f64| (v - v.round()).abs() < 1e-9;
    if whole(actual) && whole(expected) {
        return difference < 1e-9;
    }
    let scale = expected.abs().max(actual.abs());
    scale > 0.0 && difference / scale <= 0.001
}

/// The manifest note for rows proven derived by arithmetic.
fn arithmetic_note(rows: &[u32]) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let indices = rows
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("derived rows proven by arithmetic: {indices}"))
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

fn confidence(
    grid: &TableGrid,
    formulas_known: bool,
    arithmetic_derived: bool,
    notes: &[String],
) -> Confidence {
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
        // A row proven by arithmetic is stronger evidence than a guessed
        // structure is weak: a markdown table with a real total is Medium.
        return if arithmetic_derived {
            Confidence::Medium
        } else {
            Confidence::Low
        };
    }
    let label_derived = grid.rows.iter().any(|row| {
        row.iter()
            .any(|cell| matches!(cell, Cell::Text(text) if is_derived_label(text)))
    });
    if label_derived || arithmetic_derived {
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
        // A long English decimal (World Bank GDP) keeps its fractional part.
        assert_eq!(locale_number("913140741332.687"), Some(913_140_741_332.687));
        assert_eq!(locale_number("1.234.567"), Some(1_234_567.0));
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
    fn a_long_digit_string_is_an_identifier_that_cannot_be_summed() {
        let nip = "197011281990031002";
        assert_eq!(cell_from_text(nip), Cell::Text(nip.to_string()));
        let manifest = build_manifest(&grid(
            &["Nama PNS", "NIP"],
            vec![
                vec![text("MOCHAMAD NURROSYID"), text(nip)],
                vec![text("SRI WAHYUNINGSIH"), text("197707112010012010")],
            ],
        ));
        assert_eq!(manifest.columns[1].kind, ColumnKind::Text);
        assert_eq!(manifest.columns[1].role, ColumnRole::Code);
        assert!(
            manifest.columns[1].sample_values.contains(&nip.to_string()),
            "{:?}",
            manifest.columns[1].sample_values
        );
        // The compiler refuses SUM/AVG unless the role is Quantity.
        assert_ne!(manifest.columns[1].role, ColumnRole::Quantity);
        // A six-digit kode wilayah written as text is a code by its shape too.
        let codes = build_manifest(&grid(
            &["Wilayah", "Kode"],
            vec![
                vec![text("Prambanan"), text("331001")],
                vec![text("Wedi"), text("331002")],
            ],
        ));
        assert_eq!(codes.columns[1].role, ColumnRole::Code);
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

    #[test]
    fn bps_placeholders_are_empty_and_a_year_column_stays_a_quantity() {
        let placeholders = [
            "-", "\u{2013}", "\u{2014}", "...", "\u{2026}", "x", "X", "NA", "N/A", "n.a.", "*",
        ];
        for placeholder in placeholders {
            assert_eq!(
                cell_from_text(placeholder),
                Cell::Empty,
                "{placeholder:?} is a placeholder"
            );
            assert_eq!(cell_from_text(&format!("  {placeholder}  ")), Cell::Empty);
            assert_eq!(locale_number(placeholder), None);
        }
        // Only the exact placeholder is empty: a negative number and a label
        // that merely starts with one are values.
        assert_eq!(cell_from_text("-0,25"), Cell::Number(-0.25));
        assert_eq!(
            cell_from_text("n.a. belum tersedia"),
            Cell::Text("n.a. belum tersedia".to_string())
        );
        assert_eq!(placeholder_note(0), None);
        assert_eq!(
            placeholder_note(3).as_deref(),
            Some("3 placeholder cells treated as empty")
        );

        let manifest = build_manifest(&grid(
            &["Kecamatan", "2018", "2019", "2020"],
            vec![
                vec![
                    text("Klaten Utara"),
                    number(120.0),
                    Cell::Empty,
                    number(140.0),
                ],
                vec![
                    text("Klaten Tengah"),
                    number(130.0),
                    number(150.0),
                    Cell::Empty,
                ],
            ],
        ));
        assert_eq!(manifest.columns[1].kind, ColumnKind::Number);
        assert_eq!(manifest.columns[1].role, ColumnRole::Quantity);
        assert_eq!(manifest.columns[2].role, ColumnRole::Quantity);
        assert_eq!(manifest.columns[3].role, ColumnRole::Quantity);
    }

    #[test]
    fn a_total_row_labelled_with_the_region_is_found_by_arithmetic() {
        // The shape of a BPS health table: 26 kecamatan rows and a last row
        // "Kabupaten Klaten" holding the column sums, with no formula.
        let mut rows: Vec<Vec<Cell>> = (0..26)
            .map(|index| vec![text(&format!("Kecamatan {index}")), number(1.0)])
            .collect();
        rows.push(vec![text("Kabupaten Klaten"), number(26.0)]);
        let manifest = build_manifest(&grid(&["Kecamatan", "Dokter"], rows));

        assert_eq!(manifest.derived_rows, vec![26]);
        assert!(!manifest.formulas_known);
        assert_eq!(manifest.confidence, Confidence::Medium);
        assert!(
            manifest
                .notes
                .iter()
                .any(|note| note.contains("proven by arithmetic")),
            "{:?}",
            manifest.notes
        );
    }

    #[test]
    fn a_group_subtotal_and_the_grand_total_are_both_derived() {
        let manifest = build_manifest(&grid(
            &["Kecamatan", "Dokter"],
            vec![
                vec![text("A"), number(10.0)],
                vec![text("B"), number(20.0)],
                vec![text("Subtotal A"), number(30.0)],
                vec![text("C"), number(5.0)],
                vec![text("D"), number(5.0)],
                vec![text("Subtotal B"), number(10.0)],
                vec![text("Kabupaten Klaten"), number(40.0)],
            ],
        ));
        assert_eq!(manifest.derived_rows, vec![2, 5, 6]);
        assert_eq!(manifest.confidence, Confidence::Medium);
    }

    #[test]
    fn a_grand_total_on_the_first_row_is_derived_too() {
        let manifest = build_manifest(&grid(
            &["Kecamatan", "Dokter"],
            vec![
                vec![text("Kabupaten Klaten"), number(30.0)],
                vec![text("A"), number(10.0)],
                vec![text("B"), number(20.0)],
            ],
        ));
        assert_eq!(manifest.derived_rows, vec![0]);
    }

    #[test]
    fn a_row_is_never_derived_when_there_are_fewer_than_two_rows_to_sum() {
        let manifest = build_manifest(&grid(
            &["Kecamatan", "Dokter"],
            vec![
                vec![text("A"), number(100.0)],
                vec![text("B"), number(100.0)],
            ],
        ));
        assert!(
            manifest.derived_rows.is_empty(),
            "{:?}",
            manifest.derived_rows
        );
        assert_eq!(manifest.confidence, Confidence::High);

        // Two rows above that happen to add up differently are data, not a total.
        let not_a_total = build_manifest(&grid(
            &["Kecamatan", "Dokter"],
            vec![
                vec![text("A"), number(10.0)],
                vec![text("B"), number(20.0)],
                vec![text("C"), number(35.0)],
            ],
        ));
        assert!(not_a_total.derived_rows.is_empty());
    }

    #[test]
    fn a_year_keyed_row_is_not_a_subtotal() {
        // The stacked `2019 / 2020` block below a kecamatan table is data: its
        // values may equal the sum of the rows above by coincidence.
        let manifest = build_manifest(&grid(
            &["Kecamatan", "Produksi"],
            vec![
                vec![text("A"), number(10.0)],
                vec![text("B"), number(20.0)],
                vec![text("Jumlah"), number(30.0)],
                vec![number(2019.0), number(30.0)],
                vec![number(2020.0), number(15.0)],
            ],
        ));
        assert_eq!(
            manifest.derived_rows,
            vec![2],
            "{:?}",
            manifest.derived_rows
        );
    }

    #[test]
    fn a_formula_in_a_year_keyed_row_does_not_make_it_a_subtotal() {
        let mut sheet = grid(
            &["Kecamatan", "Produksi", "Rasio"],
            vec![
                vec![text("A"), number(10.0), number(1.0)],
                vec![text("Jumlah"), number(10.0), number(1.0)],
                vec![number(2019.0), number(20.0), number(2.0)],
            ],
        );
        sheet.formula_rows = vec![1, 2];
        let manifest = build_manifest(&sheet);
        assert_eq!(
            manifest.derived_rows,
            vec![1],
            "{:?}",
            manifest.derived_rows
        );
        assert!(manifest.formulas_known);
    }

    #[test]
    fn a_single_column_coincidence_is_not_a_total() {
        // [1, 2, 3]: the 3 equals 1 + 2, but one column and two rows prove nothing.
        let manifest = build_manifest(&grid(
            &["Kecamatan", "Jumlah Sekolah"],
            vec![
                vec![text("A"), number(1.0)],
                vec![text("B"), number(2.0)],
                vec![text("C"), number(3.0)],
            ],
        ));
        assert!(
            manifest.derived_rows.is_empty(),
            "{:?}",
            manifest.derived_rows
        );
    }

    #[test]
    fn small_decimals_do_not_sum_by_tolerance() {
        // 0.3 is within 0.5 of 0.2 + 0.2 + 0.1; it is a rate, not a total.
        let manifest = build_manifest(&grid(
            &["Wilayah", "Rasio", "Laju"],
            vec![
                vec![text("A"), number(0.2), number(0.1)],
                vec![text("B"), number(0.2), number(0.1)],
                vec![text("C"), number(0.1), number(0.1)],
                vec![text("D"), number(0.3), number(0.2)],
            ],
        ));
        assert!(
            manifest.derived_rows.is_empty(),
            "{:?}",
            manifest.derived_rows
        );
    }

    #[test]
    fn a_single_column_total_over_three_rows_is_found() {
        let manifest = build_manifest(&grid(
            &["Kecamatan", "Jumlah"],
            vec![
                vec![text("A"), number(10.0)],
                vec![text("B"), number(20.0)],
                vec![text("C"), number(30.0)],
                vec![text("Kabupaten Klaten"), number(60.0)],
            ],
        ));
        assert_eq!(manifest.derived_rows, vec![3]);
    }

    #[test]
    fn arithmetic_proof_lifts_a_guessed_markdown_table_to_medium() {
        let mut guessed = grid(
            &["Region", "Value"],
            vec![
                vec![text("A"), number(1.0)],
                vec![text("B"), number(2.0)],
                vec![text("Indonesia"), number(3.0)],
            ],
        );
        guessed.source = Some(TableSource::Markdown);
        let manifest = build_manifest(&guessed);
        assert_eq!(manifest.derived_rows, vec![2]);
        assert_eq!(manifest.confidence, Confidence::Medium);
    }
}
