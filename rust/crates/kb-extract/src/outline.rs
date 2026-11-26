//! PDF outline extraction: the document's section hierarchy for the tree arm.
//!
//! This is a port of EVAL-2's prototype (`rust/crates/kb-eval/scripts/outline.py`),
//! with the same two methods and the same rules:
//!
//! * **bookmarks** — the PDF's own outline. pdfium exposes it as a tree; each
//!   bookmark's destination gives its page. The bookmark's nesting is the level,
//!   but a title that carries its own numbering ("2.1.1") is trusted over the
//!   nesting, because producers sometimes elide parent bookmarks and the number
//!   in the title is exact.
//! * **headings** — no bookmarks. Every page's text is scanned for a numbered
//!   heading ("3.4 Title", "Chapter 3 …"). Table-of-contents pages are skipped
//!   first, or every heading would be "discovered" on the contents page and win
//!   the first-occurrence dedupe.
//! * **none** — neither produced anything. Reported, not silently dropped, so a
//!   corpus with missing structure is visible.
//!
//! End pages follow the rule a reader uses: a section ends on the page before
//! the next section at the same or a higher level starts. A section that shares
//! its start page with its first child therefore has a one-page span, which is
//! the honest answer.
//!
//! Page numbers in [`OutlineNode`] are **zero-based**, matching
//! `PageText.page`; the prototype printed 1-based PDF pages and the conversion
//! happens here, once.

use std::collections::HashSet;

use kb_core::tree::{Outline, OutlineMethod, OutlineNode, numbering_level};
use pdfium_render::prelude::{PdfDocument, Pdfium};

/// Parse the PDF's bookmark tree, in document order.
///
/// A bookmark with no destination is skipped but its children are still
/// visited: it groups rather than points at a page.
#[must_use]
pub fn bookmark_nodes(document: &PdfDocument<'_>) -> Vec<OutlineNode> {
    let Some(root) = document.bookmarks().root() else {
        return Vec::new();
    };
    let mut raw: Vec<(String, u32, u32)> = Vec::new();
    {
        let mut visited: HashSet<usize> = HashSet::new();
        let mut stack: Vec<(pdfium_render::prelude::PdfBookmark<'_>, u32)> = vec![(root, 1)];
        while let Some((bookmark, depth)) = stack.pop() {
            // A cyclic outline must not loop forever: producers do write them.
            let handle = handle_of(&bookmark);
            if !visited.insert(handle) {
                continue;
            }
            if let Some(title) = bookmark.title()
                && let Some(page) = bookmark
                    .destination()
                    .and_then(|destination| destination.page_index().ok())
            {
                raw.push((clean_title(&title), depth, u32::try_from(page).unwrap_or(0)));
            }
            // DFS pre-order: the next sibling waits while the children are
            // visited, because document order is what end pages depend on.
            if let Some(sibling) = bookmark.next_sibling() {
                stack.push((sibling, depth));
            }
            let mut children: Vec<pdfium_render::prelude::PdfBookmark<'_>> =
                bookmark.iter_direct_children().collect();
            children.reverse();
            for child in children {
                stack.push((child, depth + 1));
            }
        }
    }
    raw.into_iter()
        .map(|(title, depth, page)| {
            let level = numbering_level(&title).unwrap_or(depth.max(1));
            OutlineNode {
                title,
                level,
                start_page: page,
                end_page: page,
            }
        })
        .collect()
}

/// The internal handle of a bookmark, for the visited set.
///
/// `PdfBookmark` implements `Hash`, so hashing it is the supported identity
/// check; the hash is used here because the iterator needs an owned key and the
/// handle itself is private.
fn handle_of(bookmark: &pdfium_render::prelude::PdfBookmark<'_>) -> usize {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    bookmark.hash(&mut hasher);
    hasher.finish() as usize
}

/// Collapse whitespace and remove a stray trailing footnote marker.
#[must_use]
pub fn clean_title(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    strip_footnote_marker(&collapsed)
}

/// Remove a trailing " (3F )" / " (12)" that bookmark producers add.
fn strip_footnote_marker(text: &str) -> String {
    let trimmed = text.trim_end();
    let Some(open) = trimmed.rfind('(') else {
        return trimmed.to_string();
    };
    if !trimmed.ends_with(')') {
        return trimmed.to_string();
    }
    let inner = &trimmed[open + 1..trimmed.len() - 1];
    let mut digits = inner.chars();
    let first = digits.next();
    if first.is_some_and(|c| c.is_ascii_digit())
        && digits.all(|c| c.is_ascii_digit() || c.is_ascii_alphabetic() || c == ' ')
        && inner.len() <= 6
    {
        return trimmed[..open].trim_end().to_string();
    }
    trimmed.to_string()
}

/// Numbered headings found in page text, in document order, first wins.
///
/// `pages` is one string per page, zero-based, with empty strings for pages
/// that have no text layer.
#[must_use]
pub fn heading_nodes(pages: &[String]) -> Vec<OutlineNode> {
    // Page furniture: a line printed on three or more pages once its digits
    // are gone ("2 Kelas VIII SMP/MTs Semester 1", "Ilmu Pengetahuan Alam 3")
    // is a running header or footer, not a heading. Chapter-word lines are
    // exempt: a "Bab 1 Vektor" running header is still that chapter, and the
    // first-wins dedupe below keeps only its first page.
    let furniture = furniture_keys(pages);
    // A book with two or more chapter headings is structured by them; its
    // bare "3. Mistar" lines are list items and exercises, not chapters.
    let chaptered = pages
        .iter()
        .filter(|text| !is_toc_page(text))
        .flat_map(|text| text.lines())
        .map(plain_line)
        .filter(|line| chapter_parts(line.trim()).is_some())
        .map(|line| dedupe_key(&line))
        .collect::<HashSet<_>>()
        .len()
        >= 2;
    let mut nodes: Vec<OutlineNode> = Vec::new();
    let mut seen: HashSet<(String, u32)> = HashSet::new();
    for (page_index, text) in pages.iter().enumerate() {
        if is_toc_page(text) {
            continue;
        }
        let plain: Vec<String> = text.lines().map(plain_line).collect();
        let lines: Vec<&str> = plain.iter().map(String::as_str).collect();
        for (index, line) in lines.iter().enumerate() {
            let candidate = line.trim();
            if candidate.is_empty() || candidate.chars().count() > 120 {
                continue;
            }
            // An instruction or a question ("Perhatikan gambar berikut ini!").
            if candidate.ends_with('!') || candidate.ends_with('?') {
                continue;
            }
            let is_chapter =
                chapter_parts(candidate).is_some() || subtema_parts(candidate).is_some();
            if !is_chapter && furniture.contains(&furniture_key(candidate)) {
                continue;
            }
            if !is_chapter && in_a_list(&lines, index) {
                continue;
            }
            if chaptered && !is_chapter && top_numbered_body(candidate).is_some() {
                continue;
            }
            let (title, level) = heading_of(candidate, &lines, index);
            let Some(title) = title else {
                continue;
            };
            let Some(level) = level else {
                continue;
            };
            // Drop a heading that is really a sentence or a figure caption.
            let words = title.split_whitespace().count();
            if words > 16 || title.ends_with('.') {
                continue;
            }
            let key = (dedupe_key(&title), level);
            if !seen.insert(key) {
                continue;
            }
            nodes.push(OutlineNode {
                title,
                level,
                start_page: u32::try_from(page_index).unwrap_or(u32::MAX),
                end_page: u32::try_from(page_index).unwrap_or(u32::MAX),
            });
        }
    }
    nodes
}

/// A line without the markup an OCR model writes around headings: leading
/// markdown heading marks (`## BAB 2`) and bold or italic wrappers
/// (`**Bab 1 Vektor**`).
fn plain_line(line: &str) -> String {
    let trimmed = line.trim().trim_start_matches('#').trim();
    let unwrapped = trimmed
        .trim_start_matches("**")
        .trim_end_matches("**")
        .trim_start_matches("__")
        .trim_end_matches("__");
    unwrapped.trim().to_string()
}

/// One line of a table of contents.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ContentsEntry {
    title: String,
    level: u32,
    /// The page number printed after the title, when it is an arabic one.
    printed: Option<u32>,
}

/// Split a contents line into its title and its printed page number.
///
/// `Some(None)` is a line that ends in a roman page number (front matter:
/// "Kata Pengantar ... iii"); `None` is a line with no page number at all.
fn split_page_number(line: &str) -> (String, Option<Option<u32>>) {
    let trimmed = line.trim_end();
    let leader = |head: &str| head.ends_with(['.', '…', ' ', '\t']);
    let body_of = |head: &str| head.trim_end_matches(['.', '…', ' ', '\t']).to_string();
    // "Konsep Vektor...3", "Bab 3 DINAMIKA ...63", "Gaya ........ 10".
    let digits = trimmed
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .count();
    if (1..=4).contains(&digits) {
        let (head, number) = trimmed.split_at(trimmed.len() - digits);
        let body = body_of(head);
        if leader(head) && !body.is_empty() {
            return (body, Some(number.parse().ok()));
        }
    }
    // "Kata Pengantar...iii": front matter.
    let roman = trimmed
        .chars()
        .rev()
        .take_while(|c| matches!(c, 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'))
        .count();
    if (1..=6).contains(&roman) {
        let head = &trimmed[..trimmed.len() - roman];
        let body = body_of(head);
        if leader(head) && !body.is_empty() {
            return (body, Some(None));
        }
    }
    (trimmed.to_string(), None)
}

/// The entries of a book's table of contents, in order: chapters (`Bab 1
/// Vektor`, level 1), lettered sections (`A. Konsep Vektor`, level 2) and
/// numbered subsections (`1. Lambang dan Notasi`, level 3). Front matter with
/// roman page numbers and unnumbered extras (`Intisari`, `Refleksi`) are left
/// out. A chapter line without a page number takes the page of the entry
/// after it, which is where its first section starts.
fn contents_entries(pages: &[String]) -> Vec<ContentsEntry> {
    let mut entries: Vec<ContentsEntry> = Vec::new();
    for text in pages.iter().filter(|text| is_toc_page(text)) {
        for raw in text.lines() {
            let line = plain_line(raw);
            if line.is_empty() {
                continue;
            }
            let (body, number) = split_page_number(&line);
            if matches!(number, Some(None)) {
                continue;
            }
            let printed = number.flatten();
            let level = if chapter_parts(&body).is_some() {
                1
            } else if printed.is_none() {
                continue;
            } else if lettered_body(&body).is_some() {
                2
            } else if top_numbered_body(&body).is_some() || dotted_number(&body).is_some() {
                3
            } else {
                continue;
            };
            entries.push(ContentsEntry {
                title: clean_title(&body),
                level,
                printed,
            });
        }
    }
    for index in (0..entries.len()).rev() {
        if entries[index].printed.is_none() {
            entries[index].printed = entries.get(index + 1).and_then(|next| next.printed);
        }
    }
    entries.retain(|entry| entry.printed.is_some());
    entries
}

/// The difference between a PDF page index and the page number printed on
/// it, found by looking each contents entry up among the body's lines: the
/// most common `pdf index − printed page` over the entries that match. A
/// book whose contents never match its body (or match too rarely to agree)
/// has no offset, and the contents outline is not used.
fn contents_offset(pages: &[String], entries: &[ContentsEntry]) -> Option<i64> {
    let mut pages_of: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (page, text) in pages.iter().enumerate() {
        if is_toc_page(text) {
            continue;
        }
        for raw in text.lines() {
            let line = plain_line(raw);
            if line.is_empty() || line.chars().count() > 120 {
                continue;
            }
            pages_of.entry(dedupe_key(&line)).or_default().push(page);
        }
    }
    let mut votes: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut matched = 0usize;
    for entry in entries {
        let Some(printed) = entry.printed else {
            continue;
        };
        let Some(found) = pages_of.get(&dedupe_key(&entry.title)) else {
            continue;
        };
        matched += 1;
        let mut seen = HashSet::new();
        for &page in found {
            let diff = i64::try_from(page).unwrap_or(i64::MAX) - i64::from(printed);
            if (-2..=80).contains(&diff) && seen.insert(diff) {
                *votes.entry(diff).or_default() += 1;
            }
        }
    }
    let (offset, count) = votes
        .into_iter()
        .max_by_key(|&(diff, count)| (count, -diff))?;
    (count >= 3 && count * 3 >= matched).then_some(offset)
}

/// The outline a table of contents gives, its printed pages moved onto PDF
/// pages. Empty when the book has no usable contents (fewer than three
/// entries or two chapters) or the printed pages cannot be placed.
#[must_use]
pub fn contents_nodes(pages: &[String]) -> Vec<OutlineNode> {
    let entries = contents_entries(pages);
    let chapters = entries.iter().filter(|e| e.level == 1).count();
    if entries.len() < 3 || chapters < 2 {
        return Vec::new();
    }
    let Some(offset) = contents_offset(pages, &entries) else {
        return Vec::new();
    };
    let last = i64::try_from(pages.len()).unwrap_or(i64::MAX) - 1;
    let mut seen: HashSet<(String, u32)> = HashSet::new();
    let mut nodes = Vec::new();
    for entry in entries {
        let Some(printed) = entry.printed else {
            continue;
        };
        let page = (i64::from(printed) + offset).clamp(0, last.max(0));
        if !seen.insert((dedupe_key(&entry.title), entry.level)) {
            continue;
        }
        let start = u32::try_from(page).unwrap_or(0);
        nodes.push(OutlineNode {
            title: entry.title,
            level: entry.level,
            start_page: start,
            end_page: start,
        });
    }
    nodes
}

/// A line with its digits removed, lower-cased: the same running header
/// carries a different page number on every page.
fn furniture_key(line: &str) -> String {
    let key: String = dedupe_key(line)
        .chars()
        .filter(|c| !c.is_ascii_digit())
        .collect();
    key.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The lines that recur on three or more pages, by [`furniture_key`].
fn furniture_keys(pages: &[String]) -> HashSet<String> {
    let mut pages_of: std::collections::HashMap<String, HashSet<usize>> =
        std::collections::HashMap::new();
    for (page, text) in pages.iter().enumerate() {
        for line in text.lines() {
            let key = furniture_key(line.trim());
            if key.chars().count() >= 4 {
                pages_of.entry(key).or_default().insert(page);
            }
        }
    }
    pages_of
        .into_iter()
        .filter(|(_, on)| on.len() >= 3)
        .map(|(key, _)| key)
        .collect()
}

/// The kind of list marker a line opens with: a number ("3.") or a capital
/// letter ("B.").
#[derive(PartialEq, Eq)]
enum ListMarker {
    Number,
    Letter,
}

fn list_marker(line: &str) -> Option<ListMarker> {
    let first = line.split_whitespace().next()?;
    let mark = first
        .strip_suffix('.')
        .or_else(|| first.strip_suffix(')'))?;
    if !mark.is_empty() && mark.chars().all(|c| c.is_ascii_digit()) {
        return Some(ListMarker::Number);
    }
    let mut chars = mark.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(ListMarker::Letter),
        _ => None,
    }
}

/// True when the line at `index` is one item of a list: the nearest non-empty
/// line before or after it (two lines at most) opens with the same kind of
/// marker. A section heading is followed by prose; "1. Mobil mainan /
/// 2. Mistar" and "A. 10 N / B. 20 N" are materials and answer options.
fn in_a_list(lines: &[&str], index: usize) -> bool {
    let Some(kind) = list_marker(lines[index].trim()) else {
        return false;
    };
    let near = |range: &mut dyn Iterator<Item = usize>| {
        range
            .map(|i| lines[i].trim())
            .filter(|line| !line.is_empty())
            .take(1)
            .any(|line| list_marker(line).as_ref() == Some(&kind))
    };
    near(&mut (index.saturating_sub(2)..index).rev())
        || near(&mut (index + 1..(index + 3).min(lines.len())))
}

/// The title and level of one candidate line, if it looks like a heading.
fn heading_of(candidate: &str, lines: &[&str], index: usize) -> (Option<String>, Option<u32>) {
    // `Subtema` before the chapter words: it starts with "subtema", not "tema",
    // and it is a section *inside* a tema, not a chapter of its own.
    if let Some(body) = subtema_parts(candidate) {
        let mut title = clean_title(candidate);
        if body.is_empty()
            && let Some(following) = lines.get(index + 1).map(|line| line.trim())
            && !following.is_empty()
            && following.chars().count() < 80
            && following.chars().next().is_some_and(char::is_uppercase)
        {
            title = clean_title(&format!("{title} {following}"));
        }
        return (Some(title), Some(2));
    }
    if let Some(chapter) = chapter_parts(candidate) {
        let mut title = clean_title(candidate);
        // A bare "Chapter 3" often has its title on the next line.
        if chapter.is_empty()
            && let Some(following) = lines.get(index + 1).map(|line| line.trim())
            && !following.is_empty()
            && following.chars().count() < 80
            && following.chars().next().is_some_and(char::is_uppercase)
        {
            title = clean_title(&format!("{title} {following}"));
        }
        return (Some(title), Some(1));
    }
    if let Some(number) = dotted_number(candidate) {
        let level = u32::try_from(number.split('.').count()).unwrap_or(1);
        return (Some(clean_title(candidate)), Some(level));
    }
    if let Some(body) = top_numbered_body(candidate)
        && body.chars().count() < 90
    {
        return (Some(clean_title(candidate)), Some(1));
    }
    // "A. Jenis tumbuhan": a lettered section, the schoolbook form of "4.2".
    if lettered_body(candidate).is_some() {
        return (Some(clean_title(candidate)), Some(2));
    }
    (None, None)
}

/// The text after the chapter word and number, when the line is a chapter
/// heading. One list for both languages: the Indonesian schoolbook words
/// (`BAB`, `Bab`, `Pelajaran`, `Tema`) and the English ones (`Chapter`, `Unit`,
/// `Part`), because a matcher that knows only one family finds no chapters in
/// the other family's books.
///
/// Both the word and its number must be whole tokens — the same `\b` the
/// tree arm's chapter regex requires — or prose would pass for a heading:
/// "Unit costs fell" is not `Unit <c>` any more than "Babi panggang" is
/// `BAB I`.
fn chapter_parts(candidate: &str) -> Option<String> {
    let lower = candidate.to_lowercase();
    let word = CHAPTER_WORDS
        .iter()
        .copied()
        .find(|word| lower.starts_with(word) && boundary_at(&lower, word.len()))?;
    let rest = lower[word.len()..].trim_start();
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if !digits.is_empty() {
        if !boundary_at(rest, digits.len()) {
            return None;
        }
        let after = rest[digits.len()..].trim_start_matches(['.', ':', ' ']);
        return Some(after.to_string());
    }
    // Roman numerals: "Part II", "BAB IV".
    let roman: String = rest
        .chars()
        .take_while(|c| {
            matches!(
                c.to_ascii_lowercase(),
                'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
            )
        })
        .collect();
    if roman.is_empty() || roman.len() > 4 || !boundary_at(rest, roman.len()) {
        return None;
    }
    // The roman numeral is ASCII, so this cut is on a char boundary. The
    // old `candidate[rest.len()..]` sliced the original line at the
    // remainder's length, which lands inside a multi-byte character on
    // lines like "Part II – Análisis" and panicked the ingest task.
    Some(
        rest[roman.len()..]
            .trim_start_matches(['.', ':', ' '])
            .to_string(),
    )
}

/// True when `at` is the end of `text` or the character there separates
/// words: a heading's word and number must be whole tokens.
fn boundary_at(text: &str, at: usize) -> bool {
    !text.is_char_boundary(at) || !text[at..].chars().next().is_some_and(char::is_alphanumeric)
}

/// The words that can open a chapter heading. `Subtema` is deliberately not
/// here — it is a section inside a `Tema`, and [`subtema_parts`] handles it.
const CHAPTER_WORDS: &[&str] = &["pelajaran", "chapter", "tema", "bab", "unit", "part"];

/// The text after "Subtema <n>", when the line is a subtema heading.
///
/// `Subtema` is handled apart from [`chapter_parts`] because it names a
/// section inside a `Tema`, not a chapter: it gets level 2.
fn subtema_parts(candidate: &str) -> Option<String> {
    let lower = candidate.to_lowercase();
    if !lower.starts_with("subtema") || !boundary_at(&lower, "subtema".len()) {
        return None;
    }
    let rest = lower["subtema".len()..].trim_start();
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() || !boundary_at(rest, digits.len()) {
        return None;
    }
    Some(
        rest[digits.len()..]
            .trim_start_matches(['.', ':', ' '])
            .to_string(),
    )
}

/// The body of a lettered section heading ("A. Jenis tumbuhan").
///
/// The schoolbook form of "4.2": a single capital letter, a dot, and a short
/// title on the same line. The guards match [`top_numbered_body`]: the body
/// must be short and carry no sentence-ending dot, so a wrapped sentence
/// starting with "A. " is not taken for a heading.
fn lettered_body(candidate: &str) -> Option<&str> {
    let first = candidate.split_whitespace().next()?;
    let letter = first.strip_suffix('.')?;
    if letter.len() != 1 || !letter.chars().next().is_some_and(char::is_uppercase) {
        return None;
    }
    let rest = candidate[first.len()..].trim();
    let mut chars = rest.chars();
    let first_char = chars.next()?;
    if !first_char.is_uppercase() {
        return None;
    }
    let body: String = rest.chars().take(3).collect();
    if body.chars().count() < 3 || rest.contains('.') {
        return None;
    }
    Some(rest)
}

/// The dotted number at the start of a heading ("4.2", "2.1.1").
fn dotted_number(candidate: &str) -> Option<String> {
    let stripped = candidate
        .strip_prefix("section ")
        .or_else(|| candidate.strip_prefix("sec. "))
        .or_else(|| candidate.strip_prefix("Section "))
        .or_else(|| candidate.strip_prefix("Sec. "))
        .unwrap_or(candidate);
    let first = stripped.split_whitespace().next()?;
    let number = first.trim_end_matches('.');
    let parts: Vec<&str> = number.split('.').collect();
    if parts.len() < 2
        || !parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    // A dotted heading must have a title after the number.
    let rest = stripped[first.len()..].trim();
    if rest.is_empty() {
        return None;
    }
    Some(number.to_string())
}

/// The body of a top-level numbered heading ("3. Labour market developments").
fn top_numbered_body(candidate: &str) -> Option<&str> {
    let first = candidate.split_whitespace().next()?;
    let number = first.trim_end_matches('.');
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = candidate[first.len()..].trim();
    let mut chars = rest.chars();
    let first_char = chars.next()?;
    if !first_char.is_uppercase() {
        return None;
    }
    // At least three characters of body, and no sentence-ending dot after them.
    let body: String = rest.chars().take(3).collect();
    if body.chars().count() < 3 || rest.contains('.') {
        return None;
    }
    Some(rest)
}

fn dedupe_key(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
        .collect::<String>()
        .trim()
        .to_string()
}

/// A table-of-contents line ends in a page number, often after dot leaders.
///
/// `is_toc_page`: enough contents-shaped lines on one page. The heading
/// fallback must skip these pages entirely, or every chapter is "discovered"
/// on the contents page that lists it and the outline reports the contents
/// page as the chapter's first page. Indonesian contents pages say
/// "Daftar Isi", English ones "Contents"; either name is enough when the
/// page also carries contents-shaped lines.
fn is_toc_page(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return false;
    }
    let toc_lines = lines
        .iter()
        .filter(|line| has_trailing_page_number(line))
        .count();
    let top = lines
        .iter()
        .take(4)
        .map(|line| line.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let named = top.contains("contents") || top.contains("daftar isi");
    toc_lines >= 3 && (named || toc_lines * 2 >= lines.len())
}

fn has_trailing_page_number(line: &str) -> bool {
    let trimmed = line.trim_end();
    let digits: String = trimmed
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if digits.is_empty() || digits.len() > 4 {
        return false;
    }
    let raw = &trimmed[..trimmed.len() - digits.len()];
    let before = raw.trim_end();
    // Dot leaders, or a whitespace gap, as a contents line is printed either
    // way: `pdftotext -layout` pads with dots or spaces, and a schoolbook's
    // own text layer often just puts one space before the number.
    before.ends_with("...") || raw.ends_with(' ')
}

/// Fill in end pages: the page before the next node at the same or a higher
/// level starts, clamped to the last page.
///
/// A report whose bookmarks are all flat gives a chapter a one-page span,
/// because the very next bookmark is a subsection on the same page. A chapter's
/// real boundary is the next chapter heading, whatever the bookmark depth
/// claims, so that rule overrides the level rule when the title is a chapter
/// word — "Chapter", but also `BAB`, `Unit`, `Tema`, the schoolbook forms.
#[must_use]
pub fn finish_nodes(mut nodes: Vec<OutlineNode>, pages: u32) -> Vec<OutlineNode> {
    let last_page = pages.saturating_sub(1);
    for index in 0..nodes.len() {
        let mut end = last_page;
        for later in &nodes[index + 1..] {
            if later.level <= nodes[index].level {
                end = later.start_page.saturating_sub(1);
                break;
            }
        }
        nodes[index].end_page = end.max(nodes[index].start_page).min(last_page);
    }
    for index in 0..nodes.len() {
        if !kb_core::tree::is_chapter_node(&nodes[index].title) {
            continue;
        }
        for later in &nodes[index + 1..] {
            if kb_core::tree::is_chapter_node(&later.title)
                && later.start_page > nodes[index].start_page
            {
                nodes[index].end_page = later.start_page.saturating_sub(1);
                break;
            }
        }
    }
    nodes
}

/// Extract a PDF's outline: bookmarks first, then the book's own table of
/// contents, then numbered headings as the fallback.
#[must_use]
pub fn extract_outline(pdfium: &Pdfium, pdf_bytes: &[u8], pages: &[String]) -> Outline {
    let Ok(document) = pdfium.load_pdf_from_byte_slice(pdf_bytes, None) else {
        return Outline::empty(OutlineMethod::None);
    };
    let page_count = u32::try_from(document.pages().len()).unwrap_or(0);
    let bookmarks = bookmark_nodes(&document);
    if !bookmarks.is_empty() {
        return Outline {
            document_title: None,
            method: OutlineMethod::Bookmarks,
            nodes: finish_nodes(bookmarks, page_count),
        };
    }
    let contents = contents_nodes(pages);
    if !contents.is_empty() {
        return Outline {
            document_title: None,
            method: OutlineMethod::Contents,
            nodes: finish_nodes(contents, page_count),
        };
    }
    let headings = heading_nodes(pages);
    if headings.is_empty() {
        Outline::empty(OutlineMethod::None)
    } else {
        Outline {
            document_title: None,
            method: OutlineMethod::Headings,
            nodes: finish_nodes(headings, page_count),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_roman_part_heading_with_accents_does_not_panic() {
        for line in [
            "Part II – Análisis económico",
            "Part IV: Información",
            "BAB IV Kewirausahaan",
        ] {
            assert!(chapter_parts(line).is_some(), "{line}");
        }
        assert_eq!(
            chapter_parts("Part II – Análisis").as_deref(),
            Some("– análisis")
        );
        // The number must be a whole token, as the tree arm's chapter regex
        // requires: "CIÓN" is not a roman numeral, and the old matcher read
        // "CI" as one. This used to pass for the wrong reason; rejecting it
        // is the same no-panic answer the test was written for.
        assert!(chapter_parts("Part CIÓN de la población").is_none());
        // Whole tokens both sides of the number: prose never passes.
        assert!(
            chapter_parts("Babi panggang enak sekali").is_none(),
            "babi is not BAB I"
        );
        assert!(chapter_parts("Unit costs fell in the quarter").is_none());
        assert!(chapter_parts("Tema utama dari laporan ini").is_none());
    }
    use crate::pdfium::get_pdfium;

    /// Build a minimal, valid PDF from object bodies. Offsets are computed, so
    /// the xref is correct without hand-counting bytes.
    fn pdf_of(objects: &[&str]) -> Vec<u8> {
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", index + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    /// Three blank pages and an outline: Chapter 1 (page 0) with 1.1 (page 1),
    /// then Chapter 2 (page 2).
    fn bookmarked_pdf() -> Vec<u8> {
        pdf_of(&[
            "<< /Type /Catalog /Pages 2 0 R /Outlines 6 0 R >>",
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            "<< /Type /Outlines /First 7 0 R /Last 9 0 R /Count 2 >>",
            "<< /Title (Chapter 1 Introduction) /Parent 6 0 R /Dest [3 0 R /Fit] /First 8 0 R /Last 8 0 R /Count 1 /Next 9 0 R >>",
            "<< /Title (1.1 Background) /Parent 7 0 R /Dest [4 0 R /Fit] >>",
            "<< /Title (Chapter 2 Findings (3F)) /Parent 6 0 R /Dest [5 0 R /Fit] >>",
        ])
    }

    #[test]
    fn bookmarks_become_nodes_with_zero_based_pages_and_end_pages() {
        let Ok(pdfium) = get_pdfium() else {
            eprintln!("Skipping bookmarks test: libpdfium not found");
            return;
        };
        let outline = extract_outline(pdfium, &bookmarked_pdf(), &[]);
        assert_eq!(outline.method, OutlineMethod::Bookmarks, "{outline:?}");
        let titles: Vec<&str> = outline.nodes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Chapter 1 Introduction",
                "1.1 Background",
                "Chapter 2 Findings"
            ]
        );
        let pages: Vec<(u32, u32)> = outline
            .nodes
            .iter()
            .map(|n| (n.start_page, n.end_page))
            .collect();
        assert_eq!(pages, vec![(0, 1), (1, 1), (2, 2)]);
        assert_eq!(outline.nodes[1].level, 2);
    }

    #[test]
    fn a_pdf_without_bookmarks_has_none_rather_than_an_empty_bookmark_outline() {
        let Ok(pdfium) = get_pdfium() else {
            eprintln!("Skipping test: libpdfium not found");
            return;
        };
        let outline = extract_outline(
            pdfium,
            crate::pdfium::MINIMAL_PDF_HELLO_WORLD,
            &["Hello World".to_string()],
        );
        assert_eq!(outline.method, OutlineMethod::None);
        assert!(outline.is_empty());
    }

    #[test]
    fn heading_fallback_finds_numbered_headings_and_skips_the_contents_page() {
        let pages = vec![
            "Contents\n1. Introduction .......... 1\n2. Methods .......... 2\n3. Results .......... 3"
                .to_string(),
            "1. Introduction\nSome body text that is not a heading.".to_string(),
            "2. Methods\n2.1 Sampling\nWe drew a sample.".to_string(),
            "Chapter 3 Results\nEverything worked.".to_string(),
        ];
        let nodes = heading_nodes(&pages);
        let titles: Vec<&str> = nodes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "1. Introduction",
                "2. Methods",
                "2.1 Sampling",
                "Chapter 3 Results"
            ]
        );
        assert_eq!(nodes[2].level, 2);
        assert_eq!(nodes[1].start_page, 2);

        let finished = finish_nodes(nodes, 4);
        // "2. Methods" is closed by the next level-1 heading on page 3.
        assert_eq!(finished[1].end_page, 2);
        assert_eq!(finished[3].end_page, 3);
    }

    #[test]
    fn ocr_noise_in_a_chaptered_schoolbook_does_not_become_chapters() {
        // What full-page OCR of a BSE IPA book gives: a running header with the
        // page number, a materials list, numbered steps, answer options and an
        // instruction line, around two real chapters and a lettered section.
        let mut pages =
            vec!["Bab 1 Gerak Benda\nPada bab ini kamu belajar tentang gerak.".to_string()];
        for n in 2..6 {
            let section = if n == 2 {
                "A. Gerak Lurus\nBenda bergerak lurus jika lintasannya lurus.\n"
            } else {
                "Benda yang bergerak memiliki kecepatan.\n"
            };
            pages.push(format!(
                "{n} Kelas VIII SMP/MTs Semester 1\n\
                 {section}\
                 1. Mobil mainan\n2. Mistar\n3. Ticker timer\n\
                 Perhatikan gambar berikut ini!\n\
                 A. 10 N\nB. 20 N\nC. 30 N"
            ));
        }
        pages.push("Bab 2 Gaya\nGaya adalah tarikan atau dorongan.".to_string());
        let nodes = heading_nodes(&pages);
        let titles: Vec<&str> = nodes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Bab 1 Gerak Benda", "A. Gerak Lurus", "Bab 2 Gaya"]
        );
        assert_eq!(nodes[0].level, 1);
        assert_eq!(nodes[1].level, 2);
        assert_eq!(nodes[2].level, 1);
    }

    #[test]
    fn a_schoolbook_outline_comes_from_its_contents_page() {
        // An OCR'd BSE book: markdown headings, roman front matter, chapters
        // whose own pages are pictures, and printed page 1 on PDF page 4.
        let mut pages: Vec<String> = vec![String::new(); 4];
        pages[1] = "## Daftar Isi\n\
                    Kata Pengantar ... iii\n\
                    ## Bab 1 Gerak Benda\n\
                    A. Konsep Gerak ... 2\n\
                    1. Gerak Lurus ... 3\n\
                    Uji Kompetensi ... 5\n\
                    Bab 2 Gaya ... 6\n\
                    A. Jenis Gaya ... 7\n\
                    1. Gaya Gesek ... 8"
            .to_string();
        for printed in 1..=9 {
            let body = match printed {
                2 => "## A. Konsep Gerak\nBenda bergerak.",
                3 => "1. Gerak Lurus\nLintasannya lurus.",
                7 => "## A. Jenis Gaya\nAda banyak gaya.",
                8 => "## 1. Gaya Gesek\nGesekan memperlambat.",
                _ => "Isi buku.",
            };
            pages.push(format!("{printed} Ilmu Pengetahuan Alam\n{body}"));
        }
        let nodes = contents_nodes(&pages);
        let got: Vec<(&str, u32, u32)> = nodes
            .iter()
            .map(|n| (n.title.as_str(), n.level, n.start_page))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Bab 1 Gerak Benda", 1, 5),
                ("A. Konsep Gerak", 2, 5),
                ("1. Gerak Lurus", 3, 6),
                ("Bab 2 Gaya", 1, 9),
                ("A. Jenis Gaya", 2, 10),
                ("1. Gaya Gesek", 3, 11),
            ]
        );
    }

    #[test]
    fn a_contents_line_gives_its_page_with_or_without_a_space_before_the_leader() {
        assert_eq!(
            split_page_number("A. Konsep Vektor...3"),
            ("A. Konsep Vektor".to_string(), Some(Some(3)))
        );
        assert_eq!(
            split_page_number("Bab 3 DINAMIKA GERAK PARTIKEL ...63"),
            ("Bab 3 DINAMIKA GERAK PARTIKEL".to_string(), Some(Some(63)))
        );
        assert_eq!(
            split_page_number("2. Gaya ........................ 10"),
            ("2. Gaya".to_string(), Some(Some(10)))
        );
        assert_eq!(
            split_page_number("Kata Pengantar...iii"),
            ("Kata Pengantar".to_string(), Some(None))
        );
        assert_eq!(split_page_number("## Bab 1 VEKTOR").1, None);
        assert_eq!(split_page_number("Bab 2 Kimia").1, None);
    }

    #[test]
    fn markdown_heading_marks_do_not_hide_a_chapter() {
        let pages = vec!["## BAB 2\nKINEMATIKA\nGerak benda.".to_string()];
        let nodes = heading_nodes(&pages);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].title, "BAB 2 KINEMATIKA");
    }

    #[test]
    fn a_numbered_list_item_is_not_a_heading_even_without_chapters() {
        let pages = vec!["Alat dan bahan\n1. Mobil mainan\n2. Mistar\nLangkah kerja".to_string()];
        assert!(heading_nodes(&pages).is_empty());
    }

    #[test]
    fn a_repeated_heading_is_kept_once_at_the_higher_level() {
        let pages = vec![
            "4.2 Leave entitlement\ntext".to_string(),
            "4.2 Leave entitlement\ntext again".to_string(),
        ];
        let nodes = heading_nodes(&pages);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].start_page, 0);
    }

    #[test]
    fn a_sentence_with_a_leading_number_is_not_a_heading() {
        let pages = vec!["3. This is a sentence that ends with a full stop.".to_string()];
        assert!(heading_nodes(&pages).is_empty());
    }

    #[test]
    fn a_bare_chapter_line_takes_its_title_from_the_next_line() {
        let pages = vec!["Chapter 4\nSocial policy\nBody".to_string()];
        let nodes = heading_nodes(&pages);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].title, "Chapter 4 Social policy");
        assert_eq!(nodes[0].level, 1);
    }

    #[test]
    fn indonesian_chapter_headings_are_found_at_both_numberings() {
        // Roman, the schoolbook form: BAB IV with its title on the next line.
        let pages = vec!["BAB IV\nKEWIRAUSAHAAN\nEkonomi adalah…".to_string()];
        let nodes = heading_nodes(&pages);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].title, "BAB IV KEWIRAUSAHAAN");
        assert_eq!(nodes[0].level, 1);
        assert_eq!(nodes[0].start_page, 0);

        // Digits, with the title on the same line.
        let pages = vec!["Bab 2 Keragaman Makhluk Hidup\nKeanekaragaman…".to_string()];
        let nodes = heading_nodes(&pages);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].title, "Bab 2 Keragaman Makhluk Hidup");

        // The rest of the curriculum vocabulary, both languages.
        for line in [
            "Pelajaran 5 Peristiwa Sekitarku",
            "Tema 2 Bermain di Lingkunganku",
            "Chapter 1 My Family",
            "Unit 3 Things Around Me",
        ] {
            let nodes = heading_nodes(&[format!("{line}\nbody").to_string()]);
            assert_eq!(nodes.len(), 1, "{line} should be one chapter heading");
            assert_eq!(nodes[0].level, 1, "{line}");
        }
    }

    #[test]
    fn subtema_and_lettered_lines_are_sections_not_chapters() {
        let pages = vec![
            "Tema 3 Kurang Lebih\nbody".to_string(),
            "Subtema 1 Kebutuhanku\nbody".to_string(),
            "A. Jenis tumbuhan di sekitar kita\nbody".to_string(),
            "B. Cara merawat tanaman\nbody".to_string(),
        ];
        let nodes = heading_nodes(&pages);
        let titles: Vec<&str> = nodes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Tema 3 Kurang Lebih",
                "Subtema 1 Kebutuhanku",
                "A. Jenis tumbuhan di sekitar kita",
                "B. Cara merawat tanaman"
            ]
        );
        assert_eq!(nodes[0].level, 1);
        assert!(nodes[1..].iter().all(|n| n.level == 2));
    }

    #[test]
    fn a_sentence_starting_with_a_letter_and_dot_is_not_a_heading() {
        let pages = vec![
            // Too many words and it ends with a dot: prose.
            "A. Listen to your teacher and repeat after the recording, then answer.".to_string(),
        ];
        assert!(heading_nodes(&pages).is_empty());
    }

    #[test]
    fn an_indonesian_contents_page_is_ignored_not_indexed() {
        // The contents page lists every chapter with a page number; the
        // chapters themselves start later. The outline must report the page
        // each chapter starts on, never the contents page.
        let toc = "Daftar Isi\n\
                   BAB I Kewirausahaan .......... 5\n\
                   BAB II Pasar dan Uang .......... 25\n\
                   BAB III Dana dan Pinjaman .......... 45";
        let pages = vec![
            toc.to_string(),
            "Pengantar\nBuku ini membahas ekonomi.".to_string(),
            "BAB I\nKEWIRAUSAHAAN\nWirausaha adalah orang berjiwa…".to_string(),
            "BAB II\nPASAR DAN UANG\nPasar adalah tempat bertemunya…".to_string(),
        ];
        let nodes = heading_nodes(&pages);
        let titles: Vec<&str> = nodes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["BAB I KEWIRAUSAHAAN", "BAB II PASAR DAN UANG"],
            "the contents page produced no headings of its own"
        );
        assert_eq!(
            nodes[0].start_page, 2,
            "chapter I starts on page 2, not on the contents page"
        );
        assert_eq!(nodes[1].start_page, 3);

        let finished = finish_nodes(nodes, 4);
        assert_eq!(
            finished[0].end_page, 2,
            "chapter I closes where chapter II starts"
        );
        assert_eq!(finished[1].end_page, 3);
    }

    #[test]
    fn repeated_activity_templates_do_not_become_headings() {
        // The Kelas 7 English book repeats this activity shape on almost
        // every page. The chapter headings are what the outline should find.
        let mut pages: Vec<String> =
            vec!["Unit 2 Things Around Me\nHere are what we will do.".to_string()];
        for i in 0..3 {
            pages.push(format!(
                "Bagas BENI Here are what we will do. First, we will read the text. \
                 Second, we will discuss {i}.\nThis is page {}.",
                i + 1
            ));
        }
        pages.push("Unit 3 My Family\nHere are what we will do.".to_string());
        let nodes = heading_nodes(&pages);
        let titles: Vec<&str> = nodes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, vec!["Unit 2 Things Around Me", "Unit 3 My Family"]);
    }

    #[test]
    fn a_flat_outline_still_closes_a_bab_at_the_next_bab() {
        // Every bookmark flat at level 1: the level rule would close BAB I on
        // its own page, because the very next bookmark is a section. The
        // chapter-word rule overrides that — a BAB ends where the next BAB
        // starts, and "A." is not a chapter word.
        let nodes = vec![
            OutlineNode {
                title: "BAB I Kewirausahaan".into(),
                level: 1,
                start_page: 3,
                end_page: 3,
            },
            OutlineNode {
                title: "A. Pengertian".into(),
                level: 1,
                start_page: 3,
                end_page: 3,
            },
            OutlineNode {
                title: "BAB II Pasar".into(),
                level: 1,
                start_page: 20,
                end_page: 20,
            },
        ];
        let finished = finish_nodes(nodes, 30);
        assert_eq!(
            finished[0].end_page, 19,
            "BAB I runs to the page before BAB II, not to its own first page"
        );
        assert_eq!(
            finished[1].end_page, 19,
            "the level rule still governs the section"
        );
        assert_eq!(finished[2].end_page, 29);
    }

    #[test]
    fn clean_title_collapses_space_and_drops_a_footnote_marker() {
        assert_eq!(
            clean_title("  Chapter   2   Findings (3F) "),
            "Chapter 2 Findings"
        );
        // Same rule as the prototype: a trailing parenthesised number is a footnote
        // marker, even when it reads like a year.
        assert_eq!(clean_title("4.2 Leave (2024)"), "4.2 Leave");
    }

    #[test]
    fn a_flat_outline_still_closes_a_chapter_at_the_next_chapter() {
        let nodes = vec![
            OutlineNode {
                title: "Chapter 1 Introduction".into(),
                level: 1,
                start_page: 0,
                end_page: 0,
            },
            OutlineNode {
                title: "1.1 Background".into(),
                level: 2,
                start_page: 0,
                end_page: 0,
            },
            OutlineNode {
                title: "Chapter 2 Findings".into(),
                level: 1,
                start_page: 5,
                end_page: 5,
            },
        ];
        let finished = finish_nodes(nodes, 10);
        assert_eq!(finished[0].end_page, 4);
        assert_eq!(finished[2].end_page, 9);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod bench {
    use super::*;
    use crate::pdfium::get_pdfium;
    use std::time::Instant;

    fn pdf_of(objects: &[String]) -> Vec<u8> {
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", index + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    #[test]
    #[ignore = "manual outline-cost measurement, not a gate"]
    fn measure_outline_cost_on_a_2000_page_pdf() {
        let Ok(pdfium) = get_pdfium() else {
            return;
        };
        let pages = 2000usize;
        let mut objects: Vec<String> = Vec::new();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", i + 3)).collect();
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ));
        for _ in 0..pages {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {} 0 R >> >> /Contents {} 0 R >>",
                pages + 3,
                pages + 4
            ));
        }
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
        let body = "BT /F1 12 Tf 72 720 Td (2.1 Labour market developments) Tj ET";
        objects.push(format!(
            "<< /Length {} >>\nstream\n{body}\nendstream",
            body.len()
        ));
        let bytes = pdf_of(&objects);
        let started = Instant::now();
        let result = crate::pdfium::extract_pdf_text_layer(pdfium, &bytes).unwrap();
        let total = started.elapsed();
        let page_texts: Vec<String> = result
            .page_map
            .as_ref()
            .unwrap()
            .iter()
            .map(|p| p.text.clone())
            .collect();
        let started = Instant::now();
        let outline = extract_outline(pdfium, &bytes, &page_texts);
        let added = started.elapsed();
        println!(
            "pages={pages} text+outline={total:?} outline_alone={added:?} method={:?} nodes={}",
            outline.method,
            outline.nodes.len()
        );
    }
}
