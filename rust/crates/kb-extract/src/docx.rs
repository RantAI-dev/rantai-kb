//! `.docx` → text, matching what mammoth's `extractRawText` produces.
//!
//! mammoth's raw-text conversion (`node_modules/mammoth/lib/raw-text.js`) is
//! three lines of logic over the document tree: text nodes emit their value, a
//! tab emits a tab, and every other element emits its children — plus two
//! newlines when the element is a paragraph. Everything else about mammoth
//! (styles, headings, numbering, images, tables-as-grids) exists to serve the
//! HTML writer and produces no raw text, so it is not ported here.
//!
//! What that reduces to, and what this module walks: every `w:p` in document
//! order contributes its text followed by `\n\n`. Paragraphs inside table cells
//! are paragraphs too, so a table comes out as its cell texts one after another
//! with the same `\n\n` between them.
//!
//! Elements mammoth reads without keeping text are listed in [`SKIP`]: field
//! codes, symbol runs (they need mammoth's dingbat table), note and comment
//! references, and the DrawingML picture containers, whose only content is an
//! image. Their subtrees are dropped so their text cannot leak in. Property
//! elements are not in the list — they hold attributes only — but they are
//! counted, because two of them decide what happens next: `w:pPr/w:rPr/w:del`
//! marks a tracked deletion, and `w:tcPr/w:vMerge` marks a merged cell whose
//! text belongs to the cell above it.

use quick_xml::events::Event;

use crate::ExtractError;
use crate::xml;
use crate::zip_read;

/// Where the body lives inside the package.
const DOCUMENT_XML: &str = "word/document.xml";

/// Text of a `.docx`: paragraphs separated by a blank line.
pub fn parse(bytes: &[u8]) -> Result<String, ExtractError> {
    let mut archive = zip_read::open(bytes)?;
    let document = zip_read::read_text(&mut archive, DOCUMENT_XML)?;
    render(&document)
}

/// The paragraph walk, separated from the container so it can be tested on XML.
pub fn render(document: &str) -> Result<String, ExtractError> {
    let mut reader = xml::reader_from_str(document);
    let mut buffer = Vec::new();
    let mut out = String::new();
    // Depth of elements whose subtree is dropped.
    let mut skip_depth = 0usize;
    // Skip depth to restore when a `w:tc` closes, one entry per skipped cell.
    let mut skipped_cells: Vec<usize> = Vec::new();
    let mut deleted_paragraph = false;
    // Text outside a paragraph is layout, not content: a pretty-printed package
    // is indented, and mammoth's reader never sees those text nodes.
    let mut paragraph_depth = 0usize;
    let mut in_paragraph_properties = 0usize;
    let mut in_run_properties = 0usize;
    let mut in_cell_properties = 0usize;
    let mut in_text = false;

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
                    b"vMerge" if in_cell_properties > 0 && continues_vertical_merge(&element) => {
                        skip_depth += 1;
                        skipped_cells.push(skip_depth);
                    }
                    b"tab" if skip_depth == 0 => out.push('\t'),
                    b"noBreakHyphen" if skip_depth == 0 => out.push('\u{2011}'),
                    b"softHyphen" if skip_depth == 0 => out.push('\u{00ad}'),
                    _ => {}
                }
                if SKIP.contains(&name.as_ref()) {
                    skip_depth += 1;
                }
            }
            Event::Empty(element) => match element.local_name().as_ref() {
                b"del" if in_run_properties > 0 => deleted_paragraph = true,
                b"vMerge" if in_cell_properties > 0 && continues_vertical_merge(&element) => {
                    skip_depth += 1;
                    skipped_cells.push(skip_depth);
                }
                b"tab" if skip_depth == 0 => out.push('\t'),
                b"noBreakHyphen" if skip_depth == 0 => out.push('\u{2011}'),
                b"softHyphen" if skip_depth == 0 => out.push('\u{00ad}'),
                _ => {}
            },
            Event::Text(text) => {
                if in_text && skip_depth == 0 && paragraph_depth > 0 {
                    out.push_str(&xml::text_of(&text));
                }
            }
            Event::GeneralRef(reference) => {
                if in_text && skip_depth == 0 && paragraph_depth > 0 {
                    out.push_str(&xml::reference_of(&String::from_utf8_lossy(&reference)));
                }
            }
            Event::End(element) => {
                let name = element.local_name();
                match name.as_ref() {
                    b"t" => in_text = false,
                    b"p" => {
                        paragraph_depth = paragraph_depth.saturating_sub(1);
                        if skip_depth == 0 && !deleted_paragraph {
                            out.push_str("\n\n");
                        }
                    }
                    b"pPr" => in_paragraph_properties = in_paragraph_properties.saturating_sub(1),
                    b"rPr" => in_run_properties = in_run_properties.saturating_sub(1),
                    b"tcPr" => in_cell_properties = in_cell_properties.saturating_sub(1),
                    b"tc" if skipped_cells.last() == Some(&skip_depth) && skip_depth > 0 => {
                        skipped_cells.pop();
                        skip_depth -= 1;
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
    Ok(out)
}

/// A `w:vMerge` with no `w:val`, or `w:val="continue"`, means this cell shares
/// the cell above it; mammoth drops it (`calculateRowSpans`), so its duplicate
/// text is dropped here too.
fn continues_vertical_merge(element: &quick_xml::events::BytesStart<'_>) -> bool {
    match xml::attribute(element, "val") {
        Some(val) => val == "continue",
        None => true,
    }
}

/// Elements whose text mammoth does not read. From its `ignoreElements` list
/// (`body-reader.js`) plus the readers that return non-text elements: field
/// codes, symbol runs, note and comment references, bookmarks, and the
/// DrawingML picture containers.
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
        // mammoth has no heading element: a heading is a paragraph with a
        // style, so raw text carries no marker.
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
    fn table_cells_are_paragraphs_in_document_order() {
        let text = render(
            r#"<w:document xmlns:w="urn:w"><w:body>
                 <w:tbl>
                   <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc>
                         <w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
                   <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc>
                         <w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
                 </w:tbl></w:body></w:document>"#,
        )
        .unwrap();
        assert_eq!(text, "A1\n\nB1\n\nA2\n\nB2\n\n");
    }

    #[test]
    fn a_merged_cell_does_not_repeat_the_cell_above_it() {
        let text = render(
            r#"<w:document xmlns:w="urn:w"><w:body><w:tbl>
                 <w:tr><w:tc><w:p><w:r><w:t>merged</w:t></w:r></w:p></w:tc>
                     <w:tc><w:p><w:r><w:t>side</w:t></w:r></w:p></w:tc></w:tr>
                 <w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr>
                         <w:p><w:r><w:t>merged again</w:t></w:r></w:p></w:tc>
                     <w:tc><w:p><w:r><w:t>side again</w:t></w:r></w:p></w:tc></w:tr>
               </w:tbl></w:body></w:document>"#,
        )
        .unwrap();
        assert_eq!(text, "merged\n\nside\n\nside again\n\n");
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
