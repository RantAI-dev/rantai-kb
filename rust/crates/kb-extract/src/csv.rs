//! `.csv` / `.tsv` → one markdown table and one grid.
//!
//! The delimiter is sniffed from the first line (`,`, `;`, tab or `|`), unless
//! the file extension already says tab. The first row is the header; quoted
//! fields may hold the delimiter or a newline, as RFC 4180 allows.

use kb_core::tables::{
    Cell, TableGrid, TableSource, cell_from_text, flatten_header_parts, is_placeholder,
    placeholder_note, render_table_markdown, unique_header_names,
};

use crate::ExtractError;
use crate::text::to_utf8;

const CANDIDATES: [char; 4] = [',', ';', '\t', '|'];

/// Text and grid of a delimited text file.
///
/// A column that is empty in the header *and* in every row is not a column: the
/// World Bank CSVs end every line with a comma, and that trailing empty field
/// must not reach the store as a zero-length identifier. A column that has data
/// but no header is named `Column N`.
pub fn parse_with_tables(
    bytes: &[u8],
    file_name: &str,
    delimiter: Option<char>,
) -> Result<(String, Vec<TableGrid>), ExtractError> {
    let text = to_utf8(bytes);
    // A World Bank export starts with a UTF-8 BOM before `"Data Source"`;
    // without stripping it the first header name keeps the quote characters.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let delimiter = delimiter.unwrap_or_else(|| sniff_delimiter(text));
    let rows = parse_rows(text, delimiter);
    if rows.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let (header_index, levels) = find_header(&rows);
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let header_row = &rows[header_index];
    let raw_rows: Vec<Vec<String>> = rows
        .iter()
        .skip(header_index + levels)
        .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()))
        .map(|row| {
            let mut row = row.clone();
            row.resize(width, String::new());
            row.truncate(width);
            row
        })
        .collect();
    let keep: Vec<usize> = (0..width)
        .filter(|column| {
            !header_row
                .get(*column)
                .is_some_and(|cell| cell.trim().is_empty())
                || raw_rows.iter().any(|row| !row[*column].trim().is_empty())
        })
        .collect();
    if keep.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let names: Vec<String> = keep
        .iter()
        .map(|column| {
            let parts: Vec<String> = rows
                .iter()
                .skip(header_index)
                .take(levels)
                .map(|row| row.get(*column).cloned().unwrap_or_default())
                .collect();
            flatten_header_parts(&parts)
        })
        .collect();
    let header = unique_header_names(&names);
    let mut placeholders = 0usize;
    let data: Vec<Vec<Cell>> = raw_rows
        .iter()
        .map(|row| {
            keep.iter()
                .map(|column| {
                    let text = row[*column].as_str();
                    if is_placeholder(text) {
                        placeholders += 1;
                    }
                    cell_from_text(text)
                })
                .collect()
        })
        .collect();

    let title = file_name
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .map(ToString::to_string);
    let grid = TableGrid {
        source: Some(TableSource::Csv),
        sheet: None,
        page_span: None,
        title,
        caption: None,
        header,
        header_rows: 1,
        rows: data,
        formula_rows: Vec::new(),
        notes: placeholder_note(placeholders).into_iter().collect(),
        truncated: false,
    };
    let content = render_table_markdown(&grid, usize::MAX).join("\n\n");
    Ok((content, vec![grid]))
}

/// Find the header row and how many levels it spans.
///
/// The World Bank's downloads put `"Data Source", ...` and
/// `"Last Updated Date", ...` above the real header, separated by blank lines;
/// taking the first row as the header names those columns `Column N`. Score
/// the candidate rows by how many labels they hold, and require numeric data
/// underneath. One export repeats the country name and code under the header
/// before its data: that second label row is a header level, flattened as
/// `Country Name / Aruba`.
fn find_header(rows: &[Vec<String>]) -> (usize, usize) {
    let label = |cell: &str| {
        let text = cell.trim();
        if text.is_empty() {
            return true;
        }
        match cell_from_text(text) {
            Cell::Number(number) => number.fract() == 0.0 && (1900.0..=2100.0).contains(&number),
            Cell::Text(_) | Cell::Date(_) | Cell::Bool(_) => true,
            Cell::Empty => true,
        }
    };
    let non_empty = |row: &Vec<String>| row.iter().filter(|cell| !cell.trim().is_empty()).count();
    let labels = |row: &Vec<String>| {
        row.iter()
            .filter(|cell| !cell.trim().is_empty() && label(cell))
            .count()
    };
    let numbers = |row: &Vec<String>| {
        row.iter()
            .filter(|cell| matches!(cell_from_text(cell), Cell::Number(number) if !(number.fract() == 0.0 && (1900.0..=2100.0).contains(&number))))
            .count()
    };

    let mut best: Option<(f64, usize)> = None;
    for (index, row) in rows.iter().enumerate().take(25) {
        let filled = non_empty(row);
        if filled < 2 || labels(row) < (2 * filled).div_ceil(3) {
            continue;
        }
        if !rows
            .iter()
            .skip(index + 1)
            .take(4)
            .any(|following| numbers(following) >= 2)
        {
            continue;
        }
        let score = 2.0 * filled as f64 + labels(row) as f64 - 0.6 * index as f64;
        if best.is_none_or(|(previous, _)| score > previous) {
            best = Some((score, index));
        }
    }
    let header = best.map_or(0, |(_, index)| index);
    let mut levels = 1usize;
    if let Some(next) = rows.get(header + 1) {
        let filled = non_empty(next);
        if filled >= 2
            && numbers(next) == 0
            && labels(next) >= (7 * filled).div_ceil(10)
            && rows.get(header + 2).is_some_and(|row| numbers(row) >= 2)
        {
            levels = 2;
        }
    }
    (header, levels)
}

/// Which candidate delimiter appears most often on the first non-empty line.
fn sniff_delimiter(text: &str) -> char {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut best = (',', 0usize);
    for candidate in CANDIDATES {
        let count = first.matches(candidate).count();
        if count > best.1 {
            best = (candidate, count);
        }
    }
    best.0
}

fn parse_rows(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => in_quotes = true,
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                finish_row(&mut rows, &mut row, &mut field);
            }
            '\n' => finish_row(&mut rows, &mut row, &mut field),
            ch if ch == delimiter => {
                row.push(std::mem::take(&mut field));
            }
            _ => field.push(ch),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        finish_row(&mut rows, &mut row, &mut field);
    }
    rows
}

fn finish_row(rows: &mut Vec<Vec<String>>, row: &mut Vec<String>, field: &mut String) {
    row.push(std::mem::take(field));
    rows.push(std::mem::take(row));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// TBL-2's header search must not move the header of an ordinary CSV whose
    /// data is all text (no numeric rows to anchor a later candidate).
    #[test]
    fn a_text_only_csv_keeps_its_first_row_as_the_header() {
        let (_, grids) = parse_with_tables(
            b"Nama,Jabatan,Unit\nAni,Kepala,Keuangan\nBudi,Staf,SDM\n",
            "staff.csv",
            None,
        )
        .unwrap();
        assert_eq!(grids[0].header, vec!["Nama", "Jabatan", "Unit"]);
        assert_eq!(grids[0].rows.len(), 2);
    }

    #[test]
    fn a_comma_csv_is_one_grid_with_a_header() {
        let (content, grids) = parse_with_tables(
            b"Region,Value\nJawa Timur,\"1.234,5\"\nJawa Barat,12\n",
            "data.csv",
            None,
        )
        .unwrap();
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].header, vec!["Region", "Value"]);
        assert_eq!(grids[0].rows[0][1], Cell::Number(1234.5));
        assert_eq!(grids[0].title.as_deref(), Some("data.csv"));
        assert!(content.starts_with("| Region | Value |\n| --- | --- |"));
    }

    #[test]
    fn a_semicolon_csv_is_sniffed() {
        let (_, grids) = parse_with_tables(b"Region;Value\nA;1\nB;2\n", "data.csv", None).unwrap();
        assert_eq!(grids[0].header, vec!["Region", "Value"]);
        assert_eq!(grids[0].rows.len(), 2);
    }

    #[test]
    fn a_tsv_uses_the_tab_hint_and_keeps_empty_cells_aligned() {
        let (_, grids) =
            parse_with_tables(b"Region\tValue\tNote\nA\t1\t\n", "data.tsv", Some('\t')).unwrap();
        assert_eq!(grids[0].header, vec!["Region", "Value", "Note"]);
        assert_eq!(grids[0].rows[0][2], Cell::Empty);
    }

    #[test]
    fn a_quoted_field_may_hold_the_delimiter_and_a_newline() {
        let (_, grids) = parse_with_tables(
            b"Name,Note\n\"A, B\",\"line one\nline two\"\n",
            "data.csv",
            None,
        )
        .unwrap();
        assert_eq!(grids[0].rows[0][0], Cell::Text("A, B".to_string()));
        assert_eq!(
            grids[0].rows[0][1],
            Cell::Text("line one\nline two".to_string())
        );
    }

    #[test]
    fn a_world_bank_trailing_comma_column_is_dropped() {
        // Each line ends with a comma, so the last header and every last cell
        // are empty. That column must not become a zero-length identifier.
        let (content, grids) = parse_with_tables(
            b"Country Name,Country Code,Indicator Name,1960,1961,\n\
              Indonesia,IDN,Population,88,90,\n\
              Malaysia,MYS,Population,30,31,\n",
            "wb.csv",
            None,
        )
        .unwrap();
        assert_eq!(
            grids[0].header,
            vec![
                "Country Name",
                "Country Code",
                "Indicator Name",
                "1960",
                "1961"
            ]
        );
        assert_eq!(grids[0].rows[0].len(), 5);
        assert_eq!(grids[0].rows[1][4], Cell::Number(31.0));
        assert!(!content.contains("|  |"), "{content}");
    }

    #[test]
    fn a_world_bank_preamble_is_skipped_and_year_headers_stay_text() {
        let csv = b"\xEF\xBB\xBF\"Data Source\",\"World Development Indicators\",\n\n\
                    \"Last Updated Date\",\"2026-11-11\",\n\n\
                    \"Country Name\",\"Country Code\",\"Indicator Name\",\"1960\",\"2024\",\n\
                    \"Aruba\",\"ABW\",\"Population, total\",\"10\",\"20\",\n\
                    \"Indonesia\",\"IDN\",\"Population, total\",\"30\",\"40\",\n";
        let (_, grids) = parse_with_tables(csv, "wb.csv", None).unwrap();
        assert_eq!(grids.len(), 1);
        assert_eq!(
            grids[0].header,
            vec![
                "Country Name",
                "Country Code",
                "Indicator Name",
                "1960",
                "2024"
            ]
        );
        assert_eq!(grids[0].rows.len(), 2);
        assert_eq!(grids[0].rows[0][0], Cell::Text("Aruba".to_string()));
    }

    #[test]
    fn a_second_label_row_is_flattened_into_the_header() {
        let csv = b"\"Data Source\",\"World Development Indicators\",\n\n\
                    \"Country Name\",\"Country Code\",\"Indicator Name\",\"1960\",\"2024\",\n\
                    \"Aruba\",\"ABW\",\"Unemployment, total\",\"\",\"\",\n\
                    \"Indonesia\",\"IDN\",\"Unemployment, total\",\"12\",\"15\",\n";
        let (_, grids) = parse_with_tables(csv, "wb.csv", None).unwrap();
        assert_eq!(
            grids[0].header,
            vec![
                "Country Name / Aruba",
                "Country Code / ABW",
                "Indicator Name / Unemployment, total",
                "1960",
                "2024"
            ]
        );
        assert_eq!(grids[0].rows.len(), 1);
        assert_eq!(grids[0].rows[0][4], Cell::Number(15.0));
    }

    #[test]
    fn a_column_with_data_but_no_header_is_named_by_its_position() {
        let (_, grids) =
            parse_with_tables(b"Region,,Value\nA,x,1\nB,y,2\n", "data.csv", None).unwrap();
        assert_eq!(grids[0].header, vec!["Region", "Column 2", "Value"]);
    }

    #[test]
    fn a_csv_placeholder_is_empty_and_noted() {
        let (_, grids) =
            parse_with_tables(b"Region,2018,2019\nA,-,2\nB,3,4\n", "data.csv", None).unwrap();
        assert_eq!(grids[0].rows[0][1], Cell::Empty);
        assert!(
            grids[0]
                .notes
                .iter()
                .any(|note| note == "1 placeholder cells treated as empty"),
            "{:?}",
            grids[0].notes
        );
    }
}
