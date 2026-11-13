//! Pictures inside OOXML, OpenDocument and EPUB packages.

use std::collections::HashMap;
use std::io::Read;

use quick_xml::events::{BytesStart, Event};

use super::RawVisual;
use crate::epub;
use crate::xml;
use crate::zip_read::{self, Archive};

/// Every picture of a package, in document order. A package that cannot be
/// opened has no pictures here: the text extractor reports that failure.
pub(crate) fn pictures(ext: &str, bytes: &[u8]) -> Vec<RawVisual> {
    let Ok(mut archive) = zip_read::open(bytes) else {
        return Vec::new();
    };
    match ext {
        ".docx" => docx(&mut archive),
        ".pptx" => pptx(&mut archive),
        ".xlsx" => xlsx(&mut archive),
        ".odt" | ".ods" => odf(&mut archive),
        ".epub" => epub_pictures(&mut archive),
        _ => Vec::new(),
    }
}

/// One entry's bytes, or `None` when it is missing or unreadable.
fn read_bytes(archive: &mut Archive<'_>, name: &str) -> Option<Vec<u8>> {
    let mut entry = archive.by_name(name).ok()?;
    let mut out = Vec::new();
    entry.read_to_end(&mut out).ok()?;
    Some(out)
}

/// An attribute by local name, whatever its prefix (`r:embed`, `R:embed`).
fn attr_local(element: &BytesStart<'_>, local: &str) -> Option<String> {
    for attribute in element.attributes().flatten() {
        if attribute.key.local_name().as_ref() == local.as_bytes() {
            if let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Explicit1_0) {
                return Some(value.into_owned());
            }
        }
    }
    None
}

/// The directory of a part (`word/document.xml` → `word`).
fn dir_of(part: &str) -> &str {
    part.rfind('/').map_or("", |i| &part[..i])
}

/// The `_rels` file of a part (`ppt/slides/slide1.xml` →
/// `ppt/slides/_rels/slide1.xml.rels`).
fn rels_path(part: &str) -> String {
    match part.rfind('/') {
        Some(i) => format!("{}/_rels/{}.rels", &part[..i], &part[i + 1..]),
        None => format!("_rels/{part}.rels"),
    }
}

/// Relationship id → (archive path of the target, relationship type), for the
/// internal targets of `part`.
fn relationships(archive: &mut Archive<'_>, part: &str) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let Ok(text) = zip_read::read_text(archive, &rels_path(part)) else {
        return out;
    };
    let base = dir_of(part);
    let mut reader = xml::reader_from_str(&text);
    let mut buffer = Vec::new();
    while let Ok(Some(event)) = xml::next_event(&mut reader, &mut buffer) {
        if let Event::Start(e) | Event::Empty(e) = event {
            if e.local_name().as_ref() != b"Relationship" {
                continue;
            }
            if attr_local(&e, "TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External")) {
                continue;
            }
            if let (Some(id), Some(target)) = (attr_local(&e, "Id"), attr_local(&e, "Target")) {
                let kind = attr_local(&e, "Type").unwrap_or_default();
                out.insert(id, (epub::resolve(base, &target), kind));
            }
        }
    }
    out
}

/// Picture references in one DrawingML part: `(relationship id, alt text)` in
/// order. The alt text is the `descr` (else `title`) of the nearest preceding
/// non-visual properties element (`wp:docPr`, `p:cNvPr`, `xdr:cNvPr`), which
/// is where every Office app writes it.
fn drawing_refs(xml_text: &str) -> Vec<(String, Option<String>)> {
    let mut reader = xml::reader_from_str(xml_text);
    let mut buffer = Vec::new();
    let mut alt: Option<String> = None;
    let mut out = Vec::new();
    while let Ok(Some(event)) = xml::next_event(&mut reader, &mut buffer) {
        let (Event::Start(e) | Event::Empty(e)) = event else {
            continue;
        };
        match e.local_name().as_ref() {
            b"docPr" | b"cNvPr" => {
                alt = attr_local(&e, "descr")
                    .filter(|d| !d.trim().is_empty())
                    .or_else(|| attr_local(&e, "title").filter(|t| !t.trim().is_empty()));
            }
            b"blip" => {
                if let Some(id) = attr_local(&e, "embed") {
                    out.push((id, alt.clone()));
                }
            }
            // Legacy VML pictures (`v:imagedata r:id`, alt in `o:title`).
            b"imagedata" => {
                let id = e.attributes().flatten().find_map(|a| {
                    let key = a.key.as_ref();
                    (a.key.local_name().as_ref() == b"id" && key.contains(&b':'))
                        .then(|| String::from_utf8_lossy(&a.value).into_owned())
                });
                if let Some(id) = id {
                    out.push((id, attr_local(&e, "title").or_else(|| alt.clone())));
                }
            }
            _ => {}
        }
    }
    out
}

/// The pictures one DrawingML part references, read through its relationships.
fn part_pictures(archive: &mut Archive<'_>, part: &str, page: u32) -> Vec<RawVisual> {
    let Ok(text) = zip_read::read_text(archive, part) else {
        return Vec::new();
    };
    let rels = relationships(archive, part);
    let mut out = Vec::new();
    for (id, alt) in drawing_refs(&text) {
        let Some((target, _)) = rels.get(&id) else {
            continue;
        };
        if let Some(bytes) = read_bytes(archive, target) {
            out.push(RawVisual {
                page,
                bytes,
                decoded: None,
                media_name: Some(target.clone()),
                alt_text: alt,
                bbox: None,
            });
        }
    }
    out
}

fn docx(archive: &mut Archive<'_>) -> Vec<RawVisual> {
    part_pictures(archive, "word/document.xml", 0)
}

/// Parts named `<prefix><n>.xml`, in numeric order of `n`.
fn numbered_parts(archive: &Archive<'_>, prefix: &str) -> Vec<(u32, String)> {
    let mut parts: Vec<(u32, String)> = zip_read::names(archive)
        .into_iter()
        .filter_map(|name| {
            let n = name
                .strip_prefix(prefix)?
                .strip_suffix(".xml")?
                .parse()
                .ok()?;
            Some((n, name))
        })
        .collect();
    parts.sort();
    parts
}

fn pptx(archive: &mut Archive<'_>) -> Vec<RawVisual> {
    let mut out = Vec::new();
    for (n, part) in numbered_parts(archive, "ppt/slides/slide") {
        out.extend(part_pictures(archive, &part, n.saturating_sub(1)));
    }
    out
}

/// Sheets in workbook order, each with the drawing part it references.
fn xlsx(archive: &mut Archive<'_>) -> Vec<RawVisual> {
    let Ok(workbook) = zip_read::read_text(archive, "xl/workbook.xml") else {
        return Vec::new();
    };
    let workbook_rels = relationships(archive, "xl/workbook.xml");
    let mut sheet_ids = Vec::new();
    let mut reader = xml::reader_from_str(&workbook);
    let mut buffer = Vec::new();
    while let Ok(Some(event)) = xml::next_event(&mut reader, &mut buffer) {
        if let Event::Start(e) | Event::Empty(e) = event {
            if e.local_name().as_ref() == b"sheet" {
                if let Some(id) = attr_local(&e, "id") {
                    sheet_ids.push(id);
                }
            }
        }
    }
    let mut out = Vec::new();
    for (index, id) in sheet_ids.iter().enumerate() {
        let Some((sheet, _)) = workbook_rels.get(id) else {
            continue;
        };
        let sheet = sheet.clone();
        let drawings: Vec<String> = relationships(archive, &sheet)
            .into_values()
            .filter(|(_, kind)| kind.ends_with("/drawing"))
            .map(|(target, _)| target)
            .collect();
        for drawing in drawings {
            #[allow(clippy::cast_possible_truncation)]
            out.extend(part_pictures(archive, &drawing, index as u32));
        }
    }
    out
}

/// OpenDocument: `draw:frame` elements holding a `draw:image`; the frame's
/// `svg:title` and `svg:desc` are its alt text. In a spreadsheet the page is
/// the index of the `table:table` the frame sits in.
fn odf(archive: &mut Archive<'_>) -> Vec<RawVisual> {
    let Ok(content) = zip_read::read_text(archive, "content.xml") else {
        return Vec::new();
    };
    struct Frame {
        images: Vec<String>,
        title: String,
        desc: String,
        page: u32,
    }
    let mut reader = xml::reader_from_str(&content);
    let mut buffer = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut done: Vec<Frame> = Vec::new();
    let mut table_index: Option<u32> = None;
    let mut in_text: Option<&'static str> = None;
    while let Ok(Some(event)) = xml::next_event(&mut reader, &mut buffer) {
        match event {
            Event::Start(e) => match e.local_name().as_ref() {
                b"table" if e.name().as_ref().starts_with(b"table:") => {
                    table_index = Some(table_index.map_or(0, |i| i + 1));
                }
                b"frame" => frames.push(Frame {
                    images: Vec::new(),
                    title: String::new(),
                    desc: String::new(),
                    page: table_index.unwrap_or(0),
                }),
                b"image" => {
                    if let (Some(frame), Some(href)) = (frames.last_mut(), attr_local(&e, "href")) {
                        frame.images.push(href);
                    }
                }
                b"title" => in_text = Some("title"),
                b"desc" => in_text = Some("desc"),
                _ => {}
            },
            Event::Empty(e) => {
                if e.local_name().as_ref() == b"image" {
                    if let (Some(frame), Some(href)) = (frames.last_mut(), attr_local(&e, "href")) {
                        frame.images.push(href);
                    }
                }
            }
            Event::Text(t) => {
                if let (Some(which), Some(frame)) = (in_text, frames.last_mut()) {
                    let text = xml::text_of(&t);
                    match which {
                        "title" => frame.title.push_str(&text),
                        _ => frame.desc.push_str(&text),
                    }
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"frame" => {
                    if let Some(frame) = frames.pop() {
                        done.push(frame);
                    }
                }
                b"title" | b"desc" => in_text = None,
                _ => {}
            },
            _ => {}
        }
    }
    let mut out = Vec::new();
    for frame in done {
        let alt = match (frame.title.trim(), frame.desc.trim()) {
            ("", "") => None,
            (t, "") => Some(t.to_string()),
            ("", d) => Some(d.to_string()),
            (t, d) if t == d => Some(t.to_string()),
            (t, d) => Some(format!("{t}. {d}")),
        };
        // A frame may hold the same picture twice (a preview); read it once.
        let mut seen = Vec::new();
        for href in frame.images {
            if href.contains("://") || seen.contains(&href) {
                continue;
            }
            let path = epub::resolve("", href.trim_start_matches("./"));
            if let Some(bytes) = read_bytes(archive, &path) {
                out.push(RawVisual {
                    page: frame.page,
                    bytes,
                    decoded: None,
                    media_name: Some(path),
                    alt_text: alt.clone(),
                    bbox: None,
                });
            }
            seen.push(href);
        }
    }
    out
}

/// EPUB: `<img src alt>` (and SVG `<image href>`) in each spine document, in
/// spine order. Read with a pattern rather than an XML parser: real XHTML is
/// often not well-formed, and one bad chapter must not hide the others.
fn epub_pictures(archive: &mut Archive<'_>) -> Vec<RawVisual> {
    let Ok(container) = zip_read::read_text(archive, "META-INF/container.xml") else {
        return Vec::new();
    };
    let Some(opf_path) = epub::rootfile_path(&container) else {
        return Vec::new();
    };
    let Ok(opf) = zip_read::read_text(archive, &opf_path) else {
        return Vec::new();
    };
    let base = dir_of(&opf_path).to_string();
    let Ok(tag) = fancy_regex::Regex::new(r"(?is)<(?:img|image)\b[^>]*>") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for href in epub::spine_hrefs(&opf) {
        let chapter = epub::resolve(&base, &href);
        let Ok(xhtml) = zip_read::read_text(archive, &chapter) else {
            continue;
        };
        let chapter_dir = dir_of(&chapter).to_string();
        for found in tag.find_iter(&xhtml).flatten() {
            let element = found.as_str();
            let Some(src) = html_attr(element, "src").or_else(|| html_attr(element, "href")) else {
                continue;
            };
            if src.contains("://") || src.starts_with("data:") {
                continue;
            }
            let path = epub::resolve(&chapter_dir, &src);
            if let Some(bytes) = read_bytes(archive, &path) {
                out.push(RawVisual {
                    page: 0,
                    bytes,
                    decoded: None,
                    media_name: Some(path),
                    alt_text: html_attr(element, "alt").map(|a| {
                        quick_xml::escape::unescape(&a).map_or(a.clone(), |c| c.into_owned())
                    }),
                    bbox: None,
                });
            }
        }
    }
    out
}

/// An attribute of one HTML start tag, by name (`src`, `xlink:href`, `alt`).
fn html_attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!(r#"(?is)[\s:]{name}\s*=\s*(?:"([^"]*)"|'([^']*)')"#);
    let re = fancy_regex::Regex::new(&pattern).ok()?;
    let caps = re.captures(tag).ok()??;
    caps.get(1)
        .or_else(|| caps.get(2))
        .map(|m| m.as_str().to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn rels_paths_and_targets_resolve_like_office_does() {
        assert_eq!(
            rels_path("word/document.xml"),
            "word/_rels/document.xml.rels"
        );
        assert_eq!(
            rels_path("ppt/slides/slide3.xml"),
            "ppt/slides/_rels/slide3.xml.rels"
        );
        assert_eq!(
            epub::resolve("ppt/slides", "../media/image1.png"),
            "ppt/media/image1.png"
        );
    }

    #[test]
    fn alt_text_comes_from_the_nearest_non_visual_properties() {
        let xml = r#"<w:document xmlns:w="w" xmlns:wp="wp" xmlns:a="a" xmlns:r="r" xmlns:v="v" xmlns:o="o">
            <wp:docPr id="1" name="Picture 1" descr="A red bicycle"/>
            <a:blip r:embed="rId5"/>
            <wp:docPr id="2" name="Picture 2" title="Only a title"/>
            <a:blip r:embed="rId6"/>
            <wp:docPr id="3" name="Picture 3"/>
            <a:blip r:embed="rId7"/>
            <v:imagedata r:id="rId8" o:title="Old VML picture"/>
        </w:document>"#;
        assert_eq!(
            drawing_refs(xml),
            vec![
                ("rId5".to_string(), Some("A red bicycle".to_string())),
                ("rId6".to_string(), Some("Only a title".to_string())),
                ("rId7".to_string(), None),
                ("rId8".to_string(), Some("Old VML picture".to_string())),
            ]
        );
    }

    #[test]
    fn html_attributes_take_either_quote() {
        let tag = r#"<img class="x" src='../images/a b.png' alt="Bridge at dusk"/>"#;
        assert_eq!(html_attr(tag, "src").as_deref(), Some("../images/a b.png"));
        assert_eq!(html_attr(tag, "alt").as_deref(), Some("Bridge at dusk"));
        assert_eq!(
            html_attr(r#"<image xlink:href="c.jpg"/>"#, "href").as_deref(),
            Some("c.jpg")
        );
        assert_eq!(html_attr(r#"<img data-src="x.png">"#, "src"), None);
    }
}
