//! Filename and mime detection: which parser a file needs.
//!
//! Copied from `src/lib/rag/file-processor.ts` and `src/lib/files/mime-types.ts`
//! so the two services accept the same files and route them the same way.

/// The families `detectFileType` returns (`file-processor.ts:65`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedFileType {
    /// `.md`, `.markdown`
    Markdown,
    /// `.pdf`
    Pdf,
    /// `.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`, `.heic`
    Image,
    /// Office, ebook, RTF, OpenDocument, 3D model (`.docx` … `.glb`)
    Document,
    /// Structured data, config, code, plain text
    Text,
}

const MARKDOWN_EXTENSIONS: &[&str] = &[".md", ".markdown"];
const PDF_EXTENSIONS: &[&str] = &[".pdf"];
const IMAGE_EXTENSIONS: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic"];
const DOCUMENT_EXTENSIONS: &[&str] = &[
    ".docx", ".xlsx", ".pptx", ".rtf", ".epub", ".doc", ".xls", ".ppt", ".odt", ".ods", ".gltf",
    ".glb",
];
const TEXT_EXTENSIONS: &[&str] = &[
    ".csv", ".tsv", ".json", ".jsonl", ".html", ".htm", ".xml", ".yaml", ".yml", ".toml", ".py",
    ".ts", ".tsx", ".js", ".jsx", ".go", ".rs", ".java", ".c", ".cpp", ".h", ".rb", ".php", ".sh",
    ".sql", ".r", ".swift", ".kt", ".txt", ".log", ".ini", ".env",
];

/// Node's `path.extname`, lowercased: the last `.` of the basename and
/// everything after it, and empty when there is none.
///
/// A leading dot is not an extension (`.gitignore`), and the basename is what
/// counts, so `docs/report.PDF` yields `.pdf`.
#[must_use]
pub fn extension_of(path: &str) -> String {
    let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match basename.rfind('.') {
        Some(0) | None => String::new(),
        Some(index) => basename[index..].to_lowercase(),
    }
}

/// The file's family, or `None` when the extension is not supported
/// (`file-processor.ts:65`).
#[must_use]
pub fn detect_file_type(path: &str) -> Option<SupportedFileType> {
    let ext = extension_of(path);
    let ext = ext.as_str();
    if MARKDOWN_EXTENSIONS.contains(&ext) {
        Some(SupportedFileType::Markdown)
    } else if PDF_EXTENSIONS.contains(&ext) {
        Some(SupportedFileType::Pdf)
    } else if IMAGE_EXTENSIONS.contains(&ext) {
        Some(SupportedFileType::Image)
    } else if DOCUMENT_EXTENSIONS.contains(&ext) {
        Some(SupportedFileType::Document)
    } else if TEXT_EXTENSIONS.contains(&ext) {
        Some(SupportedFileType::Text)
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn extensions_match_node_path_extname() {
        assert_eq!(extension_of("report.PDF"), ".pdf");
        assert_eq!(extension_of("archive.tar.gz"), ".gz");
        assert_eq!(extension_of("docs/v1.2/report"), "");
        assert_eq!(extension_of(".gitignore"), "");
        assert_eq!(extension_of("noextension"), "");
        assert_eq!(extension_of("dir.d/report"), "");
        assert_eq!(extension_of(r"C:\docs\report.DOCX"), ".docx");
    }

    #[test]
    fn detection_covers_every_family() {
        for (name, expected) in [
            ("readme.md", SupportedFileType::Markdown),
            ("scan.pdf", SupportedFileType::Pdf),
            ("photo.JPEG", SupportedFileType::Image),
            ("book.epub", SupportedFileType::Document),
            ("rows.csv", SupportedFileType::Text),
        ] {
            assert_eq!(detect_file_type(name), Some(expected), "{name}");
        }
    }

    #[test]
    fn an_unknown_extension_is_not_supported() {
        assert_eq!(detect_file_type("archive.zip"), None);
        assert_eq!(detect_file_type(""), None);
    }
}
