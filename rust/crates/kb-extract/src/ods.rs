//! Parser for OpenDocument Spreadsheet (.ods) documents.
//!
//! ODS files are zip archives containing `content.xml`. In OpenDocument format,
//! spreadsheets contain `<table:table>` elements, each with `<table:table-row>`
//! elements and `<table:table-cell>` elements. Cell text lives inside `<text:p>`.
//!
//! Matching officeparser's behaviour, rows are converted to CSV-like lines
//! (comma-delimited with quotes when commas/quotes/newlines appear in cells),
//! joined by newlines.

use quick_xml::events::Event;

use crate::ExtractError;
use crate::xml;
use crate::zip_read;

/// Parse plain text from an ODS file's raw bytes.
pub fn parse_ods(bytes: &[u8]) -> Result<String, ExtractError> {
    let mut archive = zip_read::open(bytes)?;
    let content_xml = zip_read::read_text(&mut archive, "content.xml")?;
    parse_ods_content_xml(&content_xml)
}

/// Extract spreadsheet table rows as CSV-like lines from ODS `content.xml`.
pub fn parse_ods_content_xml(xml_str: &str) -> Result<String, ExtractError> {
    let mut reader = xml::reader_from_str(xml_str);

    let mut lines: Vec<String> = Vec::new();
    let mut current_row_cells: Vec<String> = Vec::new();
    let mut current_cell_paragraphs: Vec<String> = Vec::new();
    let mut current_p = String::new();

    let mut in_row = false;
    let mut in_cell = false;
    let mut in_p = false;

    let mut row_repeated = 1usize;
    let mut col_repeated = 1usize;

    let mut buf = Vec::new();

    while let Some(event) = xml::next_event(&mut reader, &mut buf)? {
        match event {
            Event::Start(ref e) => {
                let name = e.local_name();
                match name.as_ref() {
                    b"table-row" => {
                        in_row = true;
                        current_row_cells.clear();
                        row_repeated = 1;
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"number-rows-repeated" {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    if let Ok(n) = val.parse::<usize>() {
                                        row_repeated = n.min(100);
                                    }
                                }
                            }
                        }
                    }
                    b"table-cell" | b"covered-table-cell" => {
                        if in_row {
                            in_cell = true;
                            current_cell_paragraphs.clear();
                            col_repeated = 1;
                            for attr in e.attributes().flatten() {
                                if attr.key.local_name().as_ref() == b"number-columns-repeated" {
                                    if let Ok(val) = std::str::from_utf8(&attr.value) {
                                        if let Ok(n) = val.parse::<usize>() {
                                            col_repeated = n.min(100);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    b"p" => {
                        if in_cell {
                            in_p = true;
                            current_p.clear();
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(ref e) => {
                let name = e.local_name();
                if in_p {
                    if name.as_ref() == b"s" {
                        let mut count = 1usize;
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"c" {
                                if let Ok(val) = std::str::from_utf8(&attr.value) {
                                    if let Ok(c) = val.parse::<usize>() {
                                        count = c;
                                    }
                                }
                            }
                        }
                        for _ in 0..count {
                            current_p.push(' ');
                        }
                    } else if name.as_ref() == b"tab" {
                        current_p.push('\t');
                    } else if name.as_ref() == b"line-break" {
                        current_p.push('\n');
                    }
                } else if in_row
                    && (name.as_ref() == b"table-cell" || name.as_ref() == b"covered-table-cell")
                {
                    // Empty cell
                    let mut cols = 1usize;
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == b"number-columns-repeated" {
                            if let Ok(val) = std::str::from_utf8(&attr.value) {
                                if let Ok(n) = val.parse::<usize>() {
                                    cols = n.min(10);
                                }
                            }
                        }
                    }
                    for _ in 0..cols {
                        current_row_cells.push(String::new());
                    }
                }
            }
            Event::Text(ref text) => {
                if in_p {
                    current_p.push_str(&xml::text_of(text));
                }
            }
            Event::GeneralRef(ref r) => {
                if in_p {
                    current_p.push_str(&xml::reference_of(&String::from_utf8_lossy(r)));
                }
            }
            Event::CData(ref cdata) => {
                if in_p {
                    if let Ok(s) = std::str::from_utf8(cdata.as_ref()) {
                        current_p.push_str(s);
                    }
                }
            }
            Event::End(ref e) => {
                let name = e.local_name();
                match name.as_ref() {
                    b"p" => {
                        if in_p {
                            in_p = false;
                            current_cell_paragraphs.push(current_p.trim().to_string());
                            current_p.clear();
                        }
                    }
                    b"table-cell" | b"covered-table-cell" => {
                        if in_cell {
                            in_cell = false;
                            let cell_text = current_cell_paragraphs.join(" ");
                            let cols = if cell_text.is_empty() {
                                col_repeated.min(10)
                            } else {
                                col_repeated
                            };
                            for _ in 0..cols {
                                current_row_cells.push(cell_text.clone());
                            }
                            current_cell_paragraphs.clear();
                        }
                    }
                    b"table-row" => {
                        if in_row {
                            in_row = false;
                            // Trim trailing empty cells from row
                            while current_row_cells.last().is_some_and(String::is_empty) {
                                current_row_cells.pop();
                            }
                            if !current_row_cells.is_empty() {
                                let row_csv = format_csv_row(&current_row_cells);
                                for _ in 0..row_repeated {
                                    lines.push(row_csv.clone());
                                }
                            }
                            current_row_cells.clear();
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(lines.join("\n"))
}

fn format_csv_row(cells: &[String]) -> String {
    cells
        .iter()
        .map(|c| {
            if c.contains(',') || c.contains('"') || c.contains('\n') || c.contains('\r') {
                format!("\"{}\"", c.replace('"', "\"\""))
            } else {
                c.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_spreadsheet_rows_to_csv() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                         xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
                         xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell><text:p>Name</text:p></table:table-cell>
          <table:table-cell><text:p>Role, Title</text:p></table:table-cell>
          <table:table-cell><text:p>Count</text:p></table:table-cell>
        </table:table-row>
        <table:table-row>
          <table:table-cell><text:p>Alice</text:p></table:table-cell>
          <table:table-cell><text:p>Engineer "Lead"</text:p></table:table-cell>
          <table:table-cell><text:p>42</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

        let res = parse_ods_content_xml(xml).unwrap();
        assert_eq!(
            res,
            "Name,\"Role, Title\",Count\nAlice,\"Engineer \"\"Lead\"\"\",42"
        );
    }
}
