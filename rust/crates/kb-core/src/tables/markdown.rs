//! Markdown pipe tables: reading them from text, stitching page breaks,
//! dropping letterheads, and rendering a grid back to markdown (`TBL-1`).

use std::collections::HashMap;

use super::header::unique_header_names;
use super::manifest::{cell_from_text, is_placeholder, placeholder_note};
use super::{Cell, TableGrid, TableSource};

/// Parse every GitHub-style pipe table in `text`.
///
/// The table's title is the line just above its header (with any leading
/// heading markers stripped). Cells are typed here: `1.234,5` becomes
/// `Number(1234.5)`, an ISO date becomes `Date`, and everything else stays
/// `Text`.
#[must_use]
pub fn parse_markdown_tables(text: &str) -> Vec<TableGrid> {
    let lines: Vec<&str> = text.lines().collect();
    let mut grids: Vec<TableGrid> = Vec::new();
    let mut index = 0usize;
    while index + 1 < lines.len() {
        if !lines[index].contains('|') {
            index += 1;
            continue;
        }
        let header = split_row(lines[index]);
        if header.is_empty() || !is_separator(lines[index + 1]) {
            index += 1;
            continue;
        }
        let title = index
            .checked_sub(1)
            .and_then(|previous| clean_title(lines[previous]));
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        let mut placeholders = 0usize;
        index += 2;
        while index < lines.len() {
            let line = lines[index];
            if line.trim().is_empty() || !line.contains('|') {
                break;
            }
            let mut cells = split_row_counting(line, &mut placeholders);
            cells.resize(header.len(), Cell::Empty);
            cells.truncate(header.len());
            rows.push(cells);
            index += 1;
        }
        let names: Vec<String> = header.iter().map(text_of).collect();
        let mut grid = TableGrid {
            source: Some(TableSource::Markdown),
            header: unique_header_names(&names),
            header_rows: 1,
            rows,
            ..TableGrid::default()
        };
        if let Some(note) = placeholder_note(placeholders) {
            grid.notes.push(note);
        }
        grids.push(grid);
        if let Some(title) = title {
            if let Some(grid) = grids.last_mut() {
                grid.title = Some(title);
            }
        }
    }
    grids
}

/// Drop repeated letterheads/signature blocks, then join a table that
/// continues on the next page.
///
/// A fingerprint is the header plus the shape of the first data row (number,
/// text, date, bool, empty). A fingerprint seen on at least half of the
/// document's pages — and at least twice — is furniture. Two consecutive tables
/// with the same header, or with a header-shaped first row of the same width,
/// are one table.
#[must_use]
pub fn stitch_and_drop_furniture(grids: Vec<TableGrid>, page_count: u32) -> Vec<TableGrid> {
    let pages = page_count.max(1);
    // Furniture is a repetition *across pages*: one page cannot have a
    // letterhead on half of itself.
    let threshold = if pages < 2 {
        usize::MAX
    } else {
        pages.div_ceil(2).max(2) as usize
    };

    // Stitch first: a table that continues on the next page is one table, so it
    // counts once when furniture is measured. An identical repeat is not a
    // continuation (a letterhead is the same table on every page).
    let mut kept: Vec<TableGrid> = Vec::new();
    for grid in grids {
        if let Some(previous) = kept.last_mut() {
            if same_header(previous, &grid) && !identical(previous, &grid) {
                absorb(previous, grid, false);
                continue;
            }
            if previous.header.len() == grid.header.len()
                && !identical(previous, &grid)
                && first_row_is_data(&grid)
            {
                absorb(previous, grid, true);
                continue;
            }
        }
        kept.push(grid);
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    for grid in &kept {
        *counts.entry(fingerprint(grid)).or_insert(0) += 1;
    }
    let furniture: Vec<String> = counts
        .into_iter()
        .filter(|(_, seen)| *seen >= threshold)
        .map(|(key, _)| key)
        .collect();
    kept.retain(|grid| !furniture.contains(&fingerprint(grid)));
    kept
}

fn identical(left: &TableGrid, right: &TableGrid) -> bool {
    left.header == right.header && left.rows == right.rows
}

/// Render a grid as one or more markdown tables, repeating the header in every
/// group. `max_chars` is the budget for one group (`0` or `usize::MAX` means
/// one group).
#[must_use]
pub fn render_table_markdown(grid: &TableGrid, max_chars: usize) -> Vec<String> {
    if grid.header.is_empty() {
        return Vec::new();
    }
    let header = render_line(&grid.header);
    let separator = format!(
        "| {} |",
        grid.header
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let header_block = format!("{header}\n{separator}");

    let mut groups: Vec<String> = Vec::new();
    let mut current = header_block.clone();
    let mut rows_in_group = 0usize;
    for row in &grid.rows {
        let cells: Vec<String> = row.iter().map(text_of).collect();
        let line = render_line(&cells);
        let candidate = current.chars().count() + 1 + line.chars().count();
        if max_chars > 0 && rows_in_group > 0 && candidate > max_chars {
            groups.push(current);
            current = header_block.clone();
            rows_in_group = 0;
        }
        current.push('\n');
        current.push_str(&line);
        rows_in_group += 1;
    }
    groups.push(current);
    groups
}

fn absorb(previous: &mut TableGrid, mut next: TableGrid, header_is_data: bool) {
    if header_is_data {
        let mut row: Vec<Cell> = next.header.iter().map(|text| parse_cell(text)).collect();
        row.resize(previous.header.len(), Cell::Empty);
        previous.rows.push(row);
    }
    previous.rows.append(&mut next.rows);
    previous.truncated |= next.truncated;
    for note in next.notes {
        if !previous.notes.contains(&note) {
            previous.notes.push(note);
        }
    }
    let note = match (previous.page_span, next.page_span) {
        (Some((_, last)), Some((first, _))) => format!("stitched across pages {last}-{first}"),
        _ => "stitched across pages".to_string(),
    };
    if !previous.notes.iter().any(|existing| existing == &note) {
        previous.notes.push(note);
    }
    previous.page_span = match (previous.page_span, next.page_span) {
        (Some((first, _)), Some((_, last))) => Some((first, last.max(first))),
        (span, None) => span,
        (None, span) => span,
    };
}

fn same_header(left: &TableGrid, right: &TableGrid) -> bool {
    left.header.len() == right.header.len()
        && left
            .header
            .iter()
            .zip(&right.header)
            .all(|(a, b)| a.trim().eq_ignore_ascii_case(b.trim()))
}

fn first_row_is_data(grid: &TableGrid) -> bool {
    let data = grid
        .header
        .iter()
        .filter(|text| !text.trim().is_empty())
        .filter(|text| matches!(parse_cell(text), Cell::Number(_) | Cell::Date(_)))
        .count();
    let non_empty = grid
        .header
        .iter()
        .filter(|text| !text.trim().is_empty())
        .count();
    non_empty > 0 && data > 0 && data * 2 >= non_empty
}

fn fingerprint(grid: &TableGrid) -> String {
    let header = grid
        .header
        .iter()
        .map(|name| name.trim().to_lowercase())
        .collect::<Vec<_>>()
        .join("\u{1f}");
    let shape = grid
        .rows
        .first()
        .map(|row| {
            row.iter()
                .map(|cell| match cell {
                    Cell::Empty => 'e',
                    Cell::Text(_) => 't',
                    Cell::Number(_) => 'n',
                    Cell::Date(_) => 'd',
                    Cell::Bool(_) => 'b',
                })
                .collect::<String>()
        })
        .unwrap_or_default();
    format!("{header}\u{1e}{shape}")
}

fn clean_title(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.contains('|') {
        return None;
    }
    let title = trimmed.trim_start_matches('#').trim();
    (!title.is_empty()).then(|| title.to_string())
}

/// Split a pipe row into typed cells, honouring `\|`.
fn split_row(line: &str) -> Vec<Cell> {
    let mut placeholders = 0usize;
    split_row_counting(line, &mut placeholders)
}

/// Split a pipe row into typed cells and count the statistical placeholders
/// (`-`, `...`, `NA`) that were read as empty.
fn split_row_counting(line: &str, placeholders: &mut usize) -> Vec<Cell> {
    let mut text = line.trim();
    if let Some(stripped) = text.strip_prefix('|') {
        text = stripped;
    }
    if text.ends_with('|') && !text.ends_with("\\|") {
        text = &text[..text.len() - 1];
    }
    let mut cells: Vec<Cell> = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '|' {
            cells.push(parse_cell_counting(current.trim(), placeholders));
            current.clear();
        } else {
            current.push(ch);
        }
    }
    cells.push(parse_cell_counting(current.trim(), placeholders));
    cells
}

fn parse_cell_counting(text: &str, placeholders: &mut usize) -> Cell {
    if is_placeholder(text) {
        *placeholders += 1;
        return Cell::Empty;
    }
    cell_from_text(text)
}

fn parse_cell(text: &str) -> Cell {
    cell_from_text(text)
}

fn is_separator(line: &str) -> bool {
    let cells = split_row(line);
    !cells.is_empty()
        && cells.iter().all(|cell| match cell {
            Cell::Text(text) => {
                let text = text.trim();
                !text.is_empty()
                    && text.chars().all(|ch| ch == '-' || ch == ':')
                    && text.contains('-')
            }
            _ => false,
        })
}

fn text_of(cell: &Cell) -> String {
    match cell {
        Cell::Empty => String::new(),
        Cell::Text(text) => text.clone(),
        Cell::Number(number) => format_number(*number),
        Cell::Bool(value) => if *value { "true" } else { "false" }.to_string(),
        Cell::Date(date) => date.clone(),
    }
}

fn escape(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], "<br>")
}

fn render_line(cells: &[String]) -> String {
    format!(
        "| {} |",
        cells
            .iter()
            .map(|cell| escape(cell))
            .collect::<Vec<_>>()
            .join(" | ")
    )
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
    use crate::tables::Cell;

    fn table(rows: &[&str]) -> String {
        rows.join("\n")
    }

    #[test]
    fn a_pipe_table_is_parsed_with_its_title_and_typed_cells() {
        let text = table(&[
            "Tabel 3.1 Pendapatan",
            "| Provinsi | Pendapatan |",
            "| --- | ---: |",
            "| Jawa Timur | 1.234,5 |",
            "| Jawa Barat | 1,234.5 |",
        ]);
        let grids = parse_markdown_tables(&text);
        assert_eq!(grids.len(), 1);
        let grid = &grids[0];
        assert_eq!(grid.title.as_deref(), Some("Tabel 3.1 Pendapatan"));
        assert_eq!(grid.source, Some(TableSource::Markdown));
        assert_eq!(grid.header, vec!["Provinsi", "Pendapatan"]);
        assert_eq!(grid.rows[0][1], Cell::Number(1234.5));
        assert_eq!(grid.rows[1][1], Cell::Number(1234.5));
    }

    #[test]
    fn two_tables_in_one_text_are_two_grids() {
        let text = table(&[
            "First",
            "| A | B |",
            "| --- | --- |",
            "| 1 | 2 |",
            "",
            "Second",
            "| C | D |",
            "| --- | --- |",
            "| 3 | 4 |",
        ]);
        let grids = parse_markdown_tables(&text);
        assert_eq!(grids.len(), 2);
        assert_eq!(grids[0].title.as_deref(), Some("First"));
        assert_eq!(grids[1].title.as_deref(), Some("Second"));
    }

    #[test]
    fn a_table_continuing_on_the_next_page_is_stitched() {
        let page_one = table(&[
            "Tabel 1",
            "| Region | Value |",
            "| --- | --- |",
            "| A | 1 |",
            "| B | 2 |",
        ]);
        let page_two = table(&["| Region | Value |", "| --- | --- |", "| C | 3 |"]);
        let mut grids = parse_markdown_tables(&page_one);
        let mut next = parse_markdown_tables(&page_two);
        grids[0].page_span = Some((4, 4));
        next[0].page_span = Some((5, 5));
        grids.extend(next);

        let stitched = stitch_and_drop_furniture(grids, 5);
        assert_eq!(stitched.len(), 1);
        assert_eq!(stitched[0].rows.len(), 3);
        assert_eq!(stitched[0].page_span, Some((4, 5)));
        assert!(
            stitched[0]
                .notes
                .iter()
                .any(|note| note == "stitched across pages 4-5")
        );
    }

    #[test]
    fn a_headerless_continuation_of_the_same_width_is_stitched_too() {
        let page_one = table(&["| Region | Value |", "| --- | --- |", "| A | 1 |"]);
        let page_two = table(&["| B | 2 |", "| --- | --- |", "| C | 3 |"]);
        let mut grids = parse_markdown_tables(&page_one);
        grids.extend(parse_markdown_tables(&page_two));
        let stitched = stitch_and_drop_furniture(grids, 2);
        assert_eq!(stitched.len(), 1);
        assert_eq!(stitched[0].rows.len(), 3);
    }

    #[test]
    fn a_letterhead_repeated_on_every_page_is_dropped() {
        let letterhead = table(&[
            "| PT Contoh | NPWP |",
            "| --- | --- |",
            "| Jalan Satu | 01.234.567.8 |",
        ]);
        let mut grids = Vec::new();
        for pages in 1..=3 {
            let mut parsed = parse_markdown_tables(&letterhead);
            parsed[0].page_span = Some((pages, pages));
            grids.extend(parsed);
        }
        let real = table(&["| Region | Value |", "| --- | --- |", "| A | 1 |"]);
        grids.extend(parse_markdown_tables(&real));
        let kept = stitch_and_drop_furniture(grids, 3);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].header, vec!["Region", "Value"]);
    }

    #[test]
    fn rendering_repeats_the_header_in_every_row_group() {
        let mut grid = TableGrid {
            source: Some(TableSource::Csv),
            header: vec!["Region".to_string(), "Value".to_string()],
            ..TableGrid::default()
        };
        for index in 0..6 {
            grid.rows.push(vec![
                Cell::Text(format!("row-{index}")),
                Cell::Number(index as f64),
            ]);
        }
        let groups = render_table_markdown(&grid, 60);
        assert!(groups.len() > 1);
        for group in &groups {
            assert!(group.starts_with("| Region | Value |\n| --- | --- |"));
        }
    }

    #[test]
    fn pipe_cells_are_escaped_when_rendered() {
        let grid = TableGrid {
            source: Some(TableSource::Docx),
            header: vec!["Name".to_string()],
            rows: vec![vec![Cell::Text("a | b".to_string())]],
            ..TableGrid::default()
        };
        let rendered = render_table_markdown(&grid, 0);
        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains("a \\| b"));
    }

    #[test]
    fn header_names_are_normalised_and_placeholder_cells_are_counted() {
        let text = table(&[
            "Tabel 25.8",
            "| Bus\tBus | Bus\tBus | Kode\tCode |",
            "| --- | --- | --- |",
            "| - | 1.234 | 331001 |",
            "| x | ... | 331002 |",
        ]);
        let grids = parse_markdown_tables(&text);
        let grid = &grids[0];
        assert_eq!(grid.header, vec!["Bus Bus", "Bus Bus (2)", "Kode Code"]);
        assert_eq!(grid.rows[0][0], Cell::Empty);
        assert!(
            grid.notes
                .iter()
                .any(|note| note == "3 placeholder cells treated as empty"),
            "{:?}",
            grid.notes
        );
    }
}
