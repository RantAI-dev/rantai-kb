//! `.xlsx` / `.xls` / `.ods` → markdown tables and structured grids.
//!
//! `TBL-1` replaced the old CSV flattening: each sheet is split into table
//! regions by blank rows and columns, merged cells are spread, a multi-level
//! header is flattened with " / ", hidden sheets are skipped (with a note) and
//! cached formula values are used. The text is the same grids rendered as
//! markdown tables, so a reader (and the chunker) sees the header of every
//! table.
//!
//! Typing follows the contract: a numeric cell becomes `Cell::Number` (a text
//! cell holding `1.234,5` is read with the locale rule), a date cell becomes
//! `Cell::Date`, and a text cell that is a zero-padded digit string stays
//! `Cell::Text` so a NIK keeps its leading zeros.

use std::io::Cursor;

use calamine::{
    Data, Dimensions, Range, Reader, Sheet, SheetType, SheetVisible, Sheets, Xls, XlsError,
    open_workbook_auto_from_rs,
};
use kb_core::tables::{
    Cell, MAX_TABLE_ROWS, TableGrid, TableSource, cell_from_text, render_table_markdown,
};

use crate::ExtractError;

/// The text and grids of one workbook.
#[derive(Debug, Clone, Default)]
pub struct Spreadsheet {
    pub content: String,
    pub tables: Vec<TableGrid>,
}

/// Text of a spreadsheet: a heading, then markdown tables per sheet.
pub fn parse(bytes: &[u8], file_name: &str) -> Result<String, ExtractError> {
    Ok(parse_with_tables(bytes, file_name)?.content)
}

/// Text and grids of a workbook whose format is detected from its bytes.
pub fn parse_with_tables(bytes: &[u8], file_name: &str) -> Result<Spreadsheet, ExtractError> {
    let mut workbook: Sheets<Cursor<&[u8]>> = open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|e| ExtractError::Container(format!("not a spreadsheet: {e}")))?;
    let source = match &workbook {
        Sheets::Xlsx(_) => TableSource::Xlsx,
        Sheets::Xls(_) => TableSource::Xls,
        Sheets::Ods(_) => TableSource::Ods,
        Sheets::Xlsb(_) => TableSource::Xlsx,
    };
    Ok(render_auto(&mut workbook, file_name, source))
}

/// Text and grids of a BIFF `.xls` (Excel 5.0 to 2003) from an OLE compound
/// file.
///
/// Opened with calamine's `Xls` reader directly rather than its format
/// auto-detection, which replaces every reason with "Cannot detect file
/// format": here a password-protected or damaged workbook says so.
pub fn parse_xls_with_tables(bytes: &[u8], file_name: &str) -> Result<Spreadsheet, ExtractError> {
    let mut workbook: Xls<Cursor<&[u8]>> = Xls::new(Cursor::new(bytes)).map_err(|e| match e {
        XlsError::Password => ExtractError::Unsupported(
            "the workbook is password-protected; remove the password and upload it again"
                .to_string(),
        ),
        other => ExtractError::Container(format!("not a readable Excel 97-2003 workbook: {other}")),
    })?;
    let sheets = collect_xls_sheets(&mut workbook);
    Ok(assemble(file_name, TableSource::Xls, sheets))
}

/// Text of a legacy `.xls` workbook (the compatibility entry point).
pub fn parse_xls(bytes: &[u8], file_name: &str) -> Result<String, ExtractError> {
    Ok(parse_xls_with_tables(bytes, file_name)?.content)
}

/// One sheet's raw material, before regions are split and headers flattened.
struct RawSheet {
    name: String,
    range: Range<Data>,
    formulas: Range<String>,
    merges: Vec<Dimensions>,
}

fn render_auto(
    workbook: &mut Sheets<Cursor<&[u8]>>,
    file_name: &str,
    source: TableSource,
) -> Spreadsheet {
    let metadata = workbook.sheets_metadata().to_vec();
    let mut notes: Vec<String> = Vec::new();
    let mut sheets: Vec<RawSheet> = Vec::new();
    for sheet in &metadata {
        if !include_sheet(sheet, &mut notes) {
            continue;
        }
        let range = match workbook.worksheet_range(&sheet.name) {
            Ok(range) => range,
            Err(error) => {
                notes.push(format!("sheet {} could not be read: {error}", sheet.name));
                continue;
            }
        };
        let formulas = workbook.worksheet_formula(&sheet.name).unwrap_or_default();
        let merges = match workbook {
            Sheets::Xlsx(book) => book
                .merge_cells_by_sheet_name(&sheet.name)
                .unwrap_or_default(),
            Sheets::Xls(book) => book
                .merge_cells_by_sheet_name(&sheet.name)
                .unwrap_or_default(),
            Sheets::Xlsb(_) => Vec::new(),
            Sheets::Ods(_) => Vec::new(),
        };
        sheets.push(RawSheet {
            name: sheet.name.clone(),
            range,
            formulas,
            merges,
        });
    }
    assemble_with_notes(file_name, source, sheets, notes)
}

fn collect_xls_sheets(workbook: &mut Xls<Cursor<&[u8]>>) -> Vec<RawSheet> {
    let metadata = workbook.sheets_metadata().to_vec();
    let mut sheets = Vec::new();
    for sheet in &metadata {
        if sheet.visible != SheetVisible::Visible || sheet.typ != SheetType::WorkSheet {
            continue;
        }
        let Ok(range) = workbook.worksheet_range(&sheet.name) else {
            continue;
        };
        let formulas = workbook.worksheet_formula(&sheet.name).unwrap_or_default();
        let merges = workbook
            .merge_cells_by_sheet_name(&sheet.name)
            .unwrap_or_default();
        sheets.push(RawSheet {
            name: sheet.name.clone(),
            range,
            formulas,
            merges,
        });
    }
    sheets
}

/// A chart sheet, dialog sheet or a hidden sheet is not data.
fn include_sheet(sheet: &Sheet, notes: &mut Vec<String>) -> bool {
    if sheet.typ != SheetType::WorkSheet {
        return false;
    }
    match sheet.visible {
        SheetVisible::Visible => true,
        SheetVisible::Hidden | SheetVisible::VeryHidden => {
            notes.push(format!("hidden sheet skipped: {}", sheet.name));
            false
        }
    }
}

fn assemble(file_name: &str, source: TableSource, sheets: Vec<RawSheet>) -> Spreadsheet {
    assemble_with_notes(file_name, source, sheets, Vec::new())
}

fn assemble_with_notes(
    file_name: &str,
    source: TableSource,
    sheets: Vec<RawSheet>,
    notes: Vec<String>,
) -> Spreadsheet {
    let mut sections: Vec<String> = Vec::new();
    let mut tables: Vec<TableGrid> = Vec::new();
    for sheet in sheets {
        let mut grids = grids_for_sheet(source, &sheet);
        if grids.is_empty() {
            continue;
        }
        for grid in &mut grids {
            for note in &notes {
                if !grid.notes.contains(note) {
                    grid.notes.push(note.clone());
                }
            }
        }
        sections.push(render_sheet(&sheet.name, &grids));
        tables.append(&mut grids);
    }

    let content = if sections.is_empty() {
        format!("[Spreadsheet: {file_name}]\n\n(empty)")
    } else {
        format!("[Spreadsheet: {file_name}]\n\n{}", sections.join("\n\n"))
    };
    Spreadsheet { content, tables }
}

fn render_sheet(sheet_name: &str, grids: &[TableGrid]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for grid in grids {
        if let Some(title) = &grid.title {
            if !title.is_empty() && title != sheet_name {
                blocks.push(title.clone());
            }
        }
        blocks.extend(render_table_markdown(grid, usize::MAX));
    }
    format!("[Sheet: {sheet_name}]\n{}", blocks.join("\n\n"))
}

fn grids_for_sheet(source: TableSource, sheet: &RawSheet) -> Vec<TableGrid> {
    let matrix = sheet_matrix(&sheet.range);
    let formula_cells = formula_cells(&sheet.formulas);
    let mut grids: Vec<TableGrid> = Vec::new();

    for (rows, columns) in regions(&matrix) {
        let mut grid = region_grid(
            source,
            &sheet.name,
            &matrix,
            &rows,
            &columns,
            &sheet.merges,
            &formula_cells,
        );
        if grid.header.iter().all(String::is_empty) {
            continue;
        }
        if grid.rows.len() as u32 > MAX_TABLE_ROWS {
            grid.rows.truncate(MAX_TABLE_ROWS as usize);
            grid.truncated = true;
        }
        grids.push(grid);
    }
    grids
}

/// The used part of a range as a rectangle of typed cells.
fn sheet_matrix(range: &Range<Data>) -> Vec<Vec<Cell>> {
    let (Some((start_row, start_col)), Some((end_row, end_col))) = (range.start(), range.end())
    else {
        return Vec::new();
    };
    let mut matrix = Vec::new();
    for row in start_row..=end_row {
        let mut cells = Vec::new();
        for column in start_col..=end_col {
            cells.push(
                range
                    .get_value((row, column))
                    .map_or(Cell::Empty, cell_from_data),
            );
        }
        matrix.push(cells);
    }
    matrix
}

fn formula_cells(range: &Range<String>) -> Vec<(u32, u32)> {
    let Some((start_row, start_col)) = range.start() else {
        return Vec::new();
    };
    let mut cells = Vec::new();
    for (row_offset, row) in range.rows().enumerate() {
        for (column_offset, formula) in row.iter().enumerate() {
            if !formula.trim().is_empty() {
                cells.push((
                    start_row + row_offset as u32,
                    start_col + column_offset as u32,
                ));
            }
        }
    }
    cells
}

fn cell_from_data(data: &Data) -> Cell {
    match data {
        Data::Empty => Cell::Empty,
        Data::Int(value) => Cell::Number(*value as f64),
        Data::Float(value) => Cell::Number(*value),
        Data::String(value) => cell_from_text(value),
        Data::Bool(value) => Cell::Bool(*value),
        Data::DateTime(value) => match value.as_datetime() {
            Some(datetime) => Cell::Date(datetime.format("%Y-%m-%d").to_string()),
            None => Cell::Number(value.as_f64()),
        },
        Data::DateTimeIso(value) => Cell::Date(value.clone()),
        Data::DurationIso(value) => Cell::Text(value.clone()),
        Data::Error(value) => Cell::Text(value.to_string()),
    }
}

/// Split a sheet matrix into table regions: runs of non-empty rows, and inside
/// each run runs of non-empty columns. Both ranges are half-open.
fn regions(matrix: &[Vec<Cell>]) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let rows = matrix.len();
    let columns = matrix.first().map_or(0, Vec::len);
    let row_has_data = |row: usize| (0..columns).any(|column| matrix[row][column] != Cell::Empty);
    let column_has_data = |column: usize, start: usize, end: usize| {
        (start..end).any(|row| matrix[row][column] != Cell::Empty)
    };

    let mut regions = Vec::new();
    let mut row = 0usize;
    while row < rows {
        if !row_has_data(row) {
            row += 1;
            continue;
        }
        let row_start = row;
        while row < rows && row_has_data(row) {
            row += 1;
        }
        let row_end = row;

        let mut column = 0usize;
        while column < columns {
            if !column_has_data(column, row_start, row_end) {
                column += 1;
                continue;
            }
            let column_start = column;
            while column < columns && column_has_data(column, row_start, row_end) {
                column += 1;
            }
            regions.push((row_start..row_end, column_start..column));
        }
    }
    regions
}

fn region_grid(
    source: TableSource,
    sheet_name: &str,
    matrix: &[Vec<Cell>],
    rows: &std::ops::Range<usize>,
    columns: &std::ops::Range<usize>,
    merges: &[Dimensions],
    formula_cells: &[(u32, u32)],
) -> TableGrid {
    let mut cells: Vec<Vec<Cell>> = rows
        .clone()
        .map(|row| matrix[row][columns.clone()].to_vec())
        .collect();
    spread_merges(&mut cells, rows, columns, merges);

    let mut title = None;
    let mut title_offset = 0usize;
    if let Some(title_index) = title_row(&cells) {
        title = Some(cell_text(&cells[title_index][0]));
        cells.remove(title_index);
        title_offset = 1;
    }

    let (header_rows, header) = header_and_rows(&cells);
    let data = &cells[header_rows.min(cells.len())..];
    let width = header.len();
    let grid_rows: Vec<Vec<Cell>> = data
        .iter()
        .map(|row| {
            let mut row = row.clone();
            row.resize(width, Cell::Empty);
            row.truncate(width);
            row
        })
        .collect();

    let formula_rows = formula_rows(
        rows.start + title_offset,
        header_rows,
        columns.start,
        &grid_rows,
        formula_cells,
    );

    TableGrid {
        source: Some(source),
        sheet: Some(sheet_name.to_string()),
        page_span: None,
        title: title.or_else(|| Some(sheet_name.to_string())),
        caption: None,
        header,
        header_rows: header_rows as u32,
        rows: grid_rows,
        formula_rows,
        notes: Vec::new(),
        truncated: false,
    }
}

/// Fill every cell of a merged span with the span's top-left value, clipped to
/// the region.
fn spread_merges(
    cells: &mut [Vec<Cell>],
    rows: &std::ops::Range<usize>,
    columns: &std::ops::Range<usize>,
    merges: &[Dimensions],
) {
    for merged in merges {
        let start_row = merged.start.0 as usize;
        let start_column = merged.start.1 as usize;
        let end_row = merged.end.0 as usize;
        let end_column = merged.end.1 as usize;
        if !rows.contains(&start_row) || !columns.contains(&start_column) {
            continue;
        }
        let value = cells[start_row - rows.start][start_column - columns.start].clone();
        if value == Cell::Empty {
            continue;
        }
        for row in start_row..=end_row.min(rows.end.saturating_sub(1)) {
            for column in start_column..=end_column.min(columns.end.saturating_sub(1)) {
                cells[row - rows.start][column - columns.start] = value.clone();
            }
        }
    }
}

/// A title row is a leading row whose non-empty cells are all the same text
/// (a merged title spread by [`spread_merges`]), or a single cell above a
/// wider header.
fn title_row(cells: &[Vec<Cell>]) -> Option<usize> {
    if cells.len() < 2 {
        return None;
    }
    let first = &cells[0];
    let non_empty: Vec<&Cell> = first.iter().filter(|cell| **cell != Cell::Empty).collect();
    if non_empty.is_empty() {
        return None;
    }
    let all_same = non_empty.iter().all(|cell| cell == &non_empty[0]);
    let width = non_empty.len();
    let next_non_empty = cells[1].iter().filter(|cell| **cell != Cell::Empty).count();
    if all_same && (width >= 2 || next_non_empty >= 2) {
        // A one-cell first row is a title only when the header is wider.
        if width == 1 && next_non_empty < 2 {
            return None;
        }
        return Some(0);
    }
    None
}

/// Decide how many leading rows are headers and flatten them.
fn header_and_rows(cells: &[Vec<Cell>]) -> (usize, Vec<String>) {
    if cells.is_empty() {
        return (0, Vec::new());
    }
    let width = cells.iter().map(Vec::len).max().unwrap_or(0);
    let mut header_rows = 1usize;
    if cells.len() >= 3 {
        let first = &cells[0];
        let adjacent_duplicates = first
            .windows(2)
            .any(|pair| pair[0] != Cell::Empty && pair[0] == pair[1]);
        let first_gap_filled = (0..width).any(|column| {
            first.get(column).is_none_or(|cell| *cell == Cell::Empty)
                && cells[1]
                    .get(column)
                    .is_some_and(|cell| *cell != Cell::Empty)
        });
        let second_row_text = cells[1].iter().all(|cell| {
            matches!(
                cell,
                Cell::Empty | Cell::Text(_) | Cell::Date(_) | Cell::Bool(_)
            )
        });
        let third_row_data = cells[2].iter().any(|cell| matches!(cell, Cell::Number(_)));
        if (adjacent_duplicates || first_gap_filled) && second_row_text && third_row_data {
            header_rows = 2;
        }
    }

    let header: Vec<String> = (0..width)
        .map(|column| {
            let mut parts: Vec<String> = Vec::new();
            for row in cells.iter().take(header_rows) {
                let text = row.get(column).map_or(String::new(), cell_text);
                let text = text.trim();
                if !text.is_empty()
                    && parts
                        .last()
                        .is_none_or(|previous| previous.as_str() != text)
                {
                    parts.push(text.to_string());
                }
            }
            if parts.is_empty() {
                format!("Column {}", column + 1)
            } else {
                parts.join(" / ")
            }
        })
        .collect();
    (header_rows, unique_names(header))
}

fn unique_names(names: Vec<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut unique = Vec::with_capacity(names.len());
    for name in names {
        let base = name.clone();
        let mut candidate = name;
        let mut suffix = 2usize;
        while seen.iter().any(|existing| existing == &candidate) {
            candidate = format!("{base} ({suffix})");
            suffix += 1;
        }
        seen.push(candidate.clone());
        unique.push(candidate);
    }
    unique
}

/// Data-row indices whose numeric cells carry a formula (absolute sheet rows).
fn formula_rows(
    region_row_start: usize,
    header_rows: usize,
    region_column_start: usize,
    rows: &[Vec<Cell>],
    formula_cells: &[(u32, u32)],
) -> Vec<u32> {
    let mut derived = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let absolute_row = region_row_start + header_rows + index;
        let has_formula_number = row.iter().enumerate().any(|(column, cell)| {
            let absolute_column = region_column_start + column;
            matches!(cell, Cell::Number(_))
                && formula_cells
                    .iter()
                    .any(|(r, c)| *r as usize == absolute_row && *c as usize == absolute_column)
        });
        if has_formula_number {
            derived.push(index as u32);
        }
    }
    derived
}

fn cell_text(cell: &Cell) -> String {
    match cell {
        Cell::Empty => String::new(),
        Cell::Text(text) => text.clone(),
        Cell::Number(number) => format_number(*number),
        Cell::Bool(value) => if *value { "true" } else { "false" }.to_string(),
        Cell::Date(date) => date.clone(),
    }
}

fn format_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use rust_xlsxwriter::{Format, Workbook};

    #[test]
    fn two_regions_a_merged_title_formulas_and_a_hidden_sheet() {
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet();
        sheet.set_name("Data").unwrap();
        let format = Format::default();
        sheet
            .merge_range(0, 0, 0, 3, "Pendapatan per Provinsi", &format)
            .unwrap();
        for (column, value) in ["Provinsi", "2023", "2024", "Kode"].iter().enumerate() {
            sheet.write_string(1, column as u16, *value).unwrap();
        }
        sheet.write_string(2, 0, "Jawa Timur").unwrap();
        sheet.write_number(2, 1, 100.0).unwrap();
        sheet.write_number(2, 2, 110.0).unwrap();
        sheet.write_string(2, 3, "01").unwrap();
        sheet.write_string(3, 0, "Jawa Barat").unwrap();
        sheet.write_number(3, 1, 90.0).unwrap();
        sheet.write_number(3, 2, 95.0).unwrap();
        sheet.write_string(3, 3, "02").unwrap();
        sheet.write_string(4, 0, "Jumlah").unwrap();
        sheet.write_formula(4, 1, "=SUM(B3:B4)").unwrap();
        sheet.set_formula_result(4, 1, "190");
        sheet.write_formula(4, 2, "=SUM(C3:C4)").unwrap();
        sheet.set_formula_result(4, 2, "205");

        // A second region, below a blank row.
        sheet.write_string(6, 0, "Kode").unwrap();
        sheet.write_string(6, 1, "Nilai").unwrap();
        sheet.write_string(7, 0, "01").unwrap();
        sheet.write_number(7, 1, 5.0).unwrap();

        let hidden = workbook.add_worksheet();
        hidden.set_name("Rahasia").unwrap();
        hidden.set_hidden(true);
        hidden.write_string(0, 0, "rahasia").unwrap();

        let bytes = workbook.save_to_buffer().unwrap();
        let sheet_data = parse_with_tables(&bytes, "book.xlsx").unwrap();
        assert_eq!(sheet_data.tables.len(), 2, "{:?}", sheet_data.tables);

        let first = &sheet_data.tables[0];
        assert_eq!(first.title.as_deref(), Some("Pendapatan per Provinsi"));
        assert_eq!(first.header, vec!["Provinsi", "2023", "2024", "Kode"]);
        assert_eq!(first.rows.len(), 3);
        assert_eq!(first.rows[2][0], Cell::Text("Jumlah".to_string()));
        assert_eq!(first.rows[2][1], Cell::Number(190.0));
        assert_eq!(first.formula_rows, vec![2]);
        assert!(
            first
                .notes
                .iter()
                .any(|note| note == "hidden sheet skipped: Rahasia"),
            "{:?}",
            first.notes
        );

        let second = &sheet_data.tables[1];
        assert_eq!(second.header, vec!["Kode", "Nilai"]);
        assert!(!sheet_data.content.contains("rahasia"));
        assert!(
            sheet_data
                .content
                .contains("| Provinsi | 2023 | 2024 | Kode |")
        );
    }

    #[test]
    fn a_multi_level_header_is_flattened_with_a_slash() {
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet();
        let format = Format::default();
        sheet.merge_range(0, 0, 1, 0, "Provinsi", &format).unwrap();
        sheet.merge_range(0, 1, 0, 2, "2023", &format).unwrap();
        sheet.merge_range(0, 3, 0, 4, "2024", &format).unwrap();
        for (column, value) in ["Q1", "Q2", "Q1", "Q2"].iter().enumerate() {
            sheet.write_string(1, column as u16 + 1, *value).unwrap();
        }
        sheet.write_string(2, 0, "Jawa Timur").unwrap();
        for column in 1..5u16 {
            sheet.write_number(2, column, f64::from(column)).unwrap();
        }

        let bytes = workbook.save_to_buffer().unwrap();
        let sheet_data = parse_with_tables(&bytes, "book.xlsx").unwrap();
        assert_eq!(sheet_data.tables.len(), 1);
        assert_eq!(
            sheet_data.tables[0].header,
            vec![
                "Provinsi",
                "2023 / Q1",
                "2023 / Q2",
                "2024 / Q1",
                "2024 / Q2"
            ]
        );
        assert_eq!(sheet_data.tables[0].header_rows, 2);
    }

    #[test]
    fn a_text_number_uses_the_indonesian_convention_and_a_code_keeps_its_zeros() {
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet();
        sheet.write_string(0, 0, "NIK").unwrap();
        sheet.write_string(0, 1, "Pendapatan").unwrap();
        sheet.write_string(1, 0, "0123456789012345").unwrap();
        sheet.write_string(1, 1, "1.234,5").unwrap();

        let bytes = workbook.save_to_buffer().unwrap();
        let sheet_data = parse_with_tables(&bytes, "book.xlsx").unwrap();
        let grid = &sheet_data.tables[0];
        assert_eq!(grid.rows[0][0], Cell::Text("0123456789012345".to_string()));
        assert_eq!(grid.rows[0][1], Cell::Number(1234.5));
    }

    #[test]
    fn bytes_that_are_not_a_spreadsheet_are_a_container_error() {
        let err = parse(b"not a spreadsheet", "book.xlsx").unwrap_err();
        assert!(matches!(err, ExtractError::Container(_)), "{err}");
    }
}
