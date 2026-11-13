//! Which of a document's pictures to index, and the text that indexes one
//! (`ING-5`). Pure: the engine hands in sizes and fingerprints, and gets back
//! decisions and strings.
//!
//! # Selection
//!
//! A standalone upload is always kept: the user chose that picture. A picture
//! *inside* a document is kept unless it is
//!
//! - too small to show anything (an icon, a bullet glyph): its short side
//!   under `min_side` px, or its area under `min_area`;
//! - a rule or a strip: aspect ratio over `max_aspect`;
//! - repeated `repeat_ornament` or more times in the document — the same logo
//!   or footer band on every slide — in which case every copy is skipped;
//! - a duplicate of an earlier picture (fewer repeats than that): only the
//!   first copy is kept;
//! - past the per-document cap, in document order.
//!
//! Every skip is counted by reason, so the document's metadata says what
//! happened to each picture.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One picture as the selector sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// The upload itself, never filtered.
    pub upload: bool,
    pub width: u32,
    pub height: u32,
    /// Identity of the pixels (a hash of the normalised bytes).
    pub fingerprint: u64,
}

/// The selection thresholds (`KB_IMAGE_*` settings).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectRules {
    pub min_side: u32,
    pub min_area: u64,
    pub max_aspect: f32,
    /// A picture seen this many times is page furniture; 0 turns it off.
    pub repeat_ornament: u32,
    /// Pictures indexed per document; 0 = no cap.
    pub max_per_document: u32,
}

impl Default for SelectRules {
    fn default() -> Self {
        Self {
            min_side: 48,
            min_area: 128 * 128,
            max_aspect: 8.0,
            repeat_ornament: 3,
            max_per_document: 100,
        }
    }
}

/// Pictures not indexed, by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkipCounts {
    pub too_small: u32,
    pub extreme_aspect: u32,
    pub duplicate: u32,
    pub repeated: u32,
    pub over_cap: u32,
}

/// The kept pictures (indices into the input, in order) and the skips.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub keep: Vec<usize>,
    pub skipped: SkipCounts,
}

/// Decide which candidates to index.
#[must_use]
pub fn select(candidates: &[Candidate], rules: &SelectRules) -> Selection {
    let mut copies: HashMap<u64, u32> = HashMap::new();
    for c in candidates.iter().filter(|c| !c.upload) {
        *copies.entry(c.fingerprint).or_default() += 1;
    }
    let mut seen: HashMap<u64, ()> = HashMap::new();
    let mut out = Selection::default();
    for (i, c) in candidates.iter().enumerate() {
        if !c.upload {
            let (short, long) = (c.width.min(c.height), c.width.max(c.height));
            if short < rules.min_side || u64::from(c.width) * u64::from(c.height) < rules.min_area {
                out.skipped.too_small += 1;
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            if long as f32 > short as f32 * rules.max_aspect {
                out.skipped.extreme_aspect += 1;
                continue;
            }
            if rules.repeat_ornament > 0
                && copies.get(&c.fingerprint).copied().unwrap_or(0) >= rules.repeat_ornament
            {
                out.skipped.repeated += 1;
                continue;
            }
            if seen.insert(c.fingerprint, ()).is_some() {
                out.skipped.duplicate += 1;
                continue;
            }
        }
        if rules.max_per_document > 0 && out.keep.len() >= rules.max_per_document as usize {
            out.skipped.over_cap += 1;
            continue;
        }
        out.keep.push(i);
    }
    out
}

/// What is known about one picture when its chunk is written.
#[derive(Debug, Clone, Copy, Default)]
pub struct VisualText<'a> {
    /// The describer's kind (`photo`, `chart`, …), when it read the picture.
    pub kind: Option<&'a str>,
    pub description: Option<&'a str>,
    /// Text legible in the picture.
    pub text: Option<&'a str>,
    pub alt_text: Option<&'a str>,
    pub caption: Option<&'a str>,
    /// Text of the page the picture sits on, for grounding.
    pub context: Option<&'a str>,
}

/// The label a chunk of this kind starts with.
#[must_use]
pub fn kind_label(kind: Option<&str>) -> &'static str {
    match kind.unwrap_or("") {
        "photo" => "Photo",
        "chart" => "Chart",
        "diagram" => "Diagram",
        "screenshot" => "Screenshot",
        "table" => "Table",
        "document" => "Document image",
        "map" => "Map",
        _ => "Image",
    }
}

fn present(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// The searchable text of a picture, or `None` when nothing is known about it
/// beyond the page it sits on — page text alone would only duplicate the
/// page's own chunks.
#[must_use]
pub fn visual_chunk_text(v: &VisualText<'_>, context_chars: usize) -> Option<String> {
    let description = present(v.description);
    let text = present(v.text);
    let alt = present(v.alt_text);
    let caption = present(v.caption);
    if description.is_none() && text.is_none() && alt.is_none() && caption.is_none() {
        return None;
    }
    let mut out = format!("[{}]", kind_label(v.kind));
    if let Some(c) = caption {
        out.push(' ');
        out.push_str(c);
    }
    if let Some(d) = description {
        out.push_str(if caption.is_some() { "\n" } else { " " });
        out.push_str(d);
    }
    if let Some(a) = alt.filter(|a| Some(*a) != caption && Some(*a) != description) {
        out.push_str("\nAlt text: ");
        out.push_str(a);
    }
    if let Some(t) = text {
        out.push_str("\nText in image: ");
        out.push_str(t);
    }
    if let Some(ctx) = present(v.context) {
        let snippet: String = ctx.chars().take(context_chars).collect();
        if !snippet.is_empty() {
            out.push_str("\nContext: ");
            out.push_str(snippet.trim());
        }
    }
    Some(out)
}

/// A short section title for the chunk: the caption, the alt text, or the
/// description's first sentence, at most 120 characters.
#[must_use]
pub fn visual_section(v: &VisualText<'_>) -> Option<String> {
    let source = present(v.caption)
        .or(present(v.alt_text))
        .or_else(|| present(v.description).map(first_sentence))?;
    let mut s: String = source.chars().take(120).collect();
    if source.chars().count() > 120 {
        s.push('…');
    }
    Some(s)
}

fn first_sentence(text: &str) -> &str {
    match text.find(". ") {
        Some(i) => &text[..=i],
        None => text,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn pic(w: u32, h: u32, fp: u64) -> Candidate {
        Candidate {
            upload: false,
            width: w,
            height: h,
            fingerprint: fp,
        }
    }

    #[test]
    fn icons_strips_and_repeated_logos_are_skipped_and_counted() {
        let rules = SelectRules::default();
        let cands = [
            pic(800, 600, 1),  // kept
            pic(32, 32, 2),    // icon
            pic(1200, 40, 3),  // rule: short side 40 < 48
            pic(1600, 150, 4), // strip: aspect 10.7
            pic(200, 200, 5),  // logo on three slides
            pic(200, 200, 5),
            pic(200, 200, 5),
            pic(640, 480, 6), // a photo repeated once
            pic(640, 480, 6),
        ];
        let s = select(&cands, &rules);
        assert_eq!(s.keep, vec![0, 7]);
        assert_eq!(
            s.skipped,
            SkipCounts {
                too_small: 2,
                extreme_aspect: 1,
                duplicate: 1,
                repeated: 3,
                over_cap: 0
            }
        );
    }

    #[test]
    fn an_upload_is_always_kept_even_when_tiny_or_repeated() {
        let mut tiny = pic(16, 16, 9);
        tiny.upload = true;
        let s = select(&[tiny, tiny, tiny], &SelectRules::default());
        assert_eq!(s.keep, vec![0, 1, 2]);
    }

    #[test]
    fn the_cap_keeps_the_first_pictures_in_document_order() {
        let rules = SelectRules {
            max_per_document: 2,
            ..SelectRules::default()
        };
        let cands: Vec<Candidate> = (0..5).map(|i| pic(400, 300, i)).collect();
        let s = select(&cands, &rules);
        assert_eq!(s.keep, vec![0, 1]);
        assert_eq!(s.skipped.over_cap, 3);
    }

    #[test]
    fn chunk_text_puts_what_the_picture_shows_first() {
        let v = VisualText {
            kind: Some("photo"),
            description: Some("A man swings a tennis racket in front of a crowd."),
            text: Some("OLYMPUS CITIZEN"),
            alt_text: Some("Final match"),
            caption: None,
            context: Some("Laporan kegiatan olahraga 2024"),
        };
        assert_eq!(
            visual_chunk_text(&v, 500).unwrap(),
            "[Photo] A man swings a tennis racket in front of a crowd.\n\
             Alt text: Final match\nText in image: OLYMPUS CITIZEN\n\
             Context: Laporan kegiatan olahraga 2024"
        );
        assert_eq!(visual_section(&v).unwrap(), "Final match");
    }

    #[test]
    fn nothing_known_but_page_text_is_no_chunk() {
        let v = VisualText {
            context: Some("page text"),
            ..VisualText::default()
        };
        assert!(visual_chunk_text(&v, 500).is_none());
        assert!(visual_section(&v).is_none());
    }

    #[test]
    fn a_long_description_gives_a_short_section() {
        let long = format!("{}. Second sentence.", "word ".repeat(40).trim());
        let v = VisualText {
            description: Some(&long),
            ..VisualText::default()
        };
        let section = visual_section(&v).unwrap();
        assert!(section.chars().count() <= 121, "{section}");
        assert!(section.ends_with('…'));
    }
}
