//! Fixture builders.
//!
//! Every parser's test document is assembled here from the bytes it needs,
//! rather than committed as a binary: a reviewer can read exactly what the
//! parser was given, and a fixture cannot rot against a library upgrade.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::format_collect,
    clippy::doc_markdown
)]

use std::io::{Cursor, Write};

use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

/// A zip archive holding `entries`, deflated like a real Office package.
#[must_use]
pub fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, content) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// A `.docx` package around the given parts.
#[must_use]
pub fn docx_of(entries: &[(&str, &str)]) -> Vec<u8> {
    zip_of(entries)
}

/// A minimal but valid `.xlsx`: a workbook, its relationships, the shared
/// string table `t="s"` cells index into, and one worksheet per sheet.
#[must_use]
pub fn xlsx_of(sheets: &[(&str, &str)], shared: &[&str]) -> Vec<u8> {
    let mut entries: Vec<(String, String)> = vec![
        (
            "[Content_Types].xml".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#
                .to_string(),
        ),
        (
            "_rels/.rels".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#
                .to_string(),
        ),
        (
            "xl/_rels/workbook.xml.rels".to_string(),
            rels_for_sheets(sheets.len()),
        ),
        (
            "xl/workbook.xml".to_string(),
            workbook_for_sheets(sheets),
        ),
        (
            "xl/sharedStrings.xml".to_string(),
            shared_strings(shared),
        ),
    ];
    for (index, (_, sheet_xml)) in sheets.iter().enumerate() {
        entries.push((
            format!("xl/worksheets/sheet{}.xml", index + 1),
            (*sheet_xml).to_string(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = entries
        .iter()
        .map(|(name, content)| (name.as_str(), content.as_str()))
        .collect();
    zip_of(&borrowed)
}

fn rels_for_sheets(count: usize) -> String {
    let relationships: String = (1..=count)
        .map(|index| {
            format!(
                "<Relationship Id=\"rId{index}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{index}.xml\"/>"
            )
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{relationships}</Relationships>"
    )
}

fn workbook_for_sheets(sheets: &[(&str, &str)]) -> String {
    let entries: String = sheets
        .iter()
        .enumerate()
        .map(|(index, (name, _))| {
            format!(
                "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
                name,
                index + 1,
                index + 1
            )
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>{entries}</sheets></workbook>"
    )
}

fn shared_strings(strings: &[&str]) -> String {
    let items: String = strings
        .iter()
        .map(|value| format!("<si><t>{value}</t></si>"))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"{}\" uniqueCount=\"{}\">{items}</sst>",
        strings.len(),
        strings.len()
    )
}

/// A worksheet whose first row has values in columns A and C, so the gap in
/// between shows how a missing cell is written.
#[must_use]
pub fn worksheet_with_gap() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
  <row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c></row>
  <row r="2"><c r="A2" t="s"><v>2</v></c><c r="C2"><v>42</v></c></row>
</sheetData></worksheet>"#
}

/// A worksheet with no cells at all, which SheetJS renders as an empty sheet
/// and the parser then skips.
#[must_use]
pub fn empty_worksheet() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#
}

/// A slide with two shapes, each with one paragraph.
#[must_use]
pub fn slide_with(paragraphs: &[&str]) -> String {
    let body: String = paragraphs
        .iter()
        .map(|text| {
            format!("<p:sp><p:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>")
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:sld xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><p:cSld><p:spTree>{body}</p:spTree></p:cSld></p:sld>"
    )
}

/// An EPUB with `chapters` (entry name, XHTML body) listed in the OPF spine in
/// the order given.
#[must_use]
pub fn epub_of(chapters: &[(&str, &str)]) -> Vec<u8> {
    let manifest: String = chapters
        .iter()
        .enumerate()
        .map(|(index, (name, _))| {
            let file = name.rsplit('/').next().unwrap_or(name);
            format!(
                "<item id=\"item{index}\" href=\"{file}\" media-type=\"application/xhtml+xml\"/>"
            )
        })
        .collect();
    let spine: String = chapters
        .iter()
        .enumerate()
        .map(|(index, _)| format!("<itemref idref=\"item{index}\"/>"))
        .collect();
    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\"><manifest>{manifest}</manifest><spine>{spine}</spine></package>"
    );
    let mut entries: Vec<(String, String)> = vec![
        (
            "mimetype".to_string(),
            "application/epub+zip".to_string(),
        ),
        (
            "META-INF/container.xml".to_string(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#
                .to_string(),
        ),
        ("OEBPS/content.opf".to_string(), opf),
    ];
    for (name, body) in chapters.iter().rev() {
        entries.push(((*name).to_string(), (*body).to_string()));
    }
    let borrowed: Vec<(&str, &str)> = entries
        .iter()
        .map(|(name, content)| (name.as_str(), content.as_str()))
        .collect();
    zip_of(&borrowed)
}

/// One XHTML chapter as the parser will see it after markup is stripped.
#[must_use]
pub fn chapter(title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<html><body><h1>{title}</h1><p>{body}</p></body></html>"
    )
}
