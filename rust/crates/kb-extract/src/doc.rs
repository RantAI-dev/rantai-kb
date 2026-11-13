//! `.doc` (Word 97–2003 binary) → text.
//!
//! A `.doc` is an OLE compound file. Its text is not stored in reading order:
//! the `WordDocument` stream holds runs of characters, and a piece table (the
//! `Clx`, in the `0Table` or `1Table` stream) maps character positions to the
//! byte ranges that hold them, each either 8-bit (Windows-1252) or UTF-16LE.
//! The layout below is from [MS-DOC] 2.5 (the FIB) and 2.9.38 (the `Clx`).
//!
//! The document's character positions are laid out as consecutive "stories":
//! main text, footnotes, headers, comments, endnotes, text boxes and header
//! text boxes. Read here: main text, then footnotes, endnotes and text boxes,
//! which are content a reader of the document sees. Headers and footers repeat
//! on every page and comments are review notes, so they are left out, as the
//! `.docx` parser leaves them out.
//!
//! Inside the text, a few control characters carry structure:
//!
//! - `0x0D` ends a paragraph, `0x0B` is a line break, `0x0C` a page or section
//!   break, `0x0E` a column break: each becomes a paragraph break;
//! - `0x07` ends a table cell or row: each cell becomes its own paragraph, the
//!   way the `.docx` parser emits a table;
//! - `0x13` … `0x14` … `0x15` is a field: the instruction between `0x13` and
//!   `0x14` (`PAGE`, `HYPERLINK "…"`, `TOC \o`) is dropped and the displayed
//!   result between `0x14` and `0x15` is kept. Fields nest;
//! - `0x01`, `0x08` (pictures, drawn objects), `0x02`, `0x05` (note and comment
//!   anchors) and `0x1F` (soft hyphen) carry no text; `0x1E` is a
//!   non-breaking hyphen.
//!
//! Not supported, with a reason rather than garbage: Word 6.0/95 files (an
//! older FIB with no piece table) and encrypted or obfuscated files.

use std::io::{Cursor, Read};

use crate::ExtractError;

/// `wIdent` of a Word binary FIB.
const WORD_IDENT: u16 = 0xA5EC;
/// The first `nFib` of Word 97. Word 6.0 and 95 write 101–105.
const NFIB_WORD97: u16 = 0xC0;
/// `FibBase` flag bits ([MS-DOC] 2.5.2).
const F_ENCRYPTED: u16 = 0x0100;
const F_WHICH_TBL_STM: u16 = 0x0200;
const F_OBFUSCATED: u16 = 0x8000;
/// `fcClx` is the 34th `fc`/`lcb` pair of `FibRgFcLcb97`.
const FC_CLX_INDEX: usize = 33;

/// Text of a Word 97–2003 document, one paragraph per `\n\n`-separated block.
pub fn parse(bytes: &[u8]) -> Result<String, ExtractError> {
    let mut file = cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|e| ExtractError::Container(format!("not a Word 97-2003 document: {e}")))?;
    let word = read_stream(&mut file, "/WordDocument")?;
    let fib = Fib::parse(&word)?;
    let table = read_stream(&mut file, fib.table_stream)?;
    let clx = slice(&table, fib.fc_clx, fib.lcb_clx)
        .ok_or_else(|| container("the piece table lies outside the table stream"))?;
    let pieces = parse_clx(clx)?;

    let mut stories: Vec<String> = Vec::new();
    for (start, len) in fib.content_stories() {
        if len == 0 {
            continue;
        }
        let units = characters(&word, &pieces, start, start + len)?;
        let text = render(&units);
        if !text.is_empty() {
            stories.push(text);
        }
    }
    Ok(stories.join("\n\n"))
}

pub(crate) fn read_stream<F: Read + std::io::Seek>(
    file: &mut cfb::CompoundFile<F>,
    path: &str,
) -> Result<Vec<u8>, ExtractError> {
    let mut stream = file
        .open_stream(path)
        .map_err(|e| container(&format!("the {path} stream is missing: {e}")))?;
    let mut out = Vec::new();
    stream
        .read_to_end(&mut out)
        .map_err(|e| container(&format!("the {path} stream could not be read: {e}")))?;
    Ok(out)
}

fn container(message: &str) -> ExtractError {
    ExtractError::Container(format!("Word 97-2003 document: {message}"))
}

/// The fields of the File Information Block this parser needs.
#[derive(Debug)]
struct Fib {
    table_stream: &'static str,
    fc_clx: usize,
    lcb_clx: usize,
    /// `ccpText`, `ccpFtn`, `ccpHdd`, `ccpAtn`, `ccpEdn`, `ccpTxbx`.
    ccp_text: usize,
    ccp_ftn: usize,
    ccp_hdd: usize,
    ccp_atn: usize,
    ccp_edn: usize,
    ccp_txbx: usize,
}

impl Fib {
    fn parse(word: &[u8]) -> Result<Self, ExtractError> {
        let ident = u16_at(word, 0).ok_or_else(|| container("the FIB is truncated"))?;
        if ident != WORD_IDENT {
            return Err(container("the WordDocument stream has no Word FIB"));
        }
        let n_fib = u16_at(word, 2).unwrap_or(0);
        if n_fib < NFIB_WORD97 {
            return Err(ExtractError::Unsupported(format!(
                "this is a Word 6.0/95 document (nFib {n_fib}); only Word 97-2003 .doc files are supported. Re-save it as .docx"
            )));
        }
        let flags = u16_at(word, 0x0A).unwrap_or(0);
        if flags & (F_ENCRYPTED | F_OBFUSCATED) != 0 {
            return Err(ExtractError::Unsupported(
                "the Word document is password-protected; remove the password and upload it again"
                    .to_string(),
            ));
        }
        let table_stream = if flags & F_WHICH_TBL_STM != 0 {
            "/1Table"
        } else {
            "/0Table"
        };

        // FibBase is 32 bytes; then `csw` u16 words, `cslw` u32 longs, and
        // `cbRgFcLcb` fc/lcb pairs, each array preceded by its count. Reading
        // the counts rather than assuming 97's sizes keeps later FIBs working.
        let csw = usize::from(u16_at(word, 32).ok_or_else(|| container("the FIB is truncated"))?);
        let lw_count_at = 34 + csw * 2;
        let cslw = usize::from(
            u16_at(word, lw_count_at).ok_or_else(|| container("the FIB is truncated"))?,
        );
        let rg_lw = lw_count_at + 2;
        let fc_lcb_count_at = rg_lw + cslw * 4;
        let rg_fc_lcb = fc_lcb_count_at + 2;
        let lw = |index: usize| -> Result<usize, ExtractError> {
            u32_at(word, rg_lw + index * 4)
                .map(|v| v as usize)
                .ok_or_else(|| container("the FIB is truncated"))
        };
        let fc_clx = u32_at(word, rg_fc_lcb + FC_CLX_INDEX * 8)
            .ok_or_else(|| container("the FIB has no piece table pointer"))?;
        let lcb_clx = u32_at(word, rg_fc_lcb + FC_CLX_INDEX * 8 + 4)
            .ok_or_else(|| container("the FIB has no piece table pointer"))?;

        Ok(Self {
            table_stream,
            fc_clx: fc_clx as usize,
            lcb_clx: lcb_clx as usize,
            ccp_text: lw(3)?,
            ccp_ftn: lw(4)?,
            ccp_hdd: lw(5)?,
            ccp_atn: lw(7)?,
            ccp_edn: lw(8)?,
            ccp_txbx: lw(9)?,
        })
    }

    /// `(first character position, length)` of the stories read: main text,
    /// footnotes, endnotes, text boxes. Headers and comments are skipped over.
    fn content_stories(&self) -> [(usize, usize); 4] {
        let ftn = self.ccp_text;
        let hdd = ftn + self.ccp_ftn;
        let atn = hdd + self.ccp_hdd;
        let edn = atn + self.ccp_atn;
        let txbx = edn + self.ccp_edn;
        [
            (0, self.ccp_text),
            (ftn, self.ccp_ftn),
            (edn, self.ccp_edn),
            (txbx, self.ccp_txbx),
        ]
    }
}

/// One piece: characters `[cp_start, cp_end)` stored from byte `offset` of the
/// WordDocument stream, 8-bit when `compressed`.
#[derive(Debug, Clone, Copy)]
struct Piece {
    cp_start: usize,
    cp_end: usize,
    offset: usize,
    compressed: bool,
}

/// The `Pcdt` of a `Clx`: skip the `Prc` property blocks, then read the
/// `PlcPcd` of character positions and piece descriptors.
fn parse_clx(clx: &[u8]) -> Result<Vec<Piece>, ExtractError> {
    let mut at = 0;
    loop {
        match clx.get(at) {
            Some(0x01) => {
                let cb = i16::from_le_bytes([
                    *clx.get(at + 1).ok_or_else(|| container("truncated Prc"))?,
                    *clx.get(at + 2).ok_or_else(|| container("truncated Prc"))?,
                ]);
                let cb = usize::try_from(cb).map_err(|_| container("negative Prc size"))?;
                at += 3 + cb;
            }
            Some(0x02) => break,
            Some(other) => {
                return Err(container(&format!(
                    "unexpected Clx block type {other:#04x}"
                )));
            }
            None => return Err(container("the piece table has no Pcdt")),
        }
    }
    let lcb = u32_at(clx, at + 1).ok_or_else(|| container("truncated Pcdt"))? as usize;
    let plc = slice(clx, at + 5, lcb).ok_or_else(|| container("truncated PlcPcd"))?;
    // n pieces take (n + 1) * 4 bytes of positions and n * 8 of descriptors.
    if plc.len() < 4 || (plc.len() - 4) % 12 != 0 {
        return Err(container("malformed PlcPcd"));
    }
    let n = (plc.len() - 4) / 12;
    let mut pieces = Vec::with_capacity(n);
    for k in 0..n {
        let cp_start = u32_at(plc, k * 4).unwrap_or(0) as usize;
        let cp_end = u32_at(plc, (k + 1) * 4).unwrap_or(0) as usize;
        let fc = u32_at(plc, (n + 1) * 4 + k * 8 + 2).unwrap_or(0);
        let compressed = fc & 0x4000_0000 != 0;
        let raw = (fc & 0x3FFF_FFFF) as usize;
        pieces.push(Piece {
            cp_start,
            cp_end,
            offset: if compressed { raw / 2 } else { raw },
            compressed,
        });
    }
    Ok(pieces)
}

/// The UTF-16 code units of character positions `[from, to)`.
fn characters(
    word: &[u8],
    pieces: &[Piece],
    from: usize,
    to: usize,
) -> Result<Vec<u16>, ExtractError> {
    let mut out = Vec::with_capacity(to.saturating_sub(from));
    for piece in pieces {
        let start = piece.cp_start.max(from);
        let end = piece.cp_end.min(to);
        if start >= end {
            continue;
        }
        let skip = start - piece.cp_start;
        let count = end - start;
        if piece.compressed {
            let bytes = slice(word, piece.offset + skip, count)
                .ok_or_else(|| container("a text piece lies outside the document stream"))?;
            out.extend(bytes.iter().map(|b| cp1252(*b)));
        } else {
            let bytes = slice(word, piece.offset + skip * 2, count * 2)
                .ok_or_else(|| container("a text piece lies outside the document stream"))?;
            out.extend(
                bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]])),
            );
        }
    }
    Ok(out)
}

/// What a field is showing at the moment: its instruction or its result.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldPart {
    Instruction,
    Result,
}

/// Turn Word's character stream into paragraphs separated by blank lines.
pub(crate) fn render(units: &[u16]) -> String {
    let mut text: Vec<u16> = Vec::with_capacity(units.len());
    let mut fields: Vec<FieldPart> = Vec::new();
    for &unit in units {
        match unit {
            0x13 => fields.push(FieldPart::Instruction),
            0x14 => {
                if let Some(top) = fields.last_mut() {
                    *top = FieldPart::Result;
                }
            }
            0x15 => {
                fields.pop();
            }
            _ if fields.contains(&FieldPart::Instruction) => {}
            0x0D | 0x0B | 0x0C | 0x0E | 0x07 => text.push(u16::from(b'\n')),
            0x09 => text.push(u16::from(b'\t')),
            0x1E => text.push(u16::from(b'-')),
            0x00..=0x1F => {}
            other => text.push(other),
        }
    }
    let decoded = String::from_utf16_lossy(&text);
    decoded
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Windows-1252 byte → UTF-16 unit; the 8-bit pieces of a `.doc` use it.
pub(crate) fn cp1252(byte: u8) -> u16 {
    const HIGH: [u16; 32] = [
        0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
        0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
    ];
    match byte {
        0x80..=0x9F => HIGH[usize::from(byte - 0x80)],
        other => u16::from(other),
    }
}

pub(crate) fn slice(bytes: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    bytes.get(at..at.checked_add(len)?)
}

pub(crate) fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let b = slice(bytes, at, 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

pub(crate) fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let b = slice(bytes, at, 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn paragraphs_cells_and_breaks_become_blank_line_separated_blocks() {
        let text = render(&units(
            "Title\rfirst\u{b}second\r\rA\u{7}B\u{7}\u{7}\u{c}end\r",
        ));
        assert_eq!(text, "Title\n\nfirst\n\nsecond\n\nA\n\nB\n\nend");
    }

    #[test]
    fn a_field_keeps_its_result_and_drops_its_instruction() {
        let text = render(&units(
            "See \u{13} HYPERLINK \"https://x\" \u{14}the site\u{15} and page \u{13}PAGE\u{14}3\u{15}.",
        ));
        assert_eq!(text, "See the site and page 3.");
    }

    #[test]
    fn a_nested_field_instruction_is_dropped_too() {
        let text = render(&units(
            "\u{13}IF \u{13}PAGE\u{14}1\u{15} = 1 \"a\" \"b\"\u{14}a\u{15} done",
        ));
        assert_eq!(text, "a done");
    }

    #[test]
    fn anchors_and_soft_hyphens_vanish_and_a_hard_hyphen_stays() {
        let text = render(&units("x\u{1}y\u{2}z\u{1f}w\u{1e}v\u{8}"));
        assert_eq!(text, "xyzw-v");
    }

    #[test]
    fn the_8_bit_pieces_decode_as_windows_1252() {
        assert_eq!(cp1252(0x93), 0x201C);
        assert_eq!(cp1252(0x80), 0x20AC);
        assert_eq!(cp1252(b'a'), u16::from(b'a'));
        assert_eq!(cp1252(0xE9), 0x00E9);
    }

    #[test]
    fn a_clx_with_a_property_block_then_two_pieces() {
        // Prc of 2 bytes, then a Pcdt of two pieces: "Hi" 8-bit at byte 0x10,
        // "é!" UTF-16 at byte 0x40.
        let mut plc = Vec::new();
        for cp in [0u32, 2, 4] {
            plc.extend(cp.to_le_bytes());
        }
        for fc in [(0x10u32 * 2) | 0x4000_0000, 0x40] {
            plc.extend([0, 0]);
            plc.extend(fc.to_le_bytes());
            plc.extend([0, 0]);
        }
        let mut clx = vec![0x01, 0x02, 0x00, 0xAA, 0xBB, 0x02];
        clx.extend(u32::try_from(plc.len()).unwrap().to_le_bytes());
        clx.extend(&plc);

        let pieces = parse_clx(&clx).unwrap();
        assert_eq!(pieces.len(), 2);
        let mut word = vec![0u8; 0x50];
        word[0x10..0x12].copy_from_slice(b"Hi");
        word[0x40..0x44].copy_from_slice(&[0xE9, 0x00, b'!', 0x00]);
        let got = characters(&word, &pieces, 0, 4).unwrap();
        assert_eq!(String::from_utf16_lossy(&got), "Hié!");
        // A story that starts mid-piece reads from the right offset.
        let got = characters(&word, &pieces, 1, 3).unwrap();
        assert_eq!(String::from_utf16_lossy(&got), "ié");
    }

    #[test]
    fn a_piece_outside_the_stream_is_a_reason_not_a_panic() {
        let pieces = [Piece {
            cp_start: 0,
            cp_end: 10,
            offset: 1_000,
            compressed: true,
        }];
        let err = characters(&[0u8; 8], &pieces, 0, 10).unwrap_err();
        assert!(err.to_string().contains("outside"), "{err}");
    }

    #[test]
    fn bytes_that_are_not_a_compound_file_are_rejected_with_a_reason() {
        let err = parse(b"plain text, not OLE").unwrap_err();
        assert!(err.to_string().contains("Word 97-2003"), "{err}");
    }
}
