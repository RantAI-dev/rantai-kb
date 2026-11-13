//! `.ppt` (PowerPoint 97–2003 binary) → text: one line per paragraph, slides
//! in presentation order, then speaker notes, as the `.pptx` parser emits.
//!
//! A `.ppt` is an OLE compound file whose `PowerPoint Document` stream is a
//! tree of records, each an 8-byte header (`recVer`/`recInstance`, `recType`,
//! `recLen`) followed by its body; a `recVer` of `0xF` marks a container
//! ([MS-PPT] 2.3.1). Objects are found through the persist directory:
//!
//! 1. `Current User` gives the offset of the newest `UserEditAtom`;
//! 2. each `UserEditAtom` points at a `PersistDirectoryAtom` (persist id →
//!    stream offset) and at the previous edit, newest entries winning — that
//!    is how incremental ("fast") saves are layered;
//! 3. the `DocumentContainer` holds a `SlideListWithText` whose
//!    `SlidePersistAtom`s list the slides in presentation order.
//!
//! A slide's text lives in two places: placeholder text (titles, body
//! outlines) in the `SlideListWithText` between that slide's
//! `SlidePersistAtom` and the next, and free text boxes in the slide's own
//! drawing, as `TextCharsAtom` (UTF-16LE) or `TextBytesAtom` (8-bit) records.
//! Both are read. Notes pages are found through each slide's `SlideAtom`
//! (`notesIdRef`) and read the same way. Masters are never visited, so their
//! "Click to edit Master title style" prompts do not leak in.
//!
//! When the persist chain is damaged the parser does not give up: it reads
//! every text record in the stream, in stream order, which is what a
//! forensic text scraper would recover.

use std::collections::HashMap;
use std::io::Cursor;

use crate::ExtractError;
use crate::doc::{read_stream, u16_at, u32_at};

const RT_DOCUMENT: u16 = 0x03E8;
const RT_SLIDE: u16 = 0x03EE;
const RT_SLIDE_ATOM: u16 = 0x03EF;
const RT_NOTES: u16 = 0x03F0;
const RT_SLIDE_PERSIST_ATOM: u16 = 0x03F3;
const RT_TEXT_CHARS_ATOM: u16 = 0x0FA0;
const RT_TEXT_BYTES_ATOM: u16 = 0x0FA8;
const RT_SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
const RT_USER_EDIT_ATOM: u16 = 0x0FF5;
const RT_PERSIST_DIRECTORY_ATOM: u16 = 0x1772;
const HEADER_LEN: usize = 8;
/// A bound on the edit chain, so a cyclic `offsetLastEdit` cannot loop.
const MAX_EDITS: usize = 4_096;

/// Text of a presentation: every paragraph of every slide, then of every notes
/// page, one per line.
pub fn parse(bytes: &[u8]) -> Result<String, ExtractError> {
    let mut file = cfb::CompoundFile::open(Cursor::new(bytes)).map_err(|e| {
        ExtractError::Container(format!("not a PowerPoint 97-2003 presentation: {e}"))
    })?;
    if file.exists("/EncryptedSummary") {
        return Err(ExtractError::Unsupported(
            "the presentation is password-protected; remove the password and upload it again"
                .to_string(),
        ));
    }
    let stream = read_stream(&mut file, "/PowerPoint Document")?;
    let current_user = read_stream(&mut file, "/Current User").ok();

    let lines = current_user
        .and_then(|cu| structured_text(&stream, &cu))
        .unwrap_or_else(|| scan_all_text(&stream));
    Ok(lines.join("\n"))
}

/// One record header.
#[derive(Debug, Clone, Copy)]
struct Record {
    instance: u16,
    kind: u16,
    is_container: bool,
    /// Offset of the body.
    body: usize,
    /// Offset one past the body.
    end: usize,
}

fn record_at(stream: &[u8], at: usize) -> Option<Record> {
    let ver_inst = u16_at(stream, at)?;
    let kind = u16_at(stream, at + 2)?;
    let len = u32_at(stream, at + 4)? as usize;
    let body = at + HEADER_LEN;
    let end = body.checked_add(len)?;
    if end > stream.len() {
        return None;
    }
    Some(Record {
        instance: ver_inst >> 4,
        kind,
        is_container: ver_inst & 0x000F == 0x000F,
        body,
        end,
    })
}

/// The direct children of the records in `[from, to)`.
fn children(stream: &[u8], from: usize, to: usize) -> Vec<Record> {
    let mut out = Vec::new();
    let mut at = from;
    while at + HEADER_LEN <= to {
        let Some(record) = record_at(stream, at) else {
            break;
        };
        if record.end > to {
            break;
        }
        out.push(record);
        at = record.end;
    }
    out
}

/// The text of a text atom, split into paragraphs.
fn atom_text(stream: &[u8], record: &Record) -> Option<Vec<String>> {
    let body = &stream[record.body..record.end];
    let text = match record.kind {
        RT_TEXT_CHARS_ATOM => {
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        // TextBytesAtom: each byte is the low byte of a UTF-16 unit.
        RT_TEXT_BYTES_ATOM => body.iter().map(|b| char::from(*b)).collect(),
        _ => return None,
    };
    Some(paragraphs(&text))
}

/// Paragraphs are separated by `\r`; `\v` is a line break inside one.
fn paragraphs(text: &str) -> Vec<String> {
    text.split(['\r', '\n', '\u{b}'])
        .map(|line| {
            line.chars()
                .filter(|c| !c.is_control() || *c == '\t')
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|line| !line.is_empty())
        .collect()
}

/// Every text atom under `record`, in tree order.
fn text_under(stream: &[u8], record: &Record, out: &mut Vec<String>) {
    if let Some(lines) = atom_text(stream, record) {
        out.extend(lines);
        return;
    }
    if record.is_container {
        for child in children(stream, record.body, record.end) {
            text_under(stream, &child, out);
        }
    }
}

/// The persist id → stream offset map, newest edit winning.
fn persist_directory(stream: &[u8], current_user: &[u8]) -> Option<(HashMap<u32, usize>, u32)> {
    // CurrentUserAtom: 8-byte header, size, headerToken, offsetToCurrentEdit.
    let mut edit_at = u32_at(current_user, HEADER_LEN + 8)? as usize;
    let mut directory: HashMap<u32, usize> = HashMap::new();
    let mut doc_persist_id = None;
    for _ in 0..MAX_EDITS {
        let edit = record_at(stream, edit_at)?;
        if edit.kind != RT_USER_EDIT_ATOM {
            return None;
        }
        // UserEditAtom: lastSlideIdRef u32, version u16, minor u8, major u8,
        // offsetLastEdit u32, offsetPersistDirectory u32, docPersistIdRef u32.
        let last_edit = u32_at(stream, edit.body + 8)? as usize;
        let dir_at = u32_at(stream, edit.body + 12)? as usize;
        if doc_persist_id.is_none() {
            doc_persist_id = Some(u32_at(stream, edit.body + 16)?);
        }
        let dir = record_at(stream, dir_at)?;
        if dir.kind != RT_PERSIST_DIRECTORY_ATOM {
            return None;
        }
        let mut at = dir.body;
        while at + 4 <= dir.end {
            let entry = u32_at(stream, at)?;
            let first_id = entry & 0x000F_FFFF;
            let count = (entry >> 20) as usize;
            at += 4;
            for k in 0..count {
                let offset = u32_at(stream, at + k * 4)? as usize;
                #[allow(clippy::cast_possible_truncation)]
                let id = first_id + k as u32;
                // Older edits never override newer ones.
                directory.entry(id).or_insert(offset);
            }
            at += count * 4;
        }
        if last_edit == 0 || last_edit == edit_at {
            return Some((directory, doc_persist_id?));
        }
        edit_at = last_edit;
    }
    None
}

/// Slides in presentation order, each with its placeholder text from the
/// `SlideListWithText` and the text of its own drawing; then the notes.
fn structured_text(stream: &[u8], current_user: &[u8]) -> Option<Vec<String>> {
    let (directory, doc_id) = persist_directory(stream, current_user)?;
    let document = record_at(stream, *directory.get(&doc_id)?)?;
    if document.kind != RT_DOCUMENT {
        return None;
    }
    let slide_list = children(stream, document.body, document.end)
        .into_iter()
        .find(|r| r.kind == RT_SLIDE_LIST_WITH_TEXT && r.instance == 0)?;

    // (persist id, placeholder lines) per slide, in presentation order.
    let mut slides: Vec<(u32, Vec<String>)> = Vec::new();
    for record in children(stream, slide_list.body, slide_list.end) {
        if record.kind == RT_SLIDE_PERSIST_ATOM {
            slides.push((u32_at(stream, record.body)?, Vec::new()));
        } else if let Some((_, lines)) = slides.last_mut() {
            text_under(stream, &record, lines);
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut notes_ids: Vec<u32> = Vec::new();
    for (persist_id, placeholder_lines) in slides {
        out.extend(placeholder_lines);
        let Some(slide) = directory
            .get(&persist_id)
            .and_then(|at| record_at(stream, *at))
            .filter(|r| r.kind == RT_SLIDE)
        else {
            continue;
        };
        for child in children(stream, slide.body, slide.end) {
            if child.kind == RT_SLIDE_ATOM {
                // SlideAtom: geom u32, rgPlaceholderTypes [u8; 8],
                // masterIdRef u32, notesIdRef u32.
                if let Some(notes) = u32_at(stream, child.body + 16).filter(|id| *id != 0) {
                    notes_ids.push(notes);
                }
            } else {
                text_under(stream, &child, &mut out);
            }
        }
    }
    for notes_id in notes_ids {
        if let Some(notes) = directory
            .get(&notes_id)
            .and_then(|at| record_at(stream, *at))
            .filter(|r| r.kind == RT_NOTES)
        {
            text_under(stream, &notes, &mut out);
        }
    }
    Some(out)
}

/// Every text atom in the stream, in stream order: the fallback when the
/// persist chain cannot be followed.
fn scan_all_text(stream: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for record in children(stream, 0, stream.len()) {
        text_under(stream, &record, &mut out);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn atom(kind: u16, instance: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend((instance << 4).to_le_bytes());
        out.extend(kind.to_le_bytes());
        out.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
        out.extend(body);
        out
    }

    fn container(kind: u16, instance: u16, parts: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = parts.concat();
        let mut out = Vec::new();
        out.extend(((instance << 4) | 0xF).to_le_bytes());
        out.extend(kind.to_le_bytes());
        out.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
        out.extend(body);
        out
    }

    fn chars(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn paragraphs_split_on_carriage_returns_and_vertical_tabs() {
        assert_eq!(
            paragraphs("Title\rline one\u{b}line two\r\r"),
            vec!["Title", "line one", "line two"]
        );
    }

    #[test]
    fn the_fallback_scan_reads_both_text_atom_kinds_in_order() {
        let stream = container(
            RT_DOCUMENT,
            0,
            &[
                atom(RT_TEXT_CHARS_ATOM, 0, &chars("Résumé\rnext")),
                container(0xF000, 0, &[atom(RT_TEXT_BYTES_ATOM, 0, b"bytes text")]),
                atom(0x0FBA, 0, &chars("a CString is not slide text")),
            ],
        );
        assert_eq!(scan_all_text(&stream), vec!["Résumé", "next", "bytes text"]);
    }

    /// A stream laid out the way PowerPoint writes it: slides, the document
    /// listing them in a different order, one notes page, the persist
    /// directory and the edit atom last.
    fn presentation() -> (Vec<u8>, Vec<u8>) {
        let mut stream = Vec::new();
        let mut offsets: HashMap<u32, u32> = HashMap::new();
        let mut place = |stream: &mut Vec<u8>, id: u32, bytes: Vec<u8>| {
            offsets.insert(id, u32::try_from(stream.len()).unwrap());
            stream.extend(bytes);
        };
        // Slide 2's notes, persist id 5.
        place(
            &mut stream,
            5,
            container(
                RT_NOTES,
                0,
                &[atom(RT_TEXT_CHARS_ATOM, 0, &chars("speaker note"))],
            ),
        );
        // Slide persist 3 (shown second) with a text box; slide 4 (shown first).
        let mut slide_atom_with_notes = vec![0u8; 24];
        slide_atom_with_notes[16..20].copy_from_slice(&5u32.to_le_bytes());
        place(
            &mut stream,
            3,
            container(
                RT_SLIDE,
                0,
                &[
                    atom(RT_SLIDE_ATOM, 0, &slide_atom_with_notes),
                    container(0xF00D, 0, &[atom(RT_TEXT_BYTES_ATOM, 0, b"free text box")]),
                ],
            ),
        );
        place(
            &mut stream,
            4,
            container(RT_SLIDE, 0, &[atom(RT_SLIDE_ATOM, 0, &[0u8; 24])]),
        );
        place(
            &mut stream,
            1,
            container(
                RT_DOCUMENT,
                0,
                &[container(
                    RT_SLIDE_LIST_WITH_TEXT,
                    0,
                    &[
                        atom(RT_SLIDE_PERSIST_ATOM, 0, &[4, 0, 0, 0, 0, 0, 0, 0]),
                        atom(RT_TEXT_CHARS_ATOM, 0, &chars("First title\rfirst body")),
                        atom(RT_SLIDE_PERSIST_ATOM, 0, &[3, 0, 0, 0, 0, 0, 0, 0]),
                        atom(RT_TEXT_CHARS_ATOM, 0, &chars("Second title")),
                    ],
                )],
            ),
        );
        let mut dir_body = Vec::new();
        for id in 1u32..=5 {
            if let Some(offset) = offsets.get(&id) {
                dir_body.extend((id | (1 << 20)).to_le_bytes());
                dir_body.extend(offset.to_le_bytes());
            }
        }
        let dir_at = u32::try_from(stream.len()).unwrap();
        stream.extend(atom(RT_PERSIST_DIRECTORY_ATOM, 0, &dir_body));
        let edit_at = u32::try_from(stream.len()).unwrap();
        let mut edit = Vec::new();
        edit.extend(0u32.to_le_bytes()); // lastSlideIdRef
        edit.extend([0, 0, 0, 3]); // version, minor, major
        edit.extend(0u32.to_le_bytes()); // offsetLastEdit
        edit.extend(dir_at.to_le_bytes());
        edit.extend(1u32.to_le_bytes()); // docPersistIdRef
        stream.extend(atom(RT_USER_EDIT_ATOM, 0, &edit));

        let mut current_user = vec![0u8; HEADER_LEN + 8];
        current_user.extend(edit_at.to_le_bytes());
        (stream, current_user)
    }

    #[test]
    fn slides_come_in_presentation_order_then_notes() {
        let (stream, current_user) = presentation();
        assert_eq!(
            structured_text(&stream, &current_user).unwrap(),
            vec![
                "First title",
                "first body",
                "Second title",
                "free text box",
                "speaker note"
            ]
        );
    }

    #[test]
    fn a_broken_edit_pointer_falls_back_to_the_scan() {
        let (stream, mut current_user) = presentation();
        let len = current_user.len();
        current_user[len - 4..].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
        assert!(structured_text(&stream, &current_user).is_none());
        assert!(scan_all_text(&stream).contains(&"Second title".to_string()));
    }

    #[test]
    fn bytes_that_are_not_a_compound_file_are_rejected_with_a_reason() {
        let err = parse(b"nope").unwrap_err();
        assert!(err.to_string().contains("PowerPoint 97-2003"), "{err}");
    }
}
