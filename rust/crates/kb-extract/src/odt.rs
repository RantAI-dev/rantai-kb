//! Parser for OpenDocument Text (.odt) documents.
//!
//! ODT files are zip archives containing `content.xml`. In OpenDocument format,
//! prose paragraphs are represented as `<text:p>` and headings as `<text:h>`.
//! Formatting tags (like `<text:span>`) contain text nodes, `<text:s>` denotes
//! spaces, `<text:tab/>` denotes tabs, and `<text:line-break/>` denotes line breaks.
//!
//! Matching officeparser's behaviour for ODT, non-empty paragraphs and headings
//! are extracted in document order and joined by newlines.

use quick_xml::events::Event;

use crate::ExtractError;
use crate::xml;
use crate::zip_read;

/// Parse plain text from an ODT file's raw bytes.
pub fn parse_odt(bytes: &[u8]) -> Result<String, ExtractError> {
    let mut archive = zip_read::open(bytes)?;
    let content_xml = zip_read::read_text(&mut archive, "content.xml")?;
    parse_odt_content_xml(&content_xml)
}

/// Extract paragraphs and headings from ODT `content.xml`.
pub fn parse_odt_content_xml(xml_str: &str) -> Result<String, ExtractError> {
    let mut reader = xml::reader_from_str(xml_str);

    let mut paragraphs: Vec<String> = Vec::new();
    let mut current_block = String::new();
    let mut in_block = false;
    let mut block_depth = 0usize;

    let mut buf = Vec::new();

    while let Some(event) = xml::next_event(&mut reader, &mut buf)? {
        match event {
            Event::Start(ref e) => {
                let name = e.local_name();
                if name.as_ref() == b"p" || name.as_ref() == b"h" {
                    if !in_block {
                        in_block = true;
                        block_depth = 1;
                        current_block.clear();
                    } else {
                        block_depth += 1;
                    }
                }
            }
            Event::Empty(ref e) => {
                let name = e.local_name();
                if in_block {
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
                            current_block.push(' ');
                        }
                    } else if name.as_ref() == b"tab" {
                        current_block.push('\t');
                    } else if name.as_ref() == b"line-break" {
                        current_block.push('\n');
                    }
                }
            }
            Event::Text(ref text) => {
                if in_block {
                    current_block.push_str(&xml::text_of(text));
                }
            }
            Event::GeneralRef(ref r) => {
                if in_block {
                    current_block.push_str(&xml::reference_of(&String::from_utf8_lossy(r)));
                }
            }
            Event::CData(ref cdata) => {
                if in_block {
                    if let Ok(s) = std::str::from_utf8(cdata.as_ref()) {
                        current_block.push_str(s);
                    }
                }
            }
            Event::End(ref e) => {
                let name = e.local_name();
                if name.as_ref() == b"p" || name.as_ref() == b"h" {
                    if block_depth > 1 {
                        block_depth -= 1;
                    } else if in_block {
                        in_block = false;
                        block_depth = 0;
                        let trimmed = current_block.trim();
                        if !trimmed.is_empty() {
                            paragraphs.push(trimmed.to_string());
                        }
                        current_block.clear();
                    }
                }
            }
            _ => {}
        }
    }

    Ok(paragraphs.join("\n"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_headings_and_paragraphs_with_spaces_and_tabs() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                         xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:h text:outline-level="1">Chapter 1: The Beginning</text:h>
      <text:p>This is the first paragraph<text:s text:c="3"/>with spaces and a<text:tab/>tab.</text:p>
      <text:p>Second paragraph on a new line.</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let res = parse_odt_content_xml(xml).unwrap();
        assert_eq!(
            res,
            "Chapter 1: The Beginning\nThis is the first paragraph   with spaces and a\ttab.\nSecond paragraph on a new line."
        );
    }
}
