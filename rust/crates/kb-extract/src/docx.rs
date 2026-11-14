//! `.docx` → text with its tables in place (`TBL-1`).
//!
//! Body paragraphs still become text separated by a blank line, exactly as
//! mammoth's `extractRawText` did. A table is no longer flattened into loose
//! cell paragraphs: it becomes a markdown table at its own position in the
//! document and a [`TableGrid`] in `ExtractionResult.tables`.
//!
//! Table rules from `docs/core/specs/tbl-1.md`:
//!
//! - empty cells are **kept**, so a column stays aligned (paragon dropped them
//!   and shifted every later cell left);
//! - `w:gridSpan` repeats a cell across its columns, and a `w:vMerge` continue
//!   takes the value from the cell above;
//! - the title is the paragraph just above the table.

use kb_core::tables::{Cell, TableGrid, TableSource, cell_from_text, render_table_markdown};
use quick_xml::events::{BytesStart, Event};

use crate::ExtractError;
use crate::xml;
use crate::zip_read;

/// Where the body lives inside the package.
const DOCUMENT_XML: &str = "word/document.xml";

/// Text of a `.docx`: paragraphs separated by a blank line, tables in place.
pub fn parse(bytes: &[u8]) -> Result<String, ExtractError> {
    Ok(parse_with_tables(bytes)?.0)
}

/// Text and grids of a `.docx`.
pub fn parse_with_tables(bytes: &[u8]) -> Result<(String, Vec<TableGrid>), ExtractError> {
    let mut archive = zip_read::open(bytes)?;
    let document = zip_read::read_text(&mut archive, DOCUMENT_XML)?;
    render_with_tables(&document)
}

/// The paragraph walk, separated from the container so it can be tested on XML.
#[cfg(test)]
pub fn render(document: &str) -> Result<String, ExtractError> {
    Ok(render_with_tables(document)?.0)
}

/// Text and tables of one document XML part.
pub fn render_with_tables(document: &str) -> Result<(String, Vec<TableGrid>), ExtractError> {
    let mut reader = xml::reader_from_str(document);
    let mut buffer = Vec::new();
    let mut out = String::new();
    let mut tables: Vec<TableGrid> = Vec::new();
    let mut skip_depth = 0usize;
    let mut skipped_cells: Vec<usize> = Vec::new();
    let mut deleted_paragraph = false;
    let mut paragraph_depth = 0usize;
    let mut in_paragraph_properties = 0usize;
    let mut in_run_properties = 0usize;
    let mut in_cell_properties = 0usize;
    let mut in_text = false;

    // Body text of the paragraph being read, outside any table.
    let mut body_text = String::new();
    let mut last_paragraph: Option<String> = None;

    // Table state.
    let mut in_table = false;
    let mut nested_tables = 0usize;
    let mut table_title: Option<String> = None;
    let mut row: Vec<RawCell> = Vec::new();
    let mut rows: Vec<Vec<RawCell>> = Vec::new();
    let mut cell: Option<CellAcc> = None;
    // The value still open above each column for a vertical merge.
    let mut vertical_open: Vec<Option<String>> = Vec::new();

    while let Some(event) = xml::next_event(&mut reader, &mut buffer)? {
        match event {
            Event::Start(element) => {
                let name = element.local_name();
                match name.as_ref() {
                    b"t" => in_text = true,
                    b"p" => {
                        paragraph_depth += 1;
                        deleted_paragraph = false;
                    }
                    b"pPr" => in_paragraph_properties += 1,
                    b"rPr" if in_paragraph_properties > 0 => in_run_properties += 1,
                    b"tcPr" => in_cell_properties += 1,
                    b"del" if in_run_properties > 0 => deleted_paragraph = true,
                    b"vMerge" if in_cell_properties > 0 => {
                        if continues_vertical_merge(&element) {
                            if let Some(cell) = cell.as_mut() {
                                cell.vmerge = VMerge::Continue;
                            }
                            // The cell's text belongs to the cell above it.
                            skip_depth += 1;
                            skipped_cells.push(skip_depth);
                        } else if let Some(cell) = cell.as_mut() {
                            cell.vmerge = VMerge::Restart;
                        }
                    }
                    b"gridSpan" if in_cell_properties > 0 => {
                        if let (Some(cell), Some(value)) =
                            (cell.as_mut(), xml::local_attribute(&element, "val"))
                        {
                            cell.span = value.trim().parse::<usize>().unwrap_or(1).clamp(1, 100);
                        }
                    }
                    b"tab" if skip_depth == 0 => {
                        push_char('\t', &mut cell, &mut body_text, in_table)
                    }
                    b"noBreakHyphen" if skip_depth == 0 => {
                        push_char('\u{2011}', &mut cell, &mut body_text, in_table);
                    }
                    b"softHyphen" if skip_depth == 0 => {
                        push_char('\u{00ad}', &mut cell, &mut body_text, in_table);
                    }
                    b"tbl" => {
                        if in_table {
                            nested_tables += 1;
                        } else {
                            in_table = true;
                            table_title = last_paragraph.take();
                            row.clear();
                            rows.clear();
                            vertical_open.clear();
                        }
                    }
                    b"tr" if in_table && nested_tables == 0 => row.clear(),
                    b"tc" if in_table && nested_tables == 0 => {
                        cell = Some(CellAcc::default());
                    }
                    _ => {}
                }
                if SKIP.contains(&name.as_ref()) {
                    skip_depth += 1;
                }
            }
            Event::Empty(element) => match element.local_name().as_ref() {
                b"del" if in_run_properties > 0 => deleted_paragraph = true,
                b"vMerge" if in_cell_properties > 0 => {
                    if continues_vertical_merge(&element) {
                        if let Some(cell) = cell.as_mut() {
                            cell.vmerge = VMerge::Continue;
                        }
                        skip_depth += 1;
                        skipped_cells.push(skip_depth);
                    } else if let Some(cell) = cell.as_mut() {
                        cell.vmerge = VMerge::Restart;
                    }
                }
                b"gridSpan" if in_cell_properties > 0 => {
                    if let (Some(cell), Some(value)) =
                        (cell.as_mut(), xml::local_attribute(&element, "val"))
                    {
                        cell.span = value.trim().parse::<usize>().unwrap_or(1).clamp(1, 100);
                    }
                }
                b"tab" if skip_depth == 0 => push_char('\t', &mut cell, &mut body_text, in_table),
                b"noBreakHyphen" if skip_depth == 0 => {
                    push_char('\u{2011}', &mut cell, &mut body_text, in_table);
                }
                b"softHyphen" if skip_depth == 0 => {
                    push_char('\u{00ad}', &mut cell, &mut body_text, in_table);
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_text && skip_depth == 0 && paragraph_depth > 0 {
                    push_str(&xml::text_of(&text), &mut cell, &mut body_text, in_table);
                }
            }
            Event::GeneralRef(reference) => {
                if in_text && skip_depth == 0 && paragraph_depth > 0 {
                    push_str(
                        &xml::reference_of(&String::from_utf8_lossy(&reference)),
                        &mut cell,
                        &mut body_text,
                        in_table,
                    );
                }
            }
            Event::End(element) => {
                let name = element.local_name();
                match name.as_ref() {
                    b"t" => in_text = false,
                    b"p" => {
                        paragraph_depth = paragraph_depth.saturating_sub(1);
                        if let Some(active) = cell.as_mut() {
                            if !active.text.is_empty() && !active.text.ends_with([' ', '\t']) {
                                active.text.push(' ');
                            }
                        } else if skip_depth == 0 && !deleted_paragraph {
                            out.push_str(&body_text);
                            out.push_str("\n\n");
                            let trimmed = body_text.trim();
                            if !trimmed.is_empty() {
                                last_paragraph = Some(trimmed.to_string());
                            }
                            body_text.clear();
                        }
                    }
                    b"pPr" => in_paragraph_properties = in_paragraph_properties.saturating_sub(1),
                    b"rPr" => in_run_properties = in_run_properties.saturating_sub(1),
                    b"tcPr" => in_cell_properties = in_cell_properties.saturating_sub(1),
                    b"tc" if skipped_cells.last() == Some(&skip_depth) && skip_depth > 0 => {
                        skipped_cells.pop();
                        skip_depth -= 1;
                        if let Some(finished) = cell.take() {
                            row.push(finished.into_raw());
                        }
                    }
                    b"tc" if in_table && nested_tables == 0 => {
                        if let Some(finished) = cell.take() {
                            row.push(finished.into_raw());
                        }
                    }
                    b"tr" if in_table && nested_tables == 0 => {
                        if let Some(cell) = cell.take() {
                            row.push(cell.into_raw());
                        }
                        let finished_row = finish_row(std::mem::take(&mut row), &mut vertical_open);
                        if !finished_row.is_empty() {
                            rows.push(finished_row);
                        }
                    }
                    b"tbl" => {
                        if nested_tables > 0 {
                            nested_tables -= 1;
                        } else if in_table {
                            in_table = false;
                            if let Some(grid) =
                                finish_table(std::mem::take(&mut rows), table_title.take())
                            {
                                for piece in render_table_markdown(&grid, usize::MAX) {
                                    out.push_str(&piece);
                                    out.push('\n');
                                }
                                out.push('\n');
                                tables.push(grid);
                            }
                            vertical_open.clear();
                        }
                    }
                    _ => {}
                }
                if SKIP.contains(&name.as_ref()) {
                    skip_depth = skip_depth.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
    Ok((out, tables))
}

fn push_str(text: &str, cell: &mut Option<CellAcc>, body: &mut String, in_table: bool) {
    if in_table {
        if let Some(active) = cell.as_mut() {
            active.text.push_str(text);
        }
    } else {
        body.push_str(text);
    }
}

fn push_char(ch: char, cell: &mut Option<CellAcc>, body: &mut String, in_table: bool) {
    if in_table {
        if let Some(active) = cell.as_mut() {
            active.text.push(ch);
        }
    } else {
        body.push(ch);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum VMerge {
    #[default]
    None,
    Restart,
    Continue,
}

#[derive(Debug, Default)]
struct CellAcc {
    text: String,
    span: usize,
    vmerge: VMerge,
}

impl CellAcc {
    fn into_raw(self) -> RawCell {
        RawCell {
            text: self.text.trim().to_string(),
            span: self.span.max(1),
            vmerge: self.vmerge,
        }
    }
}

struct RawCell {
    text: String,
    span: usize,
    vmerge: VMerge,
}

/// Complete a row, resolving `vMerge` continue cells from the row above and
/// repeating a `gridSpan` cell across its columns.
fn finish_row(raw: Vec<RawCell>, vertical_open: &mut Vec<Option<String>>) -> Vec<RawCell> {
    let mut row: Vec<RawCell> = Vec::new();
    let mut column = 0usize;
    for cell in raw {
        let span = cell.span.max(1);
        let value = match cell.vmerge {
            VMerge::Continue => vertical_open
                .get(column)
                .and_then(Clone::clone)
                .unwrap_or_default(),
            VMerge::Restart | VMerge::None => cell.text.clone(),
        };
        for _ in 0..span {
            if column >= vertical_open.len() {
                vertical_open.push(None);
            }
            match cell.vmerge {
                VMerge::Continue => {}
                VMerge::Restart => vertical_open[column] = Some(value.clone()),
                VMerge::None => vertical_open[column] = None,
            }
            row.push(RawCell {
                text: value.clone(),
                span: 1,
                vmerge: VMerge::None,
            });
            column += 1;
        }
    }
    row
}

fn finish_table(raw_rows: Vec<Vec<RawCell>>, title: Option<String>) -> Option<TableGrid> {
    if raw_rows.is_empty() {
        return None;
    }
    let width = raw_rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return None;
    }
    let mut rows: Vec<Vec<Cell>> = raw_rows
        .iter()
        .map(|row| {
            let mut cells: Vec<Cell> = row.iter().map(|cell| cell_from_text(&cell.text)).collect();
            cells.resize(width, Cell::Empty);
            cells
        })
        .collect();
    let header_row = rows.remove(0);
    let header = unique_names(
        header_row
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let text = cell_text(cell);
                if text.trim().is_empty() {
                    format!("Column {}", index + 1)
                } else {
                    text
                }
            })
            .collect(),
    );
    Some(TableGrid {
        source: Some(TableSource::Docx),
        sheet: None,
        page_span: None,
        title: title.filter(|value| !value.trim().is_empty()),
        caption: None,
        header,
        header_rows: 1,
        rows,
        formula_rows: Vec::new(),
        notes: Vec::new(),
        truncated: false,
    })
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

fn cell_text(cell: &Cell) -> String {
    match cell {
        Cell::Empty => String::new(),
        Cell::Text(text) => text.clone(),
        Cell::Number(number) => {
            if number.fract() == 0.0 && number.abs() < 1e15 {
                format!("{}", *number as i64)
            } else {
                format!("{number}")
            }
        }
        Cell::Bool(value) => if *value { "true" } else { "false" }.to_string(),
        Cell::Date(date) => date.clone(),
    }
}

/// A `w:vMerge` with no `w:val`, or `w:val="continue"`, means this cell shares
/// the cell above it.
fn continues_vertical_merge(element: &BytesStart<'_>) -> bool {
    match xml::local_attribute(element, "val") {
        Some(val) => val == "continue",
        None => true,
    }
}

/// Elements whose text the paragraph walk does not read. From mammoth's
/// `ignoreElements` list plus readers that return non-text elements: field
/// codes, symbols, note and comment references, drawings and section
/// properties.
const SKIP: &[&[u8]] = &[
    b"del",
    b"instrText",
    b"fldChar",
    b"sym",
    b"drawing",
    b"wp:inline",
    b"wp:anchor",
    b"br",
    b"footnoteReference",
    b"endnoteReference",
    b"commentReference",
    b"bookmarkStart",
    b"bookmarkEnd",
    b"tblGrid",
    b"proofErr",
    b"annotationRef",
    b"footnoteRef",
    b"endnoteRef",
    b"lastRenderedPageBreak",
    b"sectPr",
    b"commentRangeStart",
    b"commentRangeEnd",
    b"office-word:wrap",
    b"v:shadow",
    b"v:shapetype",
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::testkit::docx_of;

    #[test]
    fn paragraphs_are_separated_by_a_blank_line() {
        let text = render(
            r#"<w:document xmlns:w="urn:w"><w:body>
                 <w:p><w:r><w:t>First paragraph.</w:t></w:r></w:p>
                 <w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p>
               </w:body></w:document>"#,
        )
        .unwrap();
        assert_eq!(text, "First paragraph.\n\nSecond paragraph.\n\n");
    }

    #[test]
    fn headings_are_plain_paragraphs_in_raw_text() {
        let text = render(
            r#"<w:document xmlns:w="urn:w"><w:body><w:p><w:pPr>
                 <w:pStyle w:val="Heading1"/></w:pPr>
                 <w:r><w:t>Chapter one</w:t></w:r></w:p></w:body></w:document>"#,
        )
        .unwrap();
        assert_eq!(text, "Chapter one\n\n");
    }

    #[test]
    fn runs_tabs_and_entities_join_into_one_paragraph() {
        let text = render(
            r#"<w:document xmlns:w="urn:w"><w:body><w:p>
                 <w:r><w:t>Tom &amp; Jerry</w:t><w:tab/><w:t>tabbed</w:t></w:r>
                 <w:r><w:br/><w:t>after break</w:t></w:r>
               </w:p></w:body></w:document>"#,
        )
        .unwrap();
        assert_eq!(text, "Tom & Jerry\ttabbedafter break\n\n");
    }

    #[test]
    fn list_items_are_paragraphs_too() {
        let text = render(
            r#"<w:document xmlns:w="urn:w"><w:body>
                 <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/>
                 </w:numPr></w:pPr><w:r><w:t>one</w:t></w:r></w:p>
                 <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/>
                 </w:numPr></w:pPr><w:r><w:t>two</w:t></w:r></w:p>
               </w:body></w:document>"#,
        )
        .unwrap();
        assert_eq!(text, "one\n\ntwo\n\n");
    }

    #[test]
    fn a_table_renders_in_place_as_markdown() {
        let document = r#"<w:document xmlns:w="urn:w"><w:body>
             <w:p><w:r><w:t>Revenue by region</w:t></w:r></w:p>
             <w:tbl>
               <w:tr><w:tc><w:p><w:r><w:t>Region</w:t></w:r></w:p></w:tc>
                     <w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr>
               <w:tr><w:tc><w:p><w:r><w:t>Jawa Timur</w:t></w:r></w:p></w:tc>
                     <w:tc><w:p><w:r><w:t>120</w:t></w:r></w:p></w:tc></w:tr>
             </w:tbl>
             <w:p><w:r><w:t>After the table.</w:t></w:r></w:p>
           </w:body></w:document>"#;
        let (text, tables) = render_with_tables(document).unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].header, vec!["Region", "Value"]);
        assert_eq!(tables[0].title.as_deref(), Some("Revenue by region"));
        assert_eq!(tables[0].rows[0][1], Cell::Number(120.0));
        assert!(
            text.contains("Revenue by region\n\n| Region | Value |"),
            "{text}"
        );
        assert!(
            text.contains("| Jawa Timur | 120 |\n\nAfter the table.\n\n"),
            "{text}"
        );
    }

    #[test]
    fn an_empty_cell_keeps_its_column() {
        let document = r#"<w:document xmlns:w="urn:w"><w:body><w:tbl>
             <w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc>
                   <w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc>
                   <w:tc><w:p><w:r><w:t>C</w:t></w:r></w:p></w:tc></w:tr>
             <w:tr><w:tc><w:p><w:r><w:t>1</w:t></w:r></w:p></w:tc>
                   <w:tc><w:p/></w:tc>
                   <w:tc><w:p><w:r><w:t>3</w:t></w:r></w:p></w:tc></w:tr>
           </w:tbl></w:body></w:document>"#;
        let (_, tables) = render_with_tables(document).unwrap();
        let grid = &tables[0];
        assert_eq!(grid.rows[0][0], Cell::Number(1.0));
        assert_eq!(grid.rows[0][1], Cell::Empty);
        assert_eq!(grid.rows[0][2], Cell::Number(3.0));
    }

    #[test]
    fn a_grid_span_repeats_and_a_vertical_merge_continues() {
        let document = r#"<w:document xmlns:w="urn:w"><w:body><w:tbl>
             <w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>Group</w:t></w:r></w:p></w:tc>
                   <w:tc><w:p><w:r><w:t>C</w:t></w:r></w:p></w:tc></w:tr>
             <w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>merged</w:t></w:r></w:p></w:tc>
                   <w:tc><w:p><w:r><w:t>side</w:t></w:r></w:p></w:tc>
                   <w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr>
             <w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc>
                   <w:tc><w:p><w:r><w:t>side again</w:t></w:r></w:p></w:tc>
                   <w:tc><w:p><w:r><w:t>y</w:t></w:r></w:p></w:tc></w:tr>
           </w:tbl></w:body></w:document>"#;
        let (_, tables) = render_with_tables(document).unwrap();
        let grid = &tables[0];
        assert_eq!(grid.header, vec!["Group", "Group (2)", "C"]);
        assert_eq!(grid.rows[0][0], Cell::Text("merged".to_string()));
        assert_eq!(grid.rows[1][0], Cell::Text("merged".to_string()));
    }

    #[test]
    fn field_codes_and_notes_contribute_nothing() {
        let text = render(
            r#"<w:document xmlns:w="urn:w"><w:body><w:p>
                 <w:r><w:fldChar w:fldCharType="begin"/></w:r>
                 <w:r><w:instrText>PAGE \* MERGEFORMAT</w:instrText></w:r>
                 <w:r><w:t>7</w:t></w:r>
                 <w:r><w:footnoteReference w:id="1"/></w:r>
               </w:p></w:body></w:document>"#,
        )
        .unwrap();
        assert_eq!(text, "7\n\n");
    }

    #[test]
    fn a_tracked_deletion_is_merged_into_the_next_paragraph() {
        let text = render(
            r#"<w:document xmlns:w="urn:w"><w:body>
                 <w:p><w:pPr><w:rPr><w:del/></w:rPr></w:pPr>
                   <w:r><w:t>deleted words</w:t></w:r></w:p>
                 <w:p><w:r><w:t>kept words</w:t></w:r></w:p>
               </w:body></w:document>"#,
        )
        .unwrap();
        assert_eq!(text, "deleted wordskept words\n\n");
    }

    #[test]
    fn a_package_without_document_xml_is_a_container_error() {
        let err = parse(&docx_of(&[])).unwrap_err();
        assert!(matches!(err, ExtractError::Container(_)), "{err}");
    }

    #[test]
    fn text_is_read_out_of_a_real_package() {
        let bytes = docx_of(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="urn:w"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p>
                 <w:p><w:r><w:t>docx</w:t></w:r></w:p></w:body></w:document>"#,
        )]);
        assert_eq!(parse(&bytes).unwrap(), "Hello\n\ndocx\n\n");
    }
}
