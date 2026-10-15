//! Which parser runs for which file.
//!
//! The dispatch is `extractDocumentText` (`src/lib/ingest/extract.ts:40`) and,
//! below it, `extractTextFromBuffer` (`src/lib/files/parsers.ts:244`):
//!
//! 1. the filename decides the family — PDF, image, document/text, or unknown;
//! 2. for documents and text, the mime comes from the extension table, falling
//!    back to the mime the uploader supplied, then to `text/plain`;
//! 3. that mime picks the parser.
//!
//! An unknown extension is not an error: it is read as UTF-8 text, which is what
//! the TypeScript `else` branch does. PDF and image are the two branches that
//! are not ported — they need a text layer, an OCR model and the layout
//! extractors — and they say so rather than returning nothing.

use kb_ports::ExtractionResult;

use crate::clip_reason;
use crate::code;
use crate::detect::{SupportedFileType, detect_file_type, extension_of};
use crate::docx;
use crate::epub;
use crate::gltf;
use crate::html;
use crate::mime::ext_to_mime;
use crate::pptx;
use crate::rtf;
use crate::text::to_utf8;
use crate::xlsx;
use crate::ExtractError;

const MIME_DOCX: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const MIME_XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const MIME_PPTX: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation";
const MIME_RTF: &str = "application/rtf";
const MIME_EPUB: &str = "application/epub+zip";
const MIME_GLTF: &str = "model/gltf+json";
const MIME_GLB: &str = "model/gltf-binary";

/// The legacy binary formats, which the TypeScript service reads with
/// `officeparser` over a format-specific binary parser.
const LEGACY_BINARY_MIMES: &[&str] = &[
    "application/msword",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.oasis.opendocument.spreadsheet",
];

/// Extract text from a stored upload.
pub fn extract(
    filename: &str,
    mime_type: &str,
    bytes: &[u8],
) -> Result<ExtractionResult, ExtractError> {
    match detect_file_type(filename) {
        Some(SupportedFileType::Pdf) => Err(ExtractError::Unsupported(format!(
            "PDF extraction is not ported yet (\"{filename}\"): the text layer, page map and \
             layout extractors arrive in the next slice"
        ))),
        Some(SupportedFileType::Image) => Err(ExtractError::Unsupported(format!(
            "OCR for images is not ported yet (\"{filename}\")"
        ))),
        Some(
            SupportedFileType::Markdown | SupportedFileType::Document | SupportedFileType::Text,
        ) => {
            // The extension wins over the uploader's mime, because the upload
            // route accepts the same list but browsers guess generously.
            let mime = ext_to_mime(&extension_of(filename))
                .filter(|known| !known.is_empty())
                .or_else(|| non_empty(mime_type))
                .unwrap_or("text/plain");
            Ok(match extract_from_buffer(bytes, mime, filename) {
                Ok(content) => ExtractionResult::text(content, "markdown"),
                // The TypeScript shape: no content, plus the reason, and the
                // caller aborts rather than indexing the empty document.
                Err(reason) => ExtractionResult::failed(
                    "markdown",
                    format!(
                        "Text extraction failed for \"{filename}\": {}",
                        clip_reason(&reason.to_string())
                    ),
                ),
            })
        }
        None => Ok(ExtractionResult::text(to_utf8(bytes), "markdown")),
    }
}

/// The parser registry: text for `bytes`, given the mime type it was detected
/// as. Public because the chat attachment pipeline in the TypeScript service
/// calls the same function with a mime the uploader supplied.
pub fn extract_from_buffer(
    bytes: &[u8],
    mime_type: &str,
    file_name: &str,
) -> Result<String, ExtractError> {
    match mime_type {
        MIME_DOCX => docx::parse(bytes),
        MIME_XLSX => xlsx::parse(bytes, file_name),
        MIME_PPTX => pptx::parse(bytes),
        MIME_RTF => rtf::parse(bytes),
        MIME_EPUB => epub::parse(bytes, file_name),
        other if LEGACY_BINARY_MIMES.contains(&other) => Err(ExtractError::Unsupported(format!(
            "{other} is a legacy binary format that the TypeScript service reads with \
             officeparser, which is not ported (\"{file_name}\")"
        ))),
        MIME_GLTF => gltf::parse_gltf(bytes, file_name),
        MIME_GLB => gltf::parse_glb(bytes, file_name),
        "text/html" => Ok(html::strip_html(&to_utf8(bytes))),
        "application/xml" | "text/xml" => Ok(html::strip_xml(&to_utf8(bytes))),
        _ => match code::language_of(&extension_of(file_name)) {
            // Code is fenced so the chunker keeps it whole
            // (`preserveCodeBlocks`), and the fence carries the language.
            Some(language) => Ok(code::wrap_as_code(file_name, language, &to_utf8(bytes))),
            None => Ok(to_utf8(bytes)),
        },
    }
}

fn non_empty(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::testkit::{chapter, docx_of, epub_of, slide_with, xlsx_of, zip_of};

    /// One dispatch case: filename, uploader mime, bytes, expected text.
    struct Case {
        name: &'static str,
        file_name: &'static str,
        mime_type: &'static str,
        bytes: Vec<u8>,
        expected: String,
    }

    fn text_case(name: &'static str, file_name: &'static str, mime: &'static str, body: &str) -> Case {
        Case {
            name,
            file_name,
            mime_type: mime,
            bytes: body.as_bytes().to_vec(),
            expected: body.to_string(),
        }
    }


    /// Every mime the dispatch answers for, with the parser each one reaches.
    #[test]
    fn each_mime_reaches_its_parser() {
        let cases = vec![
            text_case("plain text", "notes.txt", "text/plain", "just notes"),
            text_case("markdown", "readme.md", "text/markdown", "# Title\n\nbody"),
            text_case("csv", "rows.csv", "text/csv", "a,b\n1,2"),
            text_case("json", "config.json", "application/json", "{\"k\":1}"),
            text_case("yaml", "config.yml", "text/yaml", "k: 1"),
            text_case("toml", "config.toml", "application/toml", "k = 1"),
            Case {
                name: "html is stripped to text",
                file_name: "page.html",
                mime_type: "text/html",
                bytes: b"<p>Hello <b>world</b></p>".to_vec(),
                expected: "Hello world".to_string(),
            },
            Case {
                name: "xml is stripped to text",
                file_name: "feed.xml",
                mime_type: "application/xml",
                bytes: b"<rss><title>Feed</title></rss>".to_vec(),
                expected: "Feed".to_string(),
            },
            Case {
                name: "text/xml is stripped like application/xml",
                file_name: "feed.xml",
                mime_type: "text/xml",
                bytes: b"<rss><title>Feed</title></rss>".to_vec(),
                expected: "Feed".to_string(),
            },
            Case {
                name: "code is fenced with its language",
                file_name: "main.rs",
                mime_type: "text/plain",
                bytes: b"fn main() {}".to_vec(),
                expected: "[Code: main.rs]\n```rust\nfn main() {}\n```".to_string(),
            },
            Case {
                name: "docx paragraphs become the document text",
                file_name: "report.docx",
                mime_type: MIME_DOCX,
                bytes: docx_of(&[(
                    "word/document.xml",
                    r#"<w:document xmlns:w="urn:w"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p>
                       <w:p><w:r><w:t>docx</w:t></w:r></w:p></w:body></w:document>"#,
                )]),
                expected: "Hello\n\ndocx\n\n".to_string(),
            },
            Case {
                name: "xlsx sheets become csv blocks",
                file_name: "book.xlsx",
                mime_type: MIME_XLSX,
                bytes: xlsx_of(
                    &[(
                        "Summary",
                        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
                             <row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>"#,
                    )],
                    &["name"],
                ),
                expected: "[Spreadsheet: book.xlsx]\n\n[Sheet: Summary]\nname".to_string(),
            },
            Case {
                name: "pptx paragraphs become one line each",
                file_name: "deck.pptx",
                mime_type: MIME_PPTX,
                bytes: zip_of(&[("ppt/slides/slide1.xml", &slide_with(&["Title", "Body"]))]),
                expected: "Title\nBody".to_string(),
            },
            Case {
                name: "epub chapters follow the spine",
                file_name: "book.epub",
                mime_type: MIME_EPUB,
                bytes: epub_of(&[("OEBPS/c1.xhtml", &chapter("One", "First body"))]),
                expected: "[EPUB: book.epub]\n\nOne First body".to_string(),
            },
            Case {
                name: "rtf loses its markup",
                file_name: "memo.rtf",
                mime_type: MIME_RTF,
                bytes: br"{\rtf1\ansi Memo\par body}".to_vec(),
                expected: "Memo body".to_string(),
            },
            Case {
                name: "gltf becomes a model inventory",
                file_name: "chair.gltf",
                mime_type: MIME_GLTF,
                bytes: br#"{"asset":{"version":"2.0"}}"#.to_vec(),
                expected: "[3D Model: chair.gltf]\n\nFormat: glTF 2.0".to_string(),
            },
        ];

        for case in &cases {
            let result = extract(case.file_name, case.mime_type, &case.bytes).unwrap_or_else(
                |e| panic!("{}: unexpected error {e}", case.name),
            );
            assert!(result.error.is_none(), "{}: {:?}", case.name, result.error);
            assert_eq!(result.content, case.expected, "{}", case.name);
            assert_eq!(result.file_type, "markdown", "{}", case.name);
            assert!(!result.used_ocr, "{}", case.name);
            assert!(result.page_map.is_none(), "{}", case.name);
        }
    }

    #[test]
    fn the_extension_table_beats_the_uploaded_mime() {
        // A browser that sends `text/plain` for an `.html` upload still gets
        // the HTML parser, because the table is checked first.
        let result = extract(
            "page.html",
            "text/plain",
            b"<p>Hello</p>",
        )
        .unwrap();
        assert_eq!(result.content, "Hello");
    }

    #[test]
    fn an_unknown_extension_is_read_as_utf8() {
        let result = extract("archive.bin", "application/octet-stream", b"raw bytes").unwrap();
        assert_eq!(result.content, "raw bytes");
    }

    #[test]
    fn a_parser_failure_is_reported_without_content() {
        let result = extract("broken.docx", MIME_DOCX, b"not a zip").unwrap();
        assert_eq!(result.content, "");
        let error = result.error.expect("a reason");
        assert!(error.starts_with("Text extraction failed for \"broken.docx\": "), "{error}");
        assert!(error.contains("not a zip container"), "{error}");
    }

    #[test]
    fn pdf_and_images_refuse_instead_of_returning_nothing() {
        let pdf = extract("report.pdf", "application/pdf", b"%PDF-1.7").unwrap_err();
        assert!(pdf.to_string().contains("PDF extraction is not ported"), "{pdf}");
        let image = extract("scan.png", "image/png", b"\x89PNG").unwrap_err();
        assert!(image.to_string().contains("OCR for images is not ported"), "{image}");
    }

    #[test]
    fn legacy_binary_formats_name_themselves_as_unported() {
        for (file_name, mime) in [
            ("old.doc", "application/msword"),
            ("old.xls", "application/vnd.ms-excel"),
            ("old.ppt", "application/vnd.ms-powerpoint"),
            ("old.odt", "application/vnd.oasis.opendocument.text"),
            ("old.ods", "application/vnd.oasis.opendocument.spreadsheet"),
        ] {
            let result = extract(file_name, mime, b"binary").unwrap();
            let error = result.error.unwrap_or_else(|| panic!("{file_name}"));
            assert!(error.contains("officeparser"), "{file_name}: {error}");
            assert!(error.contains(mime), "{file_name}: {error}");
        }
    }
}