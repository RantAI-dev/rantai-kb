//! OTSL table markup: the grid, HTML, and markdown pipe tables.
//!
//! OTSL is the compact table markup Xiaomi-OCR-0 emits for a table region.
//! Every cell is introduced by a tag and the text following that tag belongs to
//! the cell; `<nl>` ends a row:
//!
//! ```text
//! <fcel>   cell carrying text          <ecel>   empty cell
//! <lcel>   continues the cell at left  <ucel>   continues the cell above
//! <xcel>   continues both              <nl>     end of row
//! ```
//!
//! Only `<fcel>` and `<ecel>` start a cell. The continuation tags carry no text
//! of their own: they extend an origin cell, which becomes `rowspan`/`colspan`
//! in the emitted HTML.
//!
//! **Ported from** `postprocess/otsl.py` of `xiaomi-research/Xiaomi-OCR-0`
//! (Apache-2.0), with the addition of the markdown pipe-table renderer that
//! `TBL-1`'s parser reads. See `docs/model-licences.md`.

/// Row break tag.
pub const ROW_BREAK: &str = "<nl>";
/// A cell that carries text.
pub const CELL: &str = "<fcel>";
/// An empty cell.
pub const EMPTY: &str = "<ecel>";
/// Continues the cell at the left.
pub const LEFT: &str = "<lcel>";
/// Continues the cell above.
pub const UP: &str = "<ucel>";
/// Continues both the cell at the left and the cell above.
pub const BOTH: &str = "<xcel>";

/// Every tag, longest first so a tag is never read as a prefix of another.
const ALL_TAGS: [&str; 6] = [CELL, EMPTY, LEFT, UP, BOTH, ROW_BREAK];

/// A resolved cell placed on the rectangular grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The cell's text, stripped.
    pub text: String,
    /// Row of the grid the cell starts at (0-based).
    pub row: usize,
    /// Column of the grid the cell starts at (0-based).
    pub column: usize,
    /// How many rows the cell covers.
    pub rowspan: usize,
    /// How many columns the cell covers.
    pub colspan: usize,
}

/// One cell of the padded grid: its tag and the text it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chunk {
    tag: &'static str,
    text: String,
}

/// The tag a chunk starts with, or `""` when it starts with none.
fn tag_of(chunk: &Chunk) -> &'static str {
    chunk.tag
}

/// The tag that starts at `index`, if any.
fn tag_at(line: &str, index: usize) -> Option<&'static str> {
    ALL_TAGS
        .iter()
        .copied()
        .find(|tag| line[index..].starts_with(tag))
}

/// Split a row into chunks, each holding one tag plus the text after it.
///
/// Text before the first tag is dropped: it is not part of any cell.
fn split_chunks(line: &str) -> Vec<Chunk> {
    let mut starts: Vec<usize> = Vec::new();
    for (index, ch) in line.char_indices() {
        if ch == '<' && tag_at(line, index).is_some() {
            starts.push(index);
        }
    }
    let mut chunks = Vec::with_capacity(starts.len());
    for (position, start) in starts.iter().copied().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(line.len());
        let tag = tag_at(line, start).unwrap_or("");
        let text = if tag == CELL {
            line[start + tag.len()..end].to_string()
        } else {
            String::new()
        };
        chunks.push(Chunk { tag, text });
    }
    chunks
}
/// Number of leading cells a row must keep to preserve its last content.
fn required_width(chunks: &[Chunk]) -> usize {
    let mut width = 0;
    for (position, chunk) in chunks.iter().enumerate() {
        if tag_of(chunk) == CELL {
            width = position + 1;
        }
    }
    width
}

/// Pick one cell count every row is padded or truncated to.
///
/// Every row must keep at least the cells it declared content for, and the
/// cheapest width balances the total amount of truncation and padding.
fn target_width(rows: &[Vec<Chunk>]) -> usize {
    let lower = rows
        .iter()
        .map(|row| required_width(row))
        .max()
        .unwrap_or(0);
    let upper = rows.iter().map(|row| row.len()).max().unwrap_or(0);
    let mut best_width = upper;
    let mut best_cost: Option<usize> = None;
    for width in lower..=upper {
        let cost = rows.iter().map(|row| row.len().abs_diff(width)).sum();
        if best_cost.is_none_or(|best| cost < best) {
            best_width = width;
            best_cost = Some(cost);
        }
    }
    best_width
}

/// The padded grid: every row has the same number of cells.
fn pad_rows(otsl: &str) -> Vec<Vec<Chunk>> {
    let text = crate::xiaomi_ocr::pytext::strip(otsl);
    if !text.contains(ROW_BREAK) {
        return split_chunks(text)
            .into_iter()
            .map(|chunk| vec![chunk])
            .collect();
    }

    let mut rows: Vec<Vec<Chunk>> = Vec::new();
    for line in text.split(ROW_BREAK) {
        let chunks = split_chunks(line);
        if !chunks.is_empty() {
            rows.push(chunks);
        }
    }
    if rows.is_empty() {
        return Vec::new();
    }

    let width = target_width(&rows);
    for row in &mut rows {
        row.truncate(width);
        while row.len() < width {
            row.push(Chunk {
                tag: EMPTY,
                text: String::new(),
            });
        }
    }
    rows
}

/// Turn origin tags into cells, measuring spans from the continuation tags.
fn resolve_cells(rows: &[Vec<Chunk>]) -> Vec<Cell> {
    let height = rows.len();
    let mut cells = Vec::new();

    for (row_index, tags) in rows.iter().enumerate() {
        for (column, chunk) in tags.iter().enumerate() {
            let tag = tag_of(chunk);
            if tag != CELL && tag != EMPTY {
                continue;
            }

            let mut colspan = 1;
            while column + colspan < tags.len() {
                let next = tag_of(&tags[column + colspan]);
                if next != LEFT && next != BOTH {
                    break;
                }
                colspan += 1;
            }

            let mut rowspan = 1;
            while row_index + rowspan < height {
                let below = &rows[row_index + rowspan];
                if column >= below.len() {
                    break;
                }
                let next = tag_of(&below[column]);
                if next != UP && next != BOTH {
                    break;
                }
                rowspan += 1;
            }

            let text = if tag == CELL {
                crate::xiaomi_ocr::pytext::strip(&chunk.text).to_string()
            } else {
                String::new()
            };
            cells.push(Cell {
                text,
                row: row_index,
                column,
                rowspan,
                colspan,
            });
        }
    }
    cells
}

/// The ownership grid: which cell covers each position.
fn ownership_grid(cells: &[Cell], rows: usize, columns: usize) -> Vec<Vec<Option<usize>>> {
    let mut grid = vec![vec![None; columns]; rows];
    for (index, cell) in cells.iter().enumerate() {
        let last_row = (cell.row + cell.rowspan).min(rows);
        let last_column = (cell.column + cell.colspan).min(columns);
        for row_cells in grid.iter_mut().take(last_row).skip(cell.row) {
            for slot in row_cells.iter_mut().take(last_column).skip(cell.column) {
                *slot = Some(index);
            }
        }
    }
    grid
}

/// Convert an OTSL table into an HTML `<table>` element.
///
/// Layout is walked column by column, so every row emits exactly one cell per
/// grid column and the result stays a well-formed table even when the input
/// OTSL contradicts itself. Spans are measured against the ownership grid, so
/// a span never claims a position owned by another cell.
#[must_use]
pub fn otsl_to_html(otsl: &str) -> String {
    let rows = pad_rows(otsl);
    if rows.is_empty() {
        return String::new();
    }
    let cells = resolve_cells(&rows);
    if cells.is_empty() {
        return String::new();
    }
    let num_rows = rows.len();
    let num_columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let grid = ownership_grid(&cells, num_rows, num_columns);

    let mut covered_until: Vec<usize> = vec![0; num_columns];
    let mut parts = String::new();
    for (row, row_grid) in grid.iter().enumerate() {
        parts.push_str("<tr>");
        let mut column = 0;
        while column < num_columns {
            if covered_until[column] > row {
                column += 1;
                continue;
            }
            let Some(owner) = row_grid[column] else {
                // A continuation tag with no origin of its own: keep the column
                // so the row still spans the full grid width.
                parts.push_str("<td></td>");
                column += 1;
                continue;
            };
            let cell = &cells[owner];
            if cell.row != row || cell.column != column {
                parts.push_str("<td></td>");
                column += 1;
                continue;
            }

            let mut colspan = 1;
            while column + colspan < num_columns && row_grid[column + colspan] == Some(owner) {
                colspan += 1;
            }
            let mut rowspan = 1;
            while row + rowspan < num_rows
                && (column..column + colspan)
                    .all(|offset| grid[row + rowspan][offset] == Some(owner))
            {
                rowspan += 1;
            }

            parts.push_str("<td");
            if rowspan > 1 {
                parts.push_str(&format!(" rowspan=\"{rowspan}\""));
            }
            if colspan > 1 {
                parts.push_str(&format!(" colspan=\"{colspan}\""));
            }
            parts.push('>');
            parts.push_str(&cell.text);
            parts.push_str("</td>");

            for slot in covered_until.iter_mut().skip(column).take(colspan) {
                *slot = (*slot).max(row + rowspan);
            }
            column += colspan;
        }
        parts.push_str("</tr>");
    }
    format!("<table>{parts}</table>")
}

/// Convert an OTSL table into a markdown pipe table.
///
/// A markdown table has no spans, so a cell that covers several positions is
/// repeated in each of them: the grid stays rectangular and `TBL-1`'s parser
/// reads a full row of cells. The first grid row becomes the header.
#[must_use]
pub fn otsl_to_markdown(otsl: &str) -> String {
    let rows = pad_rows(otsl);
    if rows.is_empty() {
        return String::new();
    }
    let cells = resolve_cells(&rows);
    let num_rows = rows.len();
    let num_columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if num_rows == 0 || num_columns == 0 {
        return String::new();
    }
    let grid = ownership_grid(&cells, num_rows, num_columns);

    let mut lines = Vec::with_capacity(num_rows + 1);
    for (row, row_grid) in grid.iter().enumerate() {
        let cells_in_row: Vec<String> = (0..num_columns)
            .map(|column| match row_grid[column] {
                Some(owner) => markdown_cell(&cells[owner].text),
                None => String::new(),
            })
            .collect();
        lines.push(format!("| {} |", cells_in_row.join(" | ")));
        if row == 0 {
            lines.push(format!("| {} |", vec!["---"; num_columns].join(" | ")));
        }
    }
    lines.join("\n")
}

/// Make a cell safe inside a markdown pipe table: a `|` would end the cell,
/// and a newline would end the row.
fn markdown_cell(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\n' | '\r' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_simple_table_becomes_an_html_table() {
        let html = otsl_to_html("<fcel>A<fcel>B<nl><fcel>1<fcel>2<nl>");
        assert_eq!(
            html,
            "<table><tr><td>A</td><td>B</td></tr><tr><td>1</td><td>2</td></tr></table>"
        );
    }

    #[test]
    fn a_horizontal_span_becomes_a_colspan() {
        let html = otsl_to_html("<fcel>head<fcel>b<fcel>c<nl><fcel>wide<lcel><lcel><fcel>1<nl>");
        assert_eq!(
            html,
            "<table><tr><td>head</td><td>b</td><td>c</td><td></td></tr>\
             <tr><td colspan=\"3\">wide</td><td>1</td></tr></table>"
        );
    }

    #[test]
    fn a_vertical_span_becomes_a_rowspan() {
        let html = otsl_to_html("<fcel>a<fcel>b<nl><fcel>tall<ucel><fcel>1<nl>");
        assert_eq!(
            html,
            "<table><tr><td>a</td><td rowspan=\"2\">b</td><td></td></tr>\
             <tr><td>tall</td><td>1</td></tr></table>"
        );
    }

    #[test]
    fn an_empty_cell_is_an_empty_td() {
        let html = otsl_to_html("<fcel>a<ecel>b<nl>");
        assert_eq!(html, "<table><tr><td>a</td><td></td></tr></table>");
    }

    #[test]
    fn a_markdown_table_repeats_a_spanning_cell() {
        let markdown = otsl_to_markdown("<fcel>head<fcel>b<fcel>c<nl><fcel>wide<lcel><lcel><nl>");
        assert_eq!(
            markdown,
            "| head | b | c |\n| --- | --- | --- |\n| wide | wide | wide |"
        );
    }

    #[test]
    fn a_pipe_inside_a_cell_is_escaped() {
        let markdown = otsl_to_markdown("<fcel>a|b<fcel>c<nl>");
        assert_eq!(markdown, "| a\\|b | c |\n| --- | --- |");
    }

    #[test]
    fn an_empty_markup_gives_no_table() {
        assert_eq!(otsl_to_html(""), "");
        assert_eq!(otsl_to_markdown(""), "");
    }
}
