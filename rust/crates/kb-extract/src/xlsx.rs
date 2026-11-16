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
    Cell, MAX_TABLE_ROWS, TableGrid, TableSource, cell_from_text, is_placeholder,
    normalize_header_name, placeholder_note, render_table_markdown, single_distinct_value,
    take_title_rows, unique_header_names,
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
    let (matrix, placeholders) = sheet_matrix(&sheet.range);
    let formula_cells = formula_cells(&sheet.formulas);
    let mut grids: Vec<TableGrid> = Vec::new();
    // A title row alone between blank rows belongs to the table below it, not
    // to a one-column table of its own.
    let mut carried_titles: Vec<String> = Vec::new();

    let regions = regions(&matrix);
    for (index, (rows, columns)) in regions.iter().enumerate() {
        let more_regions = index + 1 < regions.len();
        let Some(mut grid) = region_grid(
            source,
            &sheet.name,
            &matrix,
            &placeholders,
            rows,
            columns,
            &sheet.merges,
            &formula_cells,
            more_regions,
            &mut carried_titles,
        ) else {
            continue;
        };
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

/// The used part of a range as a rectangle of typed cells, plus a rectangle
/// that marks the cells whose text was a statistical placeholder (`-`, `...`,
/// `NA`) read as empty.
fn sheet_matrix(range: &Range<Data>) -> (Vec<Vec<Cell>>, Vec<Vec<bool>>) {
    let (Some((start_row, start_col)), Some((end_row, end_col))) = (range.start(), range.end())
    else {
        return (Vec::new(), Vec::new());
    };
    let mut matrix = Vec::new();
    let mut placeholders = Vec::new();
    for row in start_row..=end_row {
        let mut cells = Vec::new();
        let mut flags = Vec::new();
        for column in start_col..=end_col {
            let value = range.get_value((row, column));
            flags.push(matches!(value, Some(Data::String(text)) if is_placeholder(text)));
            cells.push(value.map_or(Cell::Empty, cell_from_data));
        }
        matrix.push(cells);
        placeholders.push(flags);
    }
    (matrix, placeholders)
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

#[allow(clippy::too_many_arguments)]
fn region_grid(
    source: TableSource,
    sheet_name: &str,
    matrix: &[Vec<Cell>],
    placeholders: &[Vec<bool>],
    rows: &std::ops::Range<usize>,
    columns: &std::ops::Range<usize>,
    merges: &[Dimensions],
    formula_cells: &[(u32, u32)],
    more_regions: bool,
    carried_titles: &mut Vec<String>,
) -> Option<TableGrid> {
    let mut cells: Vec<Vec<Cell>> = rows
        .clone()
        .map(|row| matrix[row][columns.clone()].to_vec())
        .collect();
    spread_merges(&mut cells, rows, columns, merges);

    let mut titles = std::mem::take(carried_titles);
    let taken = take_title_rows(&mut cells);
    let title_rows = taken.len();
    titles.extend(taken);
    if cells.is_empty() {
        *carried_titles = titles;
        return None;
    }
    // One row holding one distinct value, alone between blank rows: that is the
    // next table's title (BPS puts "Tabel 25.8" above a blank line), not a
    // one-column table.
    if cells.len() == 1 && more_regions {
        if let Some(title) = single_distinct_value(&cells[0]) {
            titles.push(title);
            *carried_titles = titles;
            return None;
        }
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
        rows.start + title_rows,
        header_rows,
        columns.start,
        &grid_rows,
        formula_cells,
    );
    let mut placeholder_count = 0usize;
    for row in (rows.start + title_rows + header_rows)..rows.end {
        for column in columns.clone() {
            if placeholders
                .get(row)
                .and_then(|flags| flags.get(column))
                .copied()
                .unwrap_or(false)
            {
                placeholder_count += 1;
            }
        }
    }

    Some(TableGrid {
        source: Some(source),
        sheet: Some(sheet_name.to_string()),
        page_span: None,
        title: if titles.is_empty() {
            Some(sheet_name.to_string())
        } else {
            Some(titles.join(" — "))
        },
        caption: None,
        header,
        header_rows: header_rows as u32,
        rows: grid_rows,
        formula_rows,
        notes: placeholder_note(placeholder_count).into_iter().collect(),
        truncated: false,
    })
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
        // A year row is a header level too: a year read as a number is a
        // label, not data. Any other number in the second row means the row
        // below the first is data.
        let second_row_labels = cells[1].iter().all(|cell| match cell {
            Cell::Empty | Cell::Text(_) | Cell::Date(_) | Cell::Bool(_) => true,
            Cell::Number(number) => number.fract() == 0.0 && (1900.0..=2100.0).contains(number),
        });
        let third_row_data = cells[2].iter().any(|cell| matches!(cell, Cell::Number(_)));
        if (adjacent_duplicates || first_gap_filled) && second_row_labels && third_row_data {
            header_rows = 2;
        }
    }

    let header: Vec<String> = (0..width)
        .map(|column| {
            let mut parts: Vec<String> = Vec::new();
            for row in cells.iter().take(header_rows) {
                let text = row.get(column).map_or(String::new(), cell_text);
                let text = normalize_header_name(&text);
                if !text.is_empty()
                    && parts
                        .last()
                        .is_none_or(|previous| previous.as_str() != text)
                {
                    parts.push(text);
                }
            }
            parts.join(" / ")
        })
        .collect();
    (header_rows, unique_header_names(&header))
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
    use kb_core::tables::{ColumnKind, ColumnRole, Confidence, build_manifest};
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

    /// A BPS Klaten sheet as the portal publishes it: a table number, a merged
    /// title, a two-row bilingual header with line breaks, a kode column, year
    /// columns with `-` placeholders, and a "Kabupaten Klaten" total row that
    /// holds the column sums without a formula.
    #[test]
    fn a_bps_sheet_is_titled_normalised_quantified_and_its_total_is_derived() {
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet();
        let format = Format::default();
        sheet.write_string(0, 0, "Tabel 25.8").unwrap();
        sheet
            .merge_range(
                1,
                0,
                1,
                6,
                "Data Uji Kendaraan Bermotor di Kabupaten Klaten Tahun 2018-2022",
                &format,
            )
            .unwrap();
        sheet.write_string(2, 0, "Kode\nCode").unwrap();
        sheet.write_string(2, 1, "Kecamatan\nDistrict").unwrap();
        sheet
            .merge_range(2, 2, 2, 6, "Kendaraan Bermotor\nMotor Vehicles", &format)
            .unwrap();
        for (column, year) in (2018..=2022).enumerate() {
            sheet
                .write_string(3, column as u16 + 2, year.to_string())
                .unwrap();
        }
        let regions = [
            ("331001", "Klaten Utara", ["120", "-", "150", "160", "170"]),
            ("331002", "Klaten Tengah", ["130", "140", "—", "150", "160"]),
            (
                "331003",
                "Klaten Selatan",
                ["140", "150", "160", "170", "180"],
            ),
        ];
        for (offset, (code, name, values)) in regions.iter().enumerate() {
            let row = (4 + offset) as u32;
            sheet.write_string(row, 0, *code).unwrap();
            sheet.write_string(row, 1, *name).unwrap();
            for (column, value) in values.iter().enumerate() {
                match value.parse::<f64>() {
                    Ok(number) => sheet.write_number(row, column as u16 + 2, number).unwrap(),
                    Err(_) => sheet.write_string(row, column as u16 + 2, *value).unwrap(),
                };
            }
        }
        // The total row carries the sums; no formula, and the label is the
        // region, not "Jumlah".
        sheet
            .merge_range(7, 0, 7, 1, "Kabupaten Klaten", &format)
            .unwrap();
        for (column, total) in [390.0, 290.0, 310.0, 480.0, 510.0].iter().enumerate() {
            sheet.write_number(7, column as u16 + 2, *total).unwrap();
        }

        let bytes = workbook.save_to_buffer().unwrap();
        let sheet_data = parse_with_tables(&bytes, "bps.xlsx").unwrap();
        assert_eq!(sheet_data.tables.len(), 1, "{:?}", sheet_data.tables);
        let grid = &sheet_data.tables[0];
        assert_eq!(
            grid.title.as_deref(),
            Some("Tabel 25.8 — Data Uji Kendaraan Bermotor di Kabupaten Klaten Tahun 2018-2022")
        );
        assert_eq!(
            grid.header,
            vec![
                "Kode Code",
                "Kecamatan District",
                "Kendaraan Bermotor Motor Vehicles / 2018",
                "Kendaraan Bermotor Motor Vehicles / 2019",
                "Kendaraan Bermotor Motor Vehicles / 2020",
                "Kendaraan Bermotor Motor Vehicles / 2021",
                "Kendaraan Bermotor Motor Vehicles / 2022",
            ]
        );
        assert_eq!(grid.header_rows, 2);
        assert_eq!(grid.rows.len(), 4);
        assert_eq!(grid.rows[0][3], Cell::Empty);
        assert_eq!(grid.rows[1][4], Cell::Empty);
        assert!(
            grid.notes
                .iter()
                .any(|note| note == "2 placeholder cells treated as empty"),
            "{:?}",
            grid.notes
        );
        assert!(!sheet_data.content.contains("Kode\nCode"));

        let manifest = build_manifest(grid);
        assert_eq!(manifest.columns[0].role, ColumnRole::Code);
        assert_eq!(manifest.columns[1].role, ColumnRole::Identifier);
        for column in &manifest.columns[2..7] {
            assert_eq!(column.kind, ColumnKind::Number);
            assert_eq!(
                column.role,
                ColumnRole::Quantity,
                "{} is a year of numbers, not Mixed",
                column.name
            );
        }
        assert_eq!(manifest.derived_rows, vec![3]);
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

    /// The table number sits above a blank line in many BPS exports. It is the
    /// next table's title, not a one-column table of its own.
    #[test]
    fn a_title_row_alone_between_blank_rows_titles_the_table_below_it() {
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet();
        sheet.write_string(0, 0, "Tabel 25.39").unwrap();
        for (column, value) in ["Kecamatan", "2018", "2019"].iter().enumerate() {
            sheet.write_string(2, column as u16, *value).unwrap();
        }
        sheet.write_string(3, 0, "Klaten Utara").unwrap();
        sheet.write_number(3, 1, 1.0).unwrap();
        sheet.write_number(3, 2, 2.0).unwrap();
        sheet.write_string(4, 0, "Klaten Tengah").unwrap();
        sheet.write_number(4, 1, 3.0).unwrap();
        sheet.write_number(4, 2, 4.0).unwrap();

        let bytes = workbook.save_to_buffer().unwrap();
        let sheet_data = parse_with_tables(&bytes, "bps.xlsx").unwrap();
        assert_eq!(sheet_data.tables.len(), 1, "{:?}", sheet_data.tables);
        assert_eq!(sheet_data.tables[0].title.as_deref(), Some("Tabel 25.39"));
        assert_eq!(
            sheet_data.tables[0].header,
            vec!["Kecamatan", "2018", "2019"]
        );
        assert_eq!(sheet_data.tables[0].rows.len(), 2);
    }
}
