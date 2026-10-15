//! `.xlsx` → text: one CSV block per sheet.
//!
//! The TypeScript path is `XLSX.utils.sheet_to_csv(sheet, { FS: ",",
//! blankrows: false })` per sheet, under a `[Spreadsheet: name]` heading
//! (`src/lib/files/parsers.ts:50`). Reading the workbook with `calamine` and
//! writing the CSV here keeps SheetJS's row rules rather than inventing new
//! ones:
//!
//! - a row with no value in any cell is dropped (`blankrows: false`);
//! - a cell that is missing still occupies its column, so trailing empties leave
//!   trailing commas, exactly as `make_csv_row` does;
//! - a value containing a comma, a newline or a quote is quoted, and quotes are
//!   doubled.
//!
//! The one thing not reproduced is SheetJS's number formatting: a cell's own
//! format (`w`) is applied there, so a currency or date cell keeps its display
//! form. Here numbers are rendered plainly and dates as ISO, which is a
//! documented difference, not an oversight.

use std::io::Cursor;

use calamine::{Data, Range, Reader, Sheets, open_workbook_auto_from_rs};

use crate::ExtractError;

/// Field and record separators, as passed in the TypeScript call.
const FS: char = ',';
const RS: &str = "\n";

/// Text of a spreadsheet: a heading, then one block per non-empty sheet.
pub fn parse(bytes: &[u8], file_name: &str) -> Result<String, ExtractError> {
    let mut workbook: Sheets<Cursor<&[u8]>> = open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|e| ExtractError::Container(format!("not a spreadsheet: {e}")))?;

    let mut sections: Vec<String> = Vec::new();
    for (name, range) in workbook.worksheets() {
        let csv = sheet_to_csv(&range);
        if !csv.trim().is_empty() {
            sections.push(format!("[Sheet: {name}]\n{csv}"));
        }
    }

    if sections.is_empty() {
        Ok(format!("[Spreadsheet: {file_name}]\n\n(empty)"))
    } else {
        Ok(format!(
            "[Spreadsheet: {file_name}]\n\n{}",
            sections.join("\n\n")
        ))
    }
}

/// One sheet as CSV, with SheetJS's empty-row and quoting rules.
fn sheet_to_csv(range: &Range<Data>) -> String {
    let mut rows: Vec<String> = Vec::new();
    for row in range.rows() {
        let cells: Vec<String> = row.iter().map(cell_text).collect();
        // `blankrows: false`: a row where nothing has a value is skipped, which
        // is also how SheetJS's `isempty` flag drops it.
        if cells.iter().all(String::is_empty) {
            continue;
        }
        rows.push(join_row(&cells));
    }
    rows.join(RS)
}

fn join_row(cells: &[String]) -> String {
    cells
        .iter()
        .map(|cell| quote_cell(cell))
        .collect::<Vec<_>>()
        .join(&FS.to_string())
}

/// SheetJS quotes when the value contains the field or record separator or a
/// quote, and separately hard-codes the literal `ID` as quoted — Excel's own
/// CSV writer does the same, and the quirk shows up in real exports.
fn quote_cell(value: &str) -> String {
    if value == "ID" {
        return "\"ID\"".to_string();
    }
    let needs_quotes = value.contains(FS) || value.contains('\n') || value.contains('"');
    if !needs_quotes {
        return value.to_string();
    }
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::Int(value) => value.to_string(),
        Data::Float(value) => format_number(*value),
        Data::String(value) => value.clone(),
        Data::Bool(value) => {
            if *value {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }
        }
        // Dates and durations are rendered from the stored value rather than
        // from the cell's display format.
        Data::DateTime(value) => value.to_string(),
        Data::DateTimeIso(value) | Data::DurationIso(value) => value.clone(),
        Data::Error(value) => value.to_string(),
    }
}

/// JavaScript's `String(number)` for the values a spreadsheet holds: integers
/// without a fraction, fractions in the shortest form, and the same words for
/// the non-numbers.
fn format_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    // Rust prints 42.0 as "42" and 1.5 as "1.5", which is what JS does too.
    format!("{value}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::testkit::{empty_worksheet, worksheet_with_gap, xlsx_of};

    #[test]
    fn numbers_and_booleans_render_like_javascript() {
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(-0.25), "-0.25");
        assert_eq!(format_number(f64::NAN), "NaN");
    }

    #[test]
    fn quoting_follows_sheetjs() {
        assert_eq!(quote_cell("plain"), "plain");
        assert_eq!(quote_cell("a,b"), "\"a,b\"");
        assert_eq!(quote_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(quote_cell("line\nbreak"), "\"line\nbreak\"");
        assert_eq!(quote_cell("ID"), "\"ID\"");
    }

    #[test]
    fn a_missing_cell_keeps_its_column() {
        // The first row has values in A and C, so B still contributes its
        // separator: SheetJS emits one field per column of the sheet's range.
        let bytes = xlsx_of(
            &[("Summary", worksheet_with_gap())],
            &["name", "value", "alpha"],
        );
        let text = parse(&bytes, "book.xlsx").unwrap();
        assert_eq!(
            text,
            "[Spreadsheet: book.xlsx]\n\n[Sheet: Summary]\nname,,value\nalpha,,42"
        );
    }

    #[test]
    fn empty_sheets_are_left_out_and_an_empty_book_says_so() {
        let bytes = xlsx_of(&[("Empty", empty_worksheet())], &[]);
        assert_eq!(
            parse(&bytes, "book.xlsx").unwrap(),
            "[Spreadsheet: book.xlsx]\n\n(empty)"
        );
    }

    #[test]
    fn every_sheet_gets_its_own_block() {
        let bytes = xlsx_of(
            &[
                ("First", worksheet_with_gap()),
                ("Second", empty_worksheet()),
            ],
            &["name", "value", "alpha"],
        );
        let text = parse(&bytes, "book.xlsx").unwrap();
        assert_eq!(
            text,
            "[Spreadsheet: book.xlsx]\n\n[Sheet: First]\nname,,value\nalpha,,42"
        );
    }

    #[test]
    fn bytes_that_are_not_a_spreadsheet_are_a_container_error() {
        let err = parse(b"not a spreadsheet", "book.xlsx").unwrap_err();
        assert!(matches!(err, ExtractError::Container(_)), "{err}");
    }
}
