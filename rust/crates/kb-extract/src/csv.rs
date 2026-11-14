//! `.csv` / `.tsv` → one markdown table and one grid.
//!
//! The delimiter is sniffed from the first line (`,`, `;`, tab or `|`), unless
//! the file extension already says tab. The first row is the header; quoted
//! fields may hold the delimiter or a newline, as RFC 4180 allows.

use kb_core::tables::{Cell, TableGrid, TableSource, cell_from_text, render_table_markdown};

use crate::ExtractError;
use crate::text::to_utf8;

const CANDIDATES: [char; 4] = [',', ';', '\t', '|'];

/// Text and grid of a delimited text file.
pub fn parse_with_tables(
    bytes: &[u8],
    file_name: &str,
    delimiter: Option<char>,
) -> Result<(String, Vec<TableGrid>), ExtractError> {
    let text = to_utf8(bytes);
    let delimiter = delimiter.unwrap_or_else(|| sniff_delimiter(&text));
    let mut rows = parse_rows(&text, delimiter);
    if rows.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let header: Vec<String> = rows
        .remove(0)
        .into_iter()
        .map(|cell| cell.trim().to_string())
        .collect();
    if header.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let width = header.len();
    let data: Vec<Vec<Cell>> = rows
        .into_iter()
        .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()))
        .map(|mut row| {
            row.resize(width, String::new());
            row.truncate(width);
            row.iter().map(|cell| cell_from_text(cell)).collect()
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
        notes: Vec::new(),
        truncated: false,
    };
    let content = render_table_markdown(&grid, usize::MAX).join("\n\n");
    Ok((content, vec![grid]))
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
}
