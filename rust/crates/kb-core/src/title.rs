//! A document's title, and the text a chunk is embedded as.
//!
//! Two decisions live here, both pure:
//!
//! * **Which title a document has.** An explicit title wins. A markdown or
//!   text document whose first non-empty line is an H1 lends that H1 to the
//!   title: `notes.md` that starts with "# Deployment" is about Deployment,
//!   and embedding the filename stem in front of every chunk told the
//!   retriever nothing at all. Otherwise the filename stem, as before.
//! * **Which text a chunk is embedded as.** `"{title}\n\n{content}"`, with the
//!   chunk's heading path after the title when the chunker found one, so a
//!   chunk from the middle of a document still says where it came from.
//!
//! Both matter for the same reason: the embedding is the only thing the
//! dense arm ever sees of a chunk, and a chunk that arrives with no idea what
//! document it belongs to is a chunk that cannot be found by a query about
//! that document. Measured on `miracl-id-200`
//! (`docs/kf-vplane-eval.md`), putting the real title in front of the
//! content was worth about 0.008 nDCG@10 and 0.025 Hit@1 on top of a fixed
//! fusion.

/// The filename without its extension: `report.pdf` → `report`,
/// `archive.tar.gz` → `archive.tar`, `noextension` → `noextension`.
///
/// A leading dot is not an extension (`.gitignore`), so `.gitignore` keeps
/// its name.
#[must_use]
pub fn filename_stem(filename: &str) -> String {
    match filename.rfind('.') {
        Some(0) | None => filename.to_string(),
        Some(index) => filename[..index].to_string(),
    }
}

/// The document's first heading, when its first non-empty line is an H1.
///
/// `# Deployment` → `Deployment`. A line that is not a heading is not a
/// title, and neither is an H2 or deeper: the title of a document is the one
/// heading that names the document, and a subsection does not.
#[must_use]
pub fn first_heading(content: &str) -> Option<String> {
    let first = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    // Exactly one hash and a space: `## Section` is a subsection, not a title.
    let text = first.strip_prefix("# ")?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// The title of a document.
///
/// * An explicit title wins: the uploader said what it is.
/// * Otherwise, a markdown or text document's own H1: the file says what it
///   is, in its first line.
/// * Otherwise the filename stem, as before.
#[must_use]
pub fn document_title(
    explicit: Option<&str>,
    filename: &str,
    content: &str,
    markdown: bool,
) -> String {
    if let Some(title) = explicit.map(str::trim).filter(|t| !t.is_empty()) {
        return title.to_string();
    }
    if markdown {
        if let Some(heading) = first_heading(content) {
            return heading;
        }
    }
    filename_stem(filename)
}

/// The heading line for a chunk: the title, then the chunk's heading path.
///
/// The path is the chunker's hierarchy metadata — the headings above the
/// chunk, outermost first, joined with " > ". When the path already starts
/// with the title (the common case: the document's own H1 is the outermost
/// heading) the title is not repeated, so the line reads
/// "Judul > Bagian" and not "Judul > Judul > Bagian".
#[must_use]
pub fn heading_line(title: &str, heading_path: Option<&str>) -> String {
    let title = title.trim();
    let path = heading_path.map(str::trim).filter(|p| !p.is_empty());
    match path {
        None => title.to_string(),
        Some(path) => {
            if title.is_empty() {
                return path.to_string();
            }
            let outermost = path.split(" > ").next().unwrap_or(path).trim();
            if outermost.eq_ignore_ascii_case(title) {
                path.to_string()
            } else {
                format!("{title} > {path}")
            }
        }
    }
}

/// The text embedded for a chunk: the heading line, a blank line, the
/// content.
///
/// With no title and no heading path this is the content alone, which is what
/// a document with no title at all gets.
#[must_use]
pub fn embedding_text(title: &str, heading_path: Option<&str>, content: &str) -> String {
    let head = heading_line(title, heading_path);
    if head.is_empty() {
        content.to_string()
    } else {
        format!("{head}\n\n{content}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_title_wins_over_everything() {
        assert_eq!(
            document_title(Some("Given"), "report.pdf", "# Heading\n\nbody", true),
            "Given"
        );
        assert_eq!(
            document_title(Some("  "), "report.pdf", "# Heading\n\nbody", true),
            "Heading",
            "a blank explicit title is not a title"
        );
    }

    #[test]
    fn a_markdown_document_lends_its_h1_to_the_title() {
        assert_eq!(
            document_title(None, "1000580_1.md", "# DPTools\n\nbody", true),
            "DPTools"
        );
        assert_eq!(
            document_title(None, "notes.md", "\n\n# Deployment\n\nbody", true),
            "Deployment",
            "the first *non-empty* line is the heading"
        );
    }

    #[test]
    fn a_non_markdown_document_keeps_the_filename_stem() {
        assert_eq!(
            document_title(None, "report.pdf", "# Heading\n\nbody", false),
            "report"
        );
    }

    #[test]
    fn a_document_without_a_heading_keeps_the_filename_stem() {
        assert_eq!(
            document_title(None, "notes.md", "just a paragraph", true),
            "notes"
        );
        assert_eq!(
            document_title(None, "notes.md", "## A subsection\n\nbody", true),
            "notes",
            "an H2 does not name the document"
        );
    }

    #[test]
    fn the_filename_stem_strips_only_the_last_extension() {
        assert_eq!(filename_stem("report.pdf"), "report");
        assert_eq!(filename_stem("archive.tar.gz"), "archive.tar");
        assert_eq!(filename_stem("noextension"), "noextension");
        assert_eq!(filename_stem(".gitignore"), ".gitignore");
    }

    #[test]
    fn the_heading_path_does_not_repeat_the_title() {
        assert_eq!(
            heading_line("Judul", Some("Judul > Bagian")),
            "Judul > Bagian",
            "the path already starts with the title"
        );
        assert_eq!(
            heading_line("Judul", Some("Bagian")),
            "Judul > Bagian",
            "the path does not start with the title"
        );
        assert_eq!(heading_line("Judul", None), "Judul");
        assert_eq!(heading_line("Judul", Some("  ")), "Judul");
        assert_eq!(heading_line("", Some("Bagian")), "Bagian");
    }

    #[test]
    fn the_embedded_text_is_the_heading_line_then_the_content() {
        assert_eq!(
            embedding_text("Judul", Some("Judul > Bagian"), "isi"),
            "Judul > Bagian\n\nisi"
        );
        assert_eq!(embedding_text("Judul", None, "isi"), "Judul\n\nisi");
        assert_eq!(
            embedding_text("", None, "isi"),
            "isi",
            "no title and no path leaves the content alone"
        );
    }
}
