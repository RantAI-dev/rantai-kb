//! `.epub` → text: the spine's chapters, markup stripped.
//!
//! The TypeScript path writes the upload to a temp file and reads it with
//! `epub2`, iterating `epub.flow` — which is the OPF spine's contents in spine
//! order (`node_modules/epub2/lib/epub.js:469`) — and runs each chapter's raw
//! XHTML through `stripHtml` (`src/lib/files/parsers.ts:102`).
//!
//! So the order matters and is the whole point: an epub whose manifest lists
//! chapters in a different order than the spine is read in spine order, which is
//! the order a reader would use. Unreadable chapters are skipped rather than
//! failing the book, matching the `catch {}` in the TypeScript loop.

use quick_xml::events::Event;

use crate::ExtractError;
use crate::html;
use crate::xml;
use crate::xml::percent_decode;
use crate::zip_read;

/// Where an epub says its package document is.
const CONTAINER_XML: &str = "META-INF/container.xml";

/// Text of an ebook, with the file name heading the TypeScript adds.
pub fn parse(bytes: &[u8], file_name: &str) -> Result<String, ExtractError> {
    let mut archive = zip_read::open(bytes)?;
    let container = zip_read::read_text(&mut archive, CONTAINER_XML)?;
    let opf_path = rootfile_path(&container).ok_or_else(|| {
        ExtractError::Container("META-INF/container.xml names no rootfile".to_string())
    })?;
    let opf = zip_read::read_text(&mut archive, &opf_path)?;
    let base = directory_of(&opf_path);

    let mut chapters: Vec<String> = Vec::new();
    for href in spine_hrefs(&opf) {
        let entry = resolve(&base, &href);
        let Ok(xhtml) = zip_read::read_text(&mut archive, &entry) else {
            // A chapter listed in the spine but missing from the package is
            // skipped, as the TypeScript loop's `catch {}` does.
            continue;
        };
        let clean = html::strip_html(&xhtml);
        if !clean.trim().is_empty() {
            chapters.push(clean);
        }
    }

    if chapters.is_empty() {
        Ok(format!("[EPUB: {file_name}]\n\n(no readable content)"))
    } else {
        Ok(format!(
            "[EPUB: {file_name}]\n\n{}",
            chapters.join("\n\n---\n\n")
        ))
    }
}

/// The `full-path` of the first `rootfile`, which is the package document.
fn rootfile_path(container: &str) -> Option<String> {
    let mut reader = xml::reader_from_str(container);
    let mut buffer = Vec::new();
    let mut path = None;
    while let Ok(Some(event)) = xml::next_event(&mut reader, &mut buffer) {
        if let Event::Start(element) | Event::Empty(element) = event {
            if element.local_name().as_ref() == b"rootfile" && path.is_none() {
                path = xml::attribute(&element, "full-path");
            }
        }
    }
    path
}

/// Manifest id → href, and the spine's ids in order.
fn spine_hrefs(opf: &str) -> Vec<String> {
    let mut reader = xml::reader_from_str(opf);
    let mut buffer = Vec::new();
    let mut manifest: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut spine: Vec<String> = Vec::new();
    let mut in_manifest = false;
    let mut in_spine = false;

    while let Ok(Some(event)) = xml::next_event(&mut reader, &mut buffer) {
        match event {
            Event::Start(element) | Event::Empty(element) => match element.local_name().as_ref() {
                b"manifest" => in_manifest = true,
                b"spine" => in_spine = true,
                b"item" if in_manifest => {
                    if let (Some(id), Some(href)) = (
                        xml::attribute(&element, "id"),
                        xml::attribute(&element, "href"),
                    ) {
                        manifest.insert(id, href);
                    }
                }
                b"itemref" if in_spine => {
                    if let Some(idref) = xml::attribute(&element, "idref") {
                        spine.push(idref);
                    }
                }
                _ => {}
            },
            Event::End(element) => match element.local_name().as_ref() {
                b"manifest" => in_manifest = false,
                b"spine" => in_spine = false,
                _ => {}
            },
            _ => {}
        }
    }

    spine
        .into_iter()
        .filter_map(|idref| manifest.get(&idref).cloned())
        .collect()
}

/// Everything before the last `/`, empty for a file at the archive root.
fn directory_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(index) => &path[..index],
        None => "",
    }
}

/// Resolve a manifest href against the package document's directory, undoing
/// percent-escapes and `.`/`..` segments.
#[must_use]
pub fn resolve(base: &str, href: &str) -> String {
    let href = percent_decode(href);
    let joined = if href.starts_with('/') || base.is_empty() {
        href
    } else {
        format!("{base}/{href}")
    };

    let mut resolved: Vec<&str> = Vec::new();
    for segment in joined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved.join("/")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::testkit::{chapter, epub_of};

    #[test]
    fn chapters_come_out_in_spine_order() {
        let bytes = epub_of(&[
            ("OEBPS/chapter1.xhtml", &chapter("One", "First body")),
            ("OEBPS/chapter2.xhtml", &chapter("Two", "Second body")),
        ]);
        let text = parse(&bytes, "book.epub").unwrap();
        assert_eq!(
            text,
            "[EPUB: book.epub]\n\nOne First body\n\n---\n\nTwo Second body"
        );
    }

    #[test]
    fn an_empty_spine_says_so() {
        let bytes = epub_of(&[]);
        assert_eq!(
            parse(&bytes, "book.epub").unwrap(),
            "[EPUB: book.epub]\n\n(no readable content)"
        );
    }

    #[test]
    fn a_missing_chapter_is_skipped_not_fatal() {
        let bytes = epub_of(&[(
            "OEBPS/only.xhtml",
            &chapter("Only", "The one body"),
        )]);
        let text = parse(&bytes, "book.epub").unwrap();
        assert!(text.contains("The one body"), "{text}");
    }

    #[test]
    fn hrefs_resolve_against_the_package_document() {
        assert_eq!(resolve("OEBPS", "chapter1.xhtml"), "OEBPS/chapter1.xhtml");
        assert_eq!(resolve("OEBPS", "Text/chap%201.xhtml"), "OEBPS/Text/chap 1.xhtml");
        assert_eq!(resolve("OEBPS", "../shared/x.xhtml"), "shared/x.xhtml");
        assert_eq!(resolve("OEBPS", "/top.xhtml"), "top.xhtml");
        assert_eq!(resolve("", "top.xhtml"), "top.xhtml");
    }

    #[test]
    fn a_package_without_a_container_is_an_error() {
        let bytes = crate::testkit::zip_of(&[("mimetype", "application/epub+zip")]);
        let err = parse(&bytes, "book.epub").unwrap_err();
        assert!(matches!(err, ExtractError::Container(_)), "{err}");
    }
}
