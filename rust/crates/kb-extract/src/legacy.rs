//! `.doc`, `.xls` and `.ppt`: route by what the bytes are, not by the name.
//!
//! The extension of a legacy Office file is a weak claim. Office's own "Save
//! as" writes RTF or HTML under `.doc`; web applications and government
//! portals export "Excel" reports as an HTML table, SpreadsheetML XML or
//! UTF-16 tab-separated text named `.xls`; and a modern OOXML package renamed
//! to `.doc` is common. Each of those already has a parser here, so the file's
//! signature picks it:
//!
//! | Signature | Read as |
//! |---|---|
//! | `D0 CF 11 E0` (OLE compound file) | the binary format: `doc`, calamine's `.xls`, `ppt` |
//! | `PK\x03\x04` (ZIP) | the OOXML sibling: `.docx`, `.xlsx`, `.pptx` |
//! | `{\rtf` | RTF |
//! | `<html`, `<!doctype html`, `<table` | HTML, tags stripped |
//! | `<?xml` | XML (SpreadsheetML, Word 2003 XML), tags stripped |
//! | UTF-16LE BOM | UTF-16 text (Excel's "Unicode Text" export) |
//! | UTF-8 text with no NUL bytes | the text itself (CSV/TSV under `.xls`) |
//!
//! Anything else is refused with the bytes it started with, so an operator can
//! see what the file was.

use crate::text::to_utf8;
use crate::{ExtractError, doc, docx, html, ppt, pptx, rtf, xlsx};

/// Which legacy Office format the extension claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claimed {
    Word,
    Excel,
    PowerPoint,
}

impl Claimed {
    fn label(self) -> &'static str {
        match self {
            Self::Word => "Word 97-2003 document",
            Self::Excel => "Excel 97-2003 workbook",
            Self::PowerPoint => "PowerPoint 97-2003 presentation",
        }
    }
}

/// What the first bytes say the file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signature {
    Ole,
    Zip,
    Rtf,
    Html,
    Xml,
    WebArchive,
    Utf16Text,
    Utf8Text,
    /// Word for Windows 1.x/2.0, which predates the OLE container.
    Word2,
    Unknown,
}

const OLE_MAGIC: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// How much of the file the text check looks at.
const SNIFF_LEN: usize = 8 * 1024;

fn signature(bytes: &[u8]) -> Signature {
    if bytes.starts_with(OLE_MAGIC) {
        return Signature::Ole;
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return Signature::Zip;
    }
    if bytes.starts_with(&[0xDB, 0xA5]) || bytes.starts_with(&[0x9B, 0xA5]) {
        return Signature::Word2;
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Signature::Utf16Text;
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let start = body
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(body.len());
    let head: Vec<u8> = body[start..]
        .iter()
        .take(64)
        .map(u8::to_ascii_lowercase)
        .collect();
    if head.starts_with(b"{\\rtf") {
        return Signature::Rtf;
    }
    if head.starts_with(b"<html")
        || head.starts_with(b"<!doctype html")
        || head.starts_with(b"<table")
    {
        return Signature::Html;
    }
    if head.starts_with(b"<?xml") {
        return Signature::Xml;
    }
    if head.starts_with(b"mime-version:") {
        return Signature::WebArchive;
    }
    let prefix = &body[..body.len().min(SNIFF_LEN)];
    if prefix.is_empty() || prefix.contains(&0) {
        return Signature::Unknown;
    }
    match std::str::from_utf8(prefix) {
        Ok(_) => Signature::Utf8Text,
        // A multi-byte character cut by the prefix is still text.
        Err(e) if e.error_len().is_none() => Signature::Utf8Text,
        Err(_) => Signature::Unknown,
    }
}

/// Text of a file that claims to be a legacy Office format.
pub fn parse(bytes: &[u8], claimed: Claimed, file_name: &str) -> Result<String, ExtractError> {
    match signature(bytes) {
        Signature::Ole => match claimed {
            Claimed::Word => doc::parse(bytes),
            // calamine reads BIFF5/BIFF8 `.xls` from the compound file itself.
            Claimed::Excel => xlsx::parse_xls(bytes, file_name),
            Claimed::PowerPoint => ppt::parse(bytes),
        },
        Signature::Zip => match claimed {
            Claimed::Word => docx::parse(bytes),
            Claimed::Excel => xlsx::parse(bytes, file_name),
            Claimed::PowerPoint => pptx::parse(bytes),
        },
        Signature::Rtf => rtf::parse(bytes),
        Signature::Html => Ok(html::strip_html(&to_utf8(bytes))),
        Signature::Xml => Ok(html::strip_xml(&to_utf8(bytes))),
        Signature::Utf16Text => {
            let units: Vec<u16> = bytes[2..]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            Ok(String::from_utf16_lossy(&units))
        }
        Signature::Utf8Text => Ok(to_utf8(bytes)),
        Signature::WebArchive => Err(ExtractError::Unsupported(format!(
            "\"{file_name}\" is a web archive (.mht) saved under a {} name; save it as a real {} or as .docx/.xlsx/.pptx",
            claimed.label(),
            claimed.label()
        ))),
        Signature::Word2 => Err(ExtractError::Unsupported(format!(
            "\"{file_name}\" is a Word for Windows 1.x/2.0 document, which is not supported; re-save it as .docx"
        ))),
        Signature::Unknown => {
            let head: Vec<String> = bytes.iter().take(8).map(|b| format!("{b:02X}")).collect();
            Err(ExtractError::Container(format!(
                "\"{file_name}\" is not a {}: it starts with bytes {}",
                claimed.label(),
                head.join(" ")
            )))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_read_from_the_first_bytes() {
        assert_eq!(signature(OLE_MAGIC), Signature::Ole);
        assert_eq!(signature(b"PK\x03\x04rest"), Signature::Zip);
        assert_eq!(signature(b"{\\rtf1\\ansi hello}"), Signature::Rtf);
        assert_eq!(signature(b"\xEF\xBB\xBF  <HTML><body>"), Signature::Html);
        assert_eq!(signature(b"<table><tr>"), Signature::Html);
        assert_eq!(
            signature(b"<?xml version=\"1.0\"?><Workbook>"),
            Signature::Xml
        );
        assert_eq!(signature(b"MIME-Version: 1.0\r\n"), Signature::WebArchive);
        assert_eq!(signature(b"\xFF\xFEa\x00"), Signature::Utf16Text);
        assert_eq!(signature("a,b\nç,d".as_bytes()), Signature::Utf8Text);
        assert_eq!(signature(b"\xDB\xA5-\x00"), Signature::Word2);
        assert_eq!(signature(b"MZ\x90\x00\x03"), Signature::Unknown);
        assert_eq!(signature(b""), Signature::Unknown);
    }

    #[test]
    fn an_html_report_named_xls_is_read_as_html() {
        let text = parse(
            b"<html><body><table><tr><td>Region</td><td>Total</td></tr></table></body></html>",
            Claimed::Excel,
            "report.xls",
        )
        .unwrap();
        assert!(text.contains("Region") && text.contains("Total"), "{text}");
        assert!(!text.contains("<td>"), "{text}");
    }

    #[test]
    fn rtf_saved_as_doc_is_read_as_rtf() {
        let text = parse(
            b"{\\rtf1\\ansi Hello \\b world\\b0.}",
            Claimed::Word,
            "a.doc",
        )
        .unwrap();
        assert!(text.contains("Hello") && text.contains("world"), "{text}");
    }

    #[test]
    fn utf16_tab_separated_text_named_xls_is_decoded() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(
            "name\tqty\r\nkopi\t3"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        let text = parse(&bytes, Claimed::Excel, "export.xls").unwrap();
        assert_eq!(text, "name\tqty\r\nkopi\t3");
    }

    #[test]
    fn an_unrecognised_binary_names_its_first_bytes() {
        let err = parse(b"MZ\x90\x00\x03\x00\x00\x00", Claimed::PowerPoint, "x.ppt").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("PowerPoint 97-2003"), "{msg}");
        assert!(msg.contains("4D 5A 90 00"), "{msg}");
    }

    #[test]
    fn a_renamed_docx_is_read_as_docx() {
        let docx = crate::testkit::docx_of(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="urn:w"><w:body><w:p><w:r><w:t>renamed body</w:t></w:r></w:p></w:body></w:document>"#,
        )]);
        let text = parse(&docx, Claimed::Word, "renamed.doc").unwrap();
        assert!(text.contains("renamed body"), "{text}");
    }
}
