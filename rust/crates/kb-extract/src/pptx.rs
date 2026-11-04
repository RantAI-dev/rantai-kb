//! `.pptx` → text: one line per paragraph, slides in order.
//!
//! The TypeScript path is `officeparser.parseOffice` for a presentation
//! (`src/lib/files/parsers.ts:68`), whose `toText` walks the parsed tree and
//! joins paragraphs, shapes and slides with `\n`, dropping the empty ones
//! (`node_modules/officeparser/dist/parsers/PowerPointParser.js:766`). Speaker
//! notes are included, and the default config leaves them in archive order, so
//! they are read after the slides here — which is the order a normally written
//! package stores them in.
//!
//! What is not reproduced is officeparser's content model: it also reads table
//! cells, chart data and embedded objects into its tree. Text inside table cells
//! is read here, because those cells hold `a:p` paragraphs too; chart data
//! lives in separate `ppt/charts/*` parts and is not read.

use quick_xml::events::Event;

use crate::ExtractError;
use crate::xml;
use crate::zip_read;

/// Text of a presentation: every paragraph of every slide and notes page.
pub fn parse(bytes: &[u8]) -> Result<String, ExtractError> {
    let mut archive = zip_read::open(bytes)?;
    let parts = presentation_parts(&archive);
    if parts.is_empty() {
        return Err(ExtractError::Container(
            "the package has no ppt/slides or ppt/notesSlides parts".to_string(),
        ));
    }

    let mut lines: Vec<String> = Vec::new();
    for part in parts {
        let xml_text = zip_read::read_text(&mut archive, &part)?;
        lines.extend(paragraphs(&xml_text)?);
    }
    Ok(lines.join("\n"))
}

/// Slide and notes entry names, slides first and each group in numeric order.
fn presentation_parts(archive: &zip_read::Archive<'_>) -> Vec<String> {
    let mut parts: Vec<(u8, u32, String)> = zip_read::names(archive)
        .into_iter()
        .filter_map(|name| part_number(&name).map(|(is_notes, number)| (is_notes, number, name)))
        .collect();
    parts.sort_by_key(|part| (part.0, part.1));
    parts.into_iter().map(|(_, _, name)| name).collect()
}

/// `(is_notes, number)` for a slide or notes part, `None` for anything else.
fn part_number(name: &str) -> Option<(u8, u32)> {
    let (prefix, is_notes) = if let Some(rest) = name.strip_prefix("ppt/slides/slide") {
        (rest, 0u8)
    } else {
        let rest = name.strip_prefix("ppt/notesSlides/notesSlide")?;
        (rest, 1u8)
    };
    prefix
        .strip_suffix(".xml")
        .and_then(|number| number.parse::<u32>().ok())
        .map(|number| (is_notes, number))
}

/// The text of every non-empty `a:p` in a slide, in document order.
fn paragraphs(slide: &str) -> Result<Vec<String>, ExtractError> {
    let mut reader = xml::reader_from_str(slide);
    let mut buffer = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_text_run = false;

    while let Some(event) = xml::next_event(&mut reader, &mut buffer)? {
        match event {
            Event::Start(element) => match element.local_name().as_ref() {
                b"t" => in_text_run = true,
                b"p" => current.clear(),
                _ => {}
            },
            Event::End(element) => match element.local_name().as_ref() {
                b"t" => in_text_run = false,
                b"p" => {
                    if !current.trim().is_empty() {
                        lines.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_text_run {
                    current.push_str(&xml::text_of(&text));
                }
            }
            Event::GeneralRef(reference) if in_text_run => {
                current.push_str(&xml::reference_of(&String::from_utf8_lossy(&reference)));
            }
            _ => {}
        }
    }
    Ok(lines)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::testkit::{slide_with, zip_of};

    #[test]
    fn slides_are_read_in_numeric_order() {
        let bytes = zip_of(&[
            ("ppt/slides/slide2.xml", &slide_with(&["Second slide"])),
            ("ppt/slides/slide10.xml", &slide_with(&["Tenth slide"])),
            ("ppt/slides/slide1.xml", &slide_with(&["First slide"])),
            ("[Content_Types].xml", "<Types/>"),
        ]);
        assert_eq!(
            parse(&bytes).unwrap(),
            "First slide\nSecond slide\nTenth slide"
        );
    }

    #[test]
    fn notes_follow_the_slides() {
        let bytes = zip_of(&[
            (
                "ppt/notesSlides/notesSlide1.xml",
                &slide_with(&["Speaker notes"]),
            ),
            ("ppt/slides/slide1.xml", &slide_with(&["Title", "Body"])),
        ]);
        assert_eq!(parse(&bytes).unwrap(), "Title\nBody\nSpeaker notes");
    }

    #[test]
    fn empty_paragraphs_are_dropped() {
        let bytes = zip_of(&[(
            "ppt/slides/slide1.xml",
            &slide_with(&["Real text", "", "   "]),
        )]);
        assert_eq!(parse(&bytes).unwrap(), "Real text");
    }

    #[test]
    fn a_package_without_slides_is_a_container_error() {
        let bytes = zip_of(&[("[Content_Types].xml", "<Types/>")]);
        let err = parse(&bytes).unwrap_err();
        assert!(matches!(err, ExtractError::Container(_)), "{err}");
    }
}
