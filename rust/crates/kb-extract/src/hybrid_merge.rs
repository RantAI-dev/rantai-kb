//! Pure merge algorithm combining structural Markdown with text-layer character fidelity.
//!
//! Mirrors `src/lib/rag/extractors/hybrid-merge.ts`.
//!
//! Keeps all structural blocks (tables, headings, code, latex) verbatim and
//! substitutes text-layer text into prose blocks wherever start and end 5-word
//! anchors match cleanly.

#[derive(Debug, PartialEq, Eq)]
enum BlockKind {
    Prose,
    Heading,
    Table,
    Code,
    Latex,
    Blank,
}

#[derive(Debug, PartialEq, Eq)]
struct Block {
    kind: BlockKind,
    raw: String,
}

const ANCHOR_WORDS: usize = 5;
const LENGTH_RATIO_MIN: f64 = 0.7;
const LENGTH_RATIO_MAX: f64 = 1.5;

fn is_atx_heading(trimmed: &str) -> bool {
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() || bytes[0] != b'#' {
        return false;
    }
    let mut count = 0usize;
    for &b in bytes {
        if b == b'#' {
            count += 1;
        } else {
            break;
        }
    }
    (1..=6).contains(&count) && bytes.get(count) == Some(&b' ')
}

fn parse_blocks(md: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0usize;
    let mut prose: Vec<&str> = Vec::new();

    let flush_prose = |prose: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !prose.is_empty() {
            blocks.push(Block {
                kind: BlockKind::Prose,
                raw: prose.join("\n"),
            });
            prose.clear();
        }
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            flush_prose(&mut prose, &mut blocks);
            blocks.push(Block {
                kind: BlockKind::Blank,
                raw: String::new(),
            });
            i += 1;
            continue;
        }

        // HTML table: span from <table...> through </table> (or starting with div containing table)
        let is_div_table = trimmed.starts_with("<div")
            && lines[i..lines.len().min(i + 3)]
                .iter()
                .any(|l| l.contains("<table"));
        if trimmed.starts_with("<table") || is_div_table {
            flush_prose(&mut prose, &mut blocks);
            let mut buf: Vec<&str> = Vec::new();
            while i < lines.len() {
                buf.push(lines[i]);
                if lines[i].contains("</table>") {
                    i += 1;
                    if i < lines.len() && lines[i].trim().starts_with("</div>") {
                        buf.push(lines[i]);
                        i += 1;
                    }
                    break;
                }
                i += 1;
            }
            blocks.push(Block {
                kind: BlockKind::Table,
                raw: buf.join("\n"),
            });
            continue;
        }

        // Markdown pipe table: at least two rows starting with '|'
        if trimmed.starts_with('|') && lines.get(i + 1).is_some_and(|l| l.trim().starts_with('|')) {
            flush_prose(&mut prose, &mut blocks);
            let mut buf: Vec<&str> = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('|') {
                buf.push(lines[i]);
                i += 1;
            }
            blocks.push(Block {
                kind: BlockKind::Table,
                raw: buf.join("\n"),
            });
            continue;
        }

        // ATX heading
        if is_atx_heading(trimmed) {
            flush_prose(&mut prose, &mut blocks);
            blocks.push(Block {
                kind: BlockKind::Heading,
                raw: line.to_string(),
            });
            i += 1;
            continue;
        }

        // Code fence
        if trimmed.starts_with("```") {
            flush_prose(&mut prose, &mut blocks);
            let mut buf = vec![lines[i]];
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with("```") {
                buf.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                buf.push(lines[i]);
                i += 1;
            }
            blocks.push(Block {
                kind: BlockKind::Code,
                raw: buf.join("\n"),
            });
            continue;
        }

        // Block LaTeX
        if trimmed == "$$" {
            flush_prose(&mut prose, &mut blocks);
            let mut buf = vec![lines[i]];
            i += 1;
            while i < lines.len() && lines[i].trim() != "$$" {
                buf.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                buf.push(lines[i]);
                i += 1;
            }
            blocks.push(Block {
                kind: BlockKind::Latex,
                raw: buf.join("\n"),
            });
            continue;
        }

        // Plain prose
        prose.push(line);
        i += 1;
    }

    flush_prose(&mut prose, &mut blocks);
    blocks
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Position {
    Start,
    End,
}

fn extract_anchor(text: &str, position: Position, word_count: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return String::new();
    }
    if words.len() <= word_count {
        return words.join(" ");
    }
    match position {
        Position::Start => words[..word_count].join(" "),
        Position::End => words[words.len() - word_count..].join(" "),
    }
}

fn normalize_space(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn try_substitute(prose_raw: &str, normalized_text_layer: &str) -> Option<String> {
    let prose = prose_raw.trim();
    let start_anchor = extract_anchor(prose, Position::Start, ANCHOR_WORDS);
    let end_anchor = extract_anchor(prose, Position::End, ANCHOR_WORDS);
    if start_anchor.is_empty() || end_anchor.is_empty() {
        return None;
    }

    let norm_start = normalize_space(&start_anchor).to_lowercase();
    let norm_end = normalize_space(&end_anchor).to_lowercase();
    let hay_lower = normalized_text_layer.to_lowercase();

    let start_idx = hay_lower.find(&norm_start)?;
    let end_search_from = start_idx + norm_start.len();
    if end_search_from > hay_lower.len() {
        return None;
    }

    let end_offset = hay_lower[end_search_from..].find(&norm_end)?;
    let end_idx = end_search_from + end_offset;
    let end_anchor_len = normalize_space(&end_anchor).len();
    let span_end = (end_idx + end_anchor_len).min(normalized_text_layer.len());

    let span = &normalized_text_layer[start_idx..span_end];

    let ratio = (span.len() as f64) / (prose.len() as f64);
    if !(LENGTH_RATIO_MIN..=LENGTH_RATIO_MAX).contains(&ratio) {
        return None;
    }

    Some(span.to_string())
}

/// Merge structural markdown (tables, headings, math) with character-fidelity
/// prose from a text layer.
#[must_use]
pub fn merge_structural_with_text_layer(structural: &str, text_layer: &str) -> String {
    if text_layer.trim().is_empty() {
        return structural.to_string();
    }
    if structural.trim().is_empty() {
        return text_layer.to_string();
    }

    let normalized_text_layer = normalize_space(text_layer);
    let blocks = parse_blocks(structural);
    let mut out: Vec<String> = Vec::new();

    for block in blocks {
        match block.kind {
            BlockKind::Blank => out.push(String::new()),
            BlockKind::Prose => {
                let sub = try_substitute(&block.raw, &normalized_text_layer);
                out.push(sub.unwrap_or(block.raw));
            }
            BlockKind::Heading | BlockKind::Table | BlockKind::Code | BlockKind::Latex => {
                out.push(block.raw);
            }
        }
    }

    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headings_and_tables_and_prose() {
        let md = "# Title\n\nSome paragraph text here.\n\n| Col1 | Col2 |\n| --- | --- |\n| Val1 | Val2 |\n\nFooter note.";
        let blocks = parse_blocks(md);
        assert_eq!(blocks.len(), 7);
        assert_eq!(blocks[0].kind, BlockKind::Heading);
        assert_eq!(blocks[1].kind, BlockKind::Blank);
        assert_eq!(blocks[2].kind, BlockKind::Prose);
        assert_eq!(blocks[3].kind, BlockKind::Blank);
        assert_eq!(blocks[4].kind, BlockKind::Table);
        assert_eq!(blocks[5].kind, BlockKind::Blank);
        assert_eq!(blocks[6].kind, BlockKind::Prose);
    }

    #[test]
    fn substitutes_prose_while_keeping_table() {
        let structural = "# Report\n\nThe dominant sequence transduction models are based on recurrent networks. Aidan and Lukasz Kaiser worked on this. Our model achieves state-of-the-art BLEU on WMT 2014.\n\n| A | B |\n|---|---|\n| 1 | 2 |";
        let text_layer = "The dominant sequence transduction models are based on recurrent networks. Aidan and Łukasz Kaiser worked on this. Our model achieves state-of-the-art BLEU on WMT 2014.";
        let merged = merge_structural_with_text_layer(structural, text_layer);
        assert!(merged.contains("# Report"));
        assert!(merged.contains("Łukasz"));
        assert!(merged.contains("| A | B |"));
    }
}
