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
//! An unknown extension is refused with a reason. The upload route already
//! rejects it with a 415, so this guards the other ways a job reaches the
//! extractor (a job row written by a calling app, a reindex of an older
//! upload): reading arbitrary bytes as text would index binary noise.

use kb_ports::{ExtractionOptions, ExtractionResult};

use crate::ExtractError;
use crate::chain::{LayoutExtractorChain, ScannedPageOcr};
use crate::clip_reason;
use crate::code;
use crate::detect::{SupportedFileType, detect_file_type, extension_of};
use crate::docx;
use crate::epub;
use crate::gltf;
use crate::html;
use crate::legacy::{self, Claimed};
use crate::mime::ext_to_mime;
use crate::ocr_router::OcrRouter;
use crate::ods;
use crate::odt;
use crate::pdfium::{extract_pdf_text_layer, get_pdfium};
use crate::pptx;
use crate::rtf;
use crate::scanned::is_pdf_scanned;
use crate::text::to_utf8;
use crate::tiff_pages;
use crate::xlsx;

const MIME_DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const MIME_XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const MIME_PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
const MIME_RTF: &str = "application/rtf";
const MIME_EPUB: &str = "application/epub+zip";
const MIME_GLTF: &str = "model/gltf+json";
const MIME_GLB: &str = "model/gltf-binary";
const MIME_ODT: &str = "application/vnd.oasis.opendocument.text";
const MIME_ODS: &str = "application/vnd.oasis.opendocument.spreadsheet";

const MIME_DOC: &str = "application/msword";
const MIME_XLS: &str = "application/vnd.ms-excel";
const MIME_PPT: &str = "application/vnd.ms-powerpoint";

/// The failure for a file whose extension no parser claims.
fn unsupported_type(filename: &str) -> ExtractionResult {
    let ext = extension_of(filename);
    let what = if ext.is_empty() {
        "has no file extension".to_string()
    } else {
        format!("has an unsupported file type ({ext})")
    };
    ExtractionResult::failed(
        "markdown",
        format!("\"{filename}\" {what}; see GET /v1/formats for the accepted list"),
    )
}

/// Synchronous local extraction for stored uploads.
pub fn extract(
    filename: &str,
    mime_type: &str,
    bytes: &[u8],
) -> Result<ExtractionResult, ExtractError> {
    match detect_file_type(filename) {
        Some(SupportedFileType::Pdf) => {
            let pdfium = get_pdfium()?;
            match extract_pdf_text_layer(pdfium, bytes) {
                Ok(res) => Ok(res),
                Err(err) => Ok(ExtractionResult::failed(
                    "pdf",
                    format!(
                        "PDF parse failed for \"{filename}\": {}",
                        clip_reason(&err.to_string())
                    ),
                )),
            }
        }
        Some(SupportedFileType::Image) => Err(ExtractError::Unsupported(format!(
            "OCR for images requires a vision provider (\"{filename}\")"
        ))),
        Some(
            SupportedFileType::Markdown | SupportedFileType::Document | SupportedFileType::Text,
        ) => {
            let mime = ext_to_mime(&extension_of(filename))
                .filter(|known| !known.is_empty())
                .or_else(|| non_empty(mime_type))
                .unwrap_or("text/plain");
            Ok(match extract_from_buffer(bytes, mime, filename) {
                Ok(content) => ExtractionResult::text(content, "markdown"),
                Err(reason) => ExtractionResult::failed(
                    "markdown",
                    format!(
                        "Text extraction failed for \"{filename}\": {}",
                        clip_reason(&reason.to_string())
                    ),
                ),
            })
        }
        None => Ok(unsupported_type(filename)),
    }
}

/// Asynchronous production extraction matching `src/lib/ingest/extract.ts:40`.
pub async fn extract_document(
    filename: &str,
    mime_type: &str,
    bytes: &[u8],
    options: ExtractionOptions,
    chain: &LayoutExtractorChain,
    vision_ocr: Option<&dyn ScannedPageOcr>,
    ocr_router: Option<&OcrRouter>,
) -> Result<ExtractionResult, ExtractError> {
    match detect_file_type(filename) {
        Some(SupportedFileType::Pdf) => {
            let pdfium = match get_pdfium() {
                Ok(p) => p,
                Err(e) => {
                    return Ok(ExtractionResult::failed(
                        "pdf",
                        format!(
                            "PDF parse failed for \"{filename}\": {}",
                            clip_reason(&e.to_string())
                        ),
                    ));
                }
            };
            let is_scanned = options.force_layout || is_pdf_scanned(pdfium, bytes);
            // The page router (ING-6) decides per page: `hybrid` keeps a
            // digital page's text and sends only scans, `full` sends every
            // page. An explicit layout request still takes the whole-document
            // chain, which is the contract `/v1/extract?layout=1` established.
            if let (Some(router), false) = (ocr_router, options.force_layout) {
                return match router.extract_pdf(bytes, filename, &options).await {
                    Ok(res) => Ok(res),
                    Err(err) => {
                        let reason = clip_reason(&err.to_string());
                        let prefix = format!("OCR failed for \"{filename}\": ");
                        let error_msg = if reason.starts_with(&prefix) {
                            reason
                        } else {
                            format!("{prefix}{reason}")
                        };
                        Ok(ExtractionResult::failed("pdf", error_msg))
                    }
                };
            }
            if is_scanned {
                match chain.extract_scanned_pdf(filename, bytes, options).await {
                    Ok(res) => Ok(res),
                    Err(err) => {
                        let reason = clip_reason(&err.to_string());
                        let prefix = format!("OCR failed for \"{filename}\": ");
                        let error_msg = if let Some(stripped) = reason.strip_prefix(&prefix) {
                            format!("{prefix}{stripped}")
                        } else if reason.starts_with(&format!("OCR failed for \"{filename}\":")) {
                            reason
                        } else {
                            format!("{prefix}{reason}")
                        };
                        Ok(ExtractionResult::failed("pdf", error_msg))
                    }
                }
            } else {
                match extract_pdf_text_layer(pdfium, bytes) {
                    Ok(res) => Ok(res),
                    Err(err) => Ok(ExtractionResult::failed(
                        "pdf",
                        format!(
                            "PDF parse failed for \"{filename}\": {}",
                            clip_reason(&err.to_string())
                        ),
                    )),
                }
            }
        }
        Some(SupportedFileType::Image) => {
            // A picture is read by a remote model (OCR or vision), or not at
            // all: nothing local is left to do, so the worker's extract seat
            // goes back before the wait (ING-6).
            options.seat.release();
            // The router carries the page cache, the shared pool and the
            // breaker for pictures too (ING-6), when its provider reads
            // images. A document-only provider (Mistral) keeps the old path.
            if let Some(router) = ocr_router {
                match router.extract_image(bytes, mime_type, filename).await {
                    Ok(res) => return Ok(res),
                    Err(ExtractError::Unsupported(_)) => {}
                    Err(err) => {
                        return Ok(ExtractionResult::failed(
                            "image",
                            format!(
                                "Image OCR failed for \"{filename}\": {}",
                                clip_reason(&err.to_string())
                            ),
                        ));
                    }
                }
            }
            if let (Some(ocr), true) = (
                vision_ocr,
                tiff_pages::is_tiff_extension(&extension_of(filename)),
            ) {
                return Ok(match tiff_pages::ocr_tiff(ocr, bytes, filename).await {
                    Ok(res) => res,
                    Err(err) => ExtractionResult::failed(
                        "image",
                        format!(
                            "Image OCR failed for \"{filename}\": {}",
                            clip_reason(&err.to_string())
                        ),
                    ),
                });
            }
            if let Some(ocr) = vision_ocr {
                let ext = extension_of(filename);
                let img_mime = match ext.as_str() {
                    ".png" => "image/png",
                    ".jpg" | ".jpeg" => "image/jpeg",
                    ".gif" => "image/gif",
                    ".webp" => "image/webp",
                    ".heic" => "image/heic",
                    _ => {
                        if !mime_type.is_empty() {
                            mime_type
                        } else {
                            "image/png"
                        }
                    }
                };
                match ocr.ocr_image(bytes, img_mime, filename).await {
                    Ok(res) => Ok(res),
                    Err(err) => Ok(ExtractionResult::failed(
                        "image",
                        format!(
                            "Image OCR failed for \"{filename}\": {}",
                            clip_reason(&err.to_string())
                        ),
                    )),
                }
            } else {
                // No OCR: the picture is still read by the visual stage when an
                // image describer is configured, and ingest fails it visibly
                // when nothing can (`ING-5`), so an empty text is not a lie.
                tracing::info!(
                    file = filename,
                    "no vision OCR provider; image text left empty"
                );
                Ok(ExtractionResult::text(String::new(), "image"))
            }
        }
        Some(
            SupportedFileType::Markdown | SupportedFileType::Document | SupportedFileType::Text,
        ) => {
            let mime = ext_to_mime(&extension_of(filename))
                .filter(|known| !known.is_empty())
                .or_else(|| non_empty(mime_type))
                .unwrap_or("text/plain");
            Ok(match extract_from_buffer(bytes, mime, filename) {
                Ok(content) => ExtractionResult::text(content, "markdown"),
                Err(reason) => ExtractionResult::failed(
                    "markdown",
                    format!(
                        "Text extraction failed for \"{filename}\": {}",
                        clip_reason(&reason.to_string())
                    ),
                ),
            })
        }
        None => Ok(unsupported_type(filename)),
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
        MIME_ODT => odt::parse_odt(bytes),
        MIME_ODS => ods::parse_ods(bytes),
        MIME_DOC => legacy::parse(bytes, Claimed::Word, file_name),
        MIME_XLS => legacy::parse(bytes, Claimed::Excel, file_name),
        MIME_PPT => legacy::parse(bytes, Claimed::PowerPoint, file_name),
        MIME_GLTF => gltf::parse_gltf(bytes, file_name),
        MIME_GLB => gltf::parse_glb(bytes, file_name),
        "text/html" => Ok(html::strip_html(&to_utf8(bytes))),
        "application/xml" | "text/xml" => Ok(html::strip_xml(&to_utf8(bytes))),
        _ => match code::language_of(&extension_of(file_name)) {
            Some(language) => Ok(code::wrap_as_code(file_name, language, &to_utf8(bytes))),
            None => Ok(to_utf8(bytes)),
        },
    }
}

fn non_empty(value: &str) -> Option<&str> {
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::testkit::{chapter, docx_of, epub_of, slide_with, xlsx_of, zip_of};

    struct Case {
        name: &'static str,
        file_name: &'static str,
        mime_type: &'static str,
        bytes: Vec<u8>,
        expected: String,
    }

    fn text_case(
        name: &'static str,
        file_name: &'static str,
        mime: &'static str,
        body: &str,
    ) -> Case {
        Case {
            name,
            file_name,
            mime_type: mime,
            bytes: body.as_bytes().to_vec(),
            expected: body.to_string(),
        }
    }

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
                name: "docx paragraphs become text with blank lines",
                file_name: "doc.docx",
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
                name: "odt extracts headings and paragraphs",
                file_name: "doc.odt",
                mime_type: MIME_ODT,
                bytes: zip_of(&[(
                    "content.xml",
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                         xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:h text:outline-level="1">Title</text:h>
      <text:p>Body</text:p>
    </office:text>
  </office:body>
</office:document-content>"#,
                )]),
                expected: "Title\nBody".to_string(),
            },
            Case {
                name: "ods extracts rows as csv",
                file_name: "sheet.ods",
                mime_type: MIME_ODS,
                bytes: zip_of(&[(
                    "content.xml",
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                         xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
                         xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="S1">
        <table:table-row>
          <table:table-cell><text:p>A</text:p></table:table-cell>
          <table:table-cell><text:p>B</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#,
                )]),
                expected: "A,B".to_string(),
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
            let result = extract(case.file_name, case.mime_type, &case.bytes)
                .unwrap_or_else(|e| panic!("{}: unexpected error {e}", case.name));
            assert!(result.error.is_none(), "{}: {:?}", case.name, result.error);
            assert_eq!(result.content, case.expected, "{}", case.name);
            assert_eq!(result.file_type, "markdown", "{}", case.name);
            assert!(!result.used_ocr, "{}", case.name);
            assert!(result.page_map.is_none(), "{}", case.name);
        }
    }

    #[test]
    fn the_extension_table_beats_the_uploaded_mime() {
        let result = extract("page.html", "text/plain", b"<p>Hello</p>").unwrap();
        assert_eq!(result.content, "Hello");
    }

    #[test]
    fn an_unknown_extension_is_refused_with_a_reason_not_indexed_as_text() {
        let result = extract("archive.bin", "application/octet-stream", b"raw bytes").unwrap();
        assert_eq!(result.content, "");
        let error = result.error.expect("a reason");
        assert!(error.contains("unsupported file type (.bin)"), "{error}");
        assert!(error.contains("/v1/formats"), "{error}");

        let result = extract("README", "text/plain", b"text").unwrap();
        assert!(
            result
                .error
                .expect("a reason")
                .contains("no file extension"),
            "an extensionless name is refused too"
        );
    }

    #[tokio::test]
    async fn the_production_path_refuses_an_unknown_extension_too() {
        let chain = LayoutExtractorChain::new(Vec::new(), None);
        let result = extract_document(
            "tool.exe",
            "application/octet-stream",
            b"MZ\x90\x00",
            ExtractionOptions::default(),
            &chain,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.content, "");
        assert!(result.error.expect("a reason").contains("(.exe)"));
    }

    #[test]
    fn a_parser_failure_is_reported_without_content() {
        let result = extract("broken.docx", MIME_DOCX, b"not a zip").unwrap();
        assert_eq!(result.content, "");
        let error = result.error.expect("a reason");
        assert!(
            error.starts_with("Text extraction failed for \"broken.docx\": "),
            "{error}"
        );
        assert!(error.contains("not a zip container"), "{error}");
    }

    #[test]
    fn images_refuse_without_vision_provider() {
        let image = extract("scan.png", "image/png", b"\x89PNG").unwrap_err();
        assert!(
            image.to_string().contains("requires a vision provider"),
            "{image}"
        );
    }

    #[test]
    fn legacy_formats_route_by_their_bytes_and_explain_garbage() {
        // Not an OLE file and not text: the reason names the claimed format
        // and the bytes, instead of indexing them.
        for (file_name, mime, label) in [
            ("old.doc", "application/msword", "Word 97-2003"),
            ("old.xls", "application/vnd.ms-excel", "Excel 97-2003"),
            (
                "old.ppt",
                "application/vnd.ms-powerpoint",
                "PowerPoint 97-2003",
            ),
        ] {
            let result = extract(file_name, mime, b"\x00\x01binary").unwrap();
            let error = result.error.unwrap_or_else(|| panic!("{file_name}"));
            assert!(error.contains(label), "{file_name}: {error}");
            assert!(error.contains("00 01 62"), "{file_name}: {error}");
        }
        // An HTML report saved as .xls is read.
        let result = extract("report.xls", "", b"<html><td>Total</td></html>").unwrap();
        assert_eq!(result.error, None);
        assert!(result.content.contains("Total"));
    }
}
