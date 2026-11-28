//! The top of a grid: header names in one normal form, and the title rows
//! above them (`TBL-1`).
//!
//! Every source normalises its headers here, so the same column has the same
//! name whichever extractor read it. A spreadsheet cell may hold a line break
//! (`Bidan\n Midwife`), a document cell may hold a tab, and a portal export may
//! use a non-breaking space; a caller names the column in a query, so those
//! must all read `Bidan Midwife`. Runs of whitespace (including NBSP and the
//! other Unicode spaces) collapse to one space, the text is trimmed, and a
//! multi-level header's parts join with one `" / "`.
//!
//! BPS sheets put one or more title rows above the header: a table number
//! (`Tabel 25.8`) and a long title, often merged across every column. After
//! merge spreading such a row holds one distinct value in all its non-empty
//! cells, so it is recognised by shape, not by words, and removed from the
//! grid before the header is read.

use super::Cell;

/// Collapse every run of whitespace to one space and trim the ends.
///
/// `char::is_whitespace` is Unicode's `White_Space` property, so a line break,
/// a tab, a no-break space (U+00A0), a narrow no-break space (U+202F) and a
/// figure space all count.
#[must_use]
pub fn collapse_whitespace(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !collapsed.is_empty();
        } else {
            if pending_space {
                collapsed.push(' ');
                pending_space = false;
            }
            collapsed.push(ch);
        }
    }
    collapsed
}

/// A header name in its one normal form: no line breaks, no double spaces, and
/// the `" / "` of a multi-level join with exactly one space on each side.
#[must_use]
pub fn normalize_header_name(text: &str) -> String {
    collapse_whitespace(text)
}

/// Join the non-empty parts of a multi-level header with `" / "`, dropping a
/// part repeated by a merged cell above it (`Kangkung` spread over three rows
/// must not become `Kangkung / Kangkung / Luas Panen`).
#[must_use]
pub fn flatten_header_parts(parts: &[String]) -> String {
    let mut flattened: Vec<String> = Vec::with_capacity(parts.len());
    for part in parts {
        let name = normalize_header_name(part);
        if !name.is_empty()
            && flattened
                .last()
                .is_none_or(|previous| previous.as_str() != name)
        {
            flattened.push(name);
        }
    }
    flattened.join(" / ")
}

/// True for a BPS column-number row such as `(1) (2) (3)` or `1 2 3`: it is
/// furniture under a multi-level header, not data and not a header name.
#[must_use]
pub fn is_column_number_row(row: &[Cell]) -> bool {
    let mut numbers: Vec<i64> = Vec::new();
    let mut negative_sign: Option<bool> = None;
    for cell in row {
        let value = match cell {
            Cell::Text(text) | Cell::Date(text) => {
                let text = text.trim();
                let digits = text
                    .strip_prefix('(')
                    .unwrap_or(text)
                    .strip_suffix(')')
                    .unwrap_or(text);
                let Ok(value) = digits.parse::<i64>() else {
                    return false;
                };
                value
            }
            Cell::Number(number) if number.fract() == 0.0 => *number as i64,
            Cell::Empty => continue,
            _ => return false,
        };
        // Calamine reads `(1)` as the accounting number -1, so both signs are
        // accepted, but a row must not mix them.
        let negative = value < 0;
        if negative_sign.is_some_and(|previous| previous != negative) {
            return false;
        }
        negative_sign = Some(negative);
        let magnitude = value.abs();
        if !(1..=999).contains(&magnitude) {
            return false;
        }
        numbers.push(magnitude);
    }
    numbers.len() >= 3 && numbers.windows(2).all(|pair| pair[0] < pair[1])
}

/// Normalise every header name, name an empty one `Column N` (1-based), and
/// keep the names unique with a ` (2)`, ` (3)`… suffix.
///
/// Unique **ignoring case**: the names become DuckDB columns, and DuckDB
/// identifiers are case-insensitive, so a physics table with `U` (energy) and
/// `u` (velocity) failed the whole document with "Column with name "u" already
/// exists". The printed case is kept; only the second one gets a suffix.
#[must_use]
pub fn unique_header_names(names: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::with_capacity(names.len());
    let mut unique = Vec::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let normalized = normalize_header_name(name);
        let base = if normalized.is_empty() {
            format!("Column {}", index + 1)
        } else {
            normalized
        };
        let mut candidate = base.clone();
        let mut suffix = 2usize;
        while seen.contains(&candidate.to_lowercase()) {
            candidate = format!("{base} ({suffix})");
            suffix += 1;
        }
        seen.push(candidate.to_lowercase());
        unique.push(candidate);
    }
    unique
}

/// The text of a cell used as a label (a header or a title), or `None` when the
/// cell is empty.
fn label_of(cell: &Cell) -> Option<String> {
    let text = match cell {
        Cell::Empty => return None,
        Cell::Text(text) => text.clone(),
        Cell::Number(number) => {
            if number.fract() == 0.0 && number.abs() < 1e15 {
                format!("{}", *number as i64)
            } else {
                format!("{number}")
            }
        }
        Cell::Bool(value) => value.to_string(),
        Cell::Date(date) => date.clone(),
    };
    let text = collapse_whitespace(&text);
    (!text.is_empty()).then_some(text)
}

/// The one distinct non-empty value of a row, if it has exactly one.
#[must_use]
pub fn single_distinct_value(row: &[Cell]) -> Option<String> {
    let mut labels = row.iter().filter_map(label_of);
    let first = labels.next()?;
    labels.all(|label| label == first).then_some(first)
}

/// Remove and return the title rows at the top of a region.
///
/// A leading row whose non-empty cells all hold one distinct value is a title
/// row: that is a title merged across the table's width, or a table number
/// written in the first cell. A row with a single value is only a title when a
/// wider row follows it, so a one-column table keeps its header. Rows are taken
/// until one has two or more distinct non-empty values; that row starts the
/// header.
#[must_use]
pub fn take_title_rows(cells: &mut Vec<Vec<Cell>>) -> Vec<String> {
    let mut titles: Vec<String> = Vec::new();
    let mut consumed = 0usize;
    while let Some(row) = cells.get(consumed) {
        let Some(title) = single_distinct_value(row) else {
            break;
        };
        let lone = row.iter().filter(|cell| **cell != Cell::Empty).count() == 1;
        if lone {
            let in_first_column = row.first().is_some_and(|cell| *cell != Cell::Empty);
            if !in_first_column || !wider_row_ahead(cells, consumed + 1) {
                break;
            }
        }
        titles.push(title);
        consumed += 1;
    }
    cells.drain(..consumed);
    titles
}

/// True when some row from `from` on is wide enough to be a header or data row
/// of a table with more than one column.
fn wider_row_ahead(cells: &[Vec<Cell>], from: usize) -> bool {
    cells.get(from..).is_some_and(|rows| {
        rows.iter()
            .any(|row| row.iter().filter(|cell| **cell != Cell::Empty).count() >= 2)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {

    #[test]
    fn header_names_differing_only_in_case_are_made_unique() {
        let names: Vec<String> = ["U", "u", "v", "U"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(
            super::unique_header_names(&names),
            vec!["U", "u (2)", "v", "U (3)"]
        );
    }

    use super::*;

    fn text(value: &str) -> Cell {
        Cell::Text(value.to_string())
    }

    fn number(value: f64) -> Cell {
        Cell::Number(value)
    }

    fn row(values: &[&str]) -> Vec<Cell> {
        values.iter().map(|value| text(value)).collect()
    }

    #[test]
    fn whitespace_of_every_kind_collapses_to_one_space() {
        assert_eq!(collapse_whitespace("Bidan\n Midwife"), "Bidan Midwife");
        assert_eq!(collapse_whitespace("Kode\r\nCode"), "Kode Code");
        assert_eq!(
            collapse_whitespace("Bus\nBus / Tidak\n Umum\n Private"),
            "Bus Bus / Tidak Umum Private"
        );
        assert_eq!(
            collapse_whitespace("\u{a0}Kode\u{a0}\u{a0}Code\u{a0}"),
            "Kode Code"
        );
        assert_eq!(collapse_whitespace("2023\u{202f}/  Q1"), "2023 / Q1");
        assert_eq!(collapse_whitespace("\t\n "), "");
    }

    #[test]
    fn a_multi_level_header_joins_parts_and_drops_merged_repeats() {
        let parts: Vec<String> = ["Kangkung ", "Kangkung", "Luas Panen\n", " ( Ha ) "]
            .iter()
            .map(|part| (*part).to_string())
            .collect();
        assert_eq!(
            flatten_header_parts(&parts),
            "Kangkung / Luas Panen / ( Ha )"
        );
        assert_eq!(
            flatten_header_parts(&["Bayam".into(), "Luas Panen ( Ha )".into(), "".into()]),
            "Bayam / Luas Panen ( Ha )"
        );
    }

    #[test]
    fn a_column_number_row_is_header_furniture() {
        assert!(is_column_number_row(&[
            text("(1)"),
            text("(2)"),
            text("(3)"),
            text("(4)")
        ]));
        assert!(is_column_number_row(&[
            number(1.0),
            number(2.0),
            number(3.0)
        ]));
        assert!(!is_column_number_row(&[
            text("2018"),
            text("2019"),
            text("2020")
        ]));
        // Calamine reads `(1)` as the accounting number -1.
        assert!(is_column_number_row(&[
            number(-1.0),
            number(-2.0),
            number(-3.0)
        ]));
        assert!(!is_column_number_row(&[text("(1)"), text("(3)")]));
    }

    #[test]
    fn header_names_are_normalised_and_unique() {
        let names: Vec<String> = ["Bidan\n Midwife", "Bidan Midwife", "Kode\nCode", "", ""]
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        let unique = unique_header_names(&names);
        assert_eq!(
            unique,
            vec![
                "Bidan Midwife",
                "Bidan Midwife (2)",
                "Kode Code",
                "Column 4",
                "Column 5",
            ]
        );
    }

    #[test]
    fn a_merged_title_row_is_taken_and_the_next_row_starts_the_header() {
        let mut cells = vec![
            row(&["Tabel 25.8", "Tabel 25.8", "Tabel 25.8"]),
            row(&[
                "Data Uji Kendaraan Bermotor di Kabupaten Klaten Tahun 2018-2022",
                "Data Uji Kendaraan Bermotor di Kabupaten Klaten Tahun 2018-2022",
                "Data Uji Kendaraan Bermotor di Kabupaten Klaten Tahun 2018-2022",
            ]),
            row(&["Kode\nCode", "Kecamatan\nDistrict", "2018"]),
            row(&["331001", "Klaten Utara", "120"]),
        ];
        let titles = take_title_rows(&mut cells);
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[0], "Tabel 25.8");
        assert!(
            titles[1].starts_with("Data Uji Kendaraan Bermotor"),
            "{titles:?}"
        );
        assert_eq!(cells[0][0], text("Kode\nCode"));
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn a_lone_table_number_is_a_title_when_a_wider_row_follows() {
        let mut cells = vec![
            row(&["Tabel 25.10"]),
            row(&["Data Uji Kendaraan", "2018", "2019"]),
            row(&["Klaten Utara", "120", "130"]),
        ];
        let titles = take_title_rows(&mut cells);
        assert_eq!(titles, vec!["Tabel 25.10"]);
        assert_eq!(cells[0], row(&["Data Uji Kendaraan", "2018", "2019"]));
    }

    #[test]
    fn a_one_column_table_keeps_its_header() {
        let mut cells = vec![
            row(&["Kecamatan"]),
            row(&["Klaten Utara"]),
            row(&["Klaten Tengah"]),
        ];
        let titles = take_title_rows(&mut cells);
        assert!(titles.is_empty());
        assert_eq!(cells.len(), 3);
    }

    #[test]
    fn several_lone_titles_above_a_wide_header_are_all_taken() {
        let mut cells = vec![
            row(&["Tabel 25.39"]),
            row(&["Jumlah Penduduk Menurut Usia"]),
            row(&["Kecamatan", "2022", "2023"]),
            row(&["Klaten Utara", "120", "130"]),
        ];
        let titles = take_title_rows(&mut cells);
        assert_eq!(titles, vec!["Tabel 25.39", "Jumlah Penduduk Menurut Usia"]);
        assert_eq!(cells.len(), 2);
    }
}
