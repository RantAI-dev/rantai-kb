//! Per-file-type ingest policy and the MIME registry. Both are pure data +
//! pure functions, no I/O, no environment reads.

use std::path::Path;

/// Figure handling mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FigureMode {
    Auto,
    Force,
    Skip,
}

/// Per-file-type policy. The single decision table for what the pipeline does
/// to a document. Replaces the old UI "enhanced" toggle.
#[derive(Debug, Clone, Copy)]
pub struct IngestPolicy {
    /// LLM entity/relation extraction into the knowledge graph.
    pub entities: bool,
    /// Whether the figure-asset step may run at all (PDF only).
    pub figures: bool,
    /// Force the layout-extractor chain even when a text layer exists.
    pub force_layout: bool,
}

/// Central MIME type and extension registry. Mirrors the Bun service's
/// `src/lib/files/mime-types.ts`; clients build their accept lists from
/// `kb_accepted_extensions()` so they don't drift apart from the server.
pub struct MimeRegistry;

impl MimeRegistry {
    /// Every extension the KB accepts.
    #[must_use]
    pub fn kb_accepted_extensions() -> &'static [&'static str] {
        EXTENSIONS
    }

    /// KB upload limit in bytes.
    #[must_use]
    pub fn kb_max_file_bytes() -> u64 {
        KB_MAX_FILE_BYTES
    }

    /// Resolve an extension (with the leading dot) to its MIME type, if any.
    #[must_use]
    pub fn ext_to_mime(ext: &str) -> Option<&'static str> {
        EXT_TO_MIME.iter().find_map(|(k, v)| (*k == ext).then_some(*v))
    }
}

const TABULAR: &[&str] = &[
    ".xlsx", ".xls", ".ods", ".csv", ".tsv", ".json", ".jsonl",
];
const CODE: &[&str] = &[
    ".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".rs", ".java", ".c", ".cpp", ".h", ".rb", ".php",
    ".sh", ".sql", ".r", ".swift", ".kt", ".yaml", ".yml", ".toml", ".ini", ".env", ".log",
];
const IMAGE: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic"];
const MODEL_3D: &[&str] = &[".gltf", ".glb"];

/// Per-file-type ingest policy. PDF is the only type where `figureMode` matters;
/// for everything else the policy is determined by extension alone.
#[must_use]
pub fn resolve_ingest_policy(filename: &str, figure_mode: FigureMode) -> IngestPolicy {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();

    let ext_lc = ext.to_ascii_lowercase();

    if ext_lc == ".pdf" {
        return IngestPolicy {
            entities: true,
            figures: figure_mode != FigureMode::Skip,
            force_layout: figure_mode == FigureMode::Force,
        };
    }

    let no_entities = TABULAR.contains(&ext_lc.as_str())
        || CODE.contains(&ext_lc.as_str())
        || IMAGE.contains(&ext_lc.as_str())
        || MODEL_3D.contains(&ext_lc.as_str());
    IngestPolicy {
        entities: !no_entities,
        figures: false,
        force_layout: false,
    }
}

/// Parse an untrusted request/job param into a [`FigureMode`]. Accepts the
/// three valid strings plus a legacy `forceOCR` boolean.
#[must_use]
pub fn parse_figure_mode(value: Option<&str>, legacy_force_ocr: Option<bool>) -> FigureMode {
    if let Some(v) = value {
        match v {
            "force" => return FigureMode::Force,
            "skip" => return FigureMode::Skip,
            "auto" => return FigureMode::Auto,
            _ => {}
        }
    }
    if matches!(legacy_force_ocr, Some(true)) {
        return FigureMode::Force;
    }
    FigureMode::Auto
}

const KB_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

const EXT_TO_MIME: &[(&str, &str)] = &[
    (".pdf", "application/pdf"),
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".gif", "image/gif"),
    (".webp", "image/webp"),
    (".heic", "image/heic"),
    (".md", "text/markdown"),
    (".markdown", "text/markdown"),
    (".txt", "text/plain"),
    (".html", "text/html"),
    (".htm", "text/html"),
    (".xml", "application/xml"),
    (".csv", "text/csv"),
    (".tsv", "text/tab-separated-values"),
    (".json", "application/json"),
    (".jsonl", "application/x-ndjson"),
    (".yaml", "text/yaml"),
    (".yml", "text/yaml"),
    (".toml", "application/toml"),
    (
        ".docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        ".xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        ".pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    (".rtf", "application/rtf"),
    (".epub", "application/epub+zip"),
    (".doc", "application/msword"),
    (".xls", "application/vnd.ms-excel"),
    (".ppt", "application/vnd.ms-powerpoint"),
    (
        ".odt",
        "application/vnd.oasis.opendocument.text",
    ),
    (
        ".ods",
        "application/vnd.oasis.opendocument.spreadsheet",
    ),
    (".gltf", "model/gltf+json"),
    (".glb", "model/gltf-binary"),
    (".log", "text/plain"),
    (".ini", "text/plain"),
    (".env", "text/plain"),
    (".py", "text/x-python"),
    (".ts", "text/typescript"),
    (".tsx", "text/typescript"),
    (".js", "text/javascript"),
    (".jsx", "text/javascript"),
    (".go", "text/plain"),
    (".rs", "text/plain"),
    (".java", "text/plain"),
    (".c", "text/plain"),
    (".cpp", "text/plain"),
    (".h", "text/plain"),
    (".rb", "text/plain"),
    (".php", "text/plain"),
    (".sh", "text/plain"),
    (".sql", "text/plain"),
    (".r", "text/plain"),
    (".swift", "text/plain"),
    (".kt", "text/plain"),
];

const EXTENSIONS: &[&str] = &[
    ".pdf", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic", ".md", ".markdown", ".txt",
    ".html", ".htm", ".xml", ".csv", ".tsv", ".json", ".jsonl", ".yaml", ".yml", ".toml",
    ".docx", ".xlsx", ".pptx", ".rtf", ".epub", ".doc", ".xls", ".ppt", ".odt", ".ods",
    ".gltf", ".glb", ".log", ".ini", ".env", ".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".rs",
    ".java", ".c", ".cpp", ".h", ".rb", ".php", ".sh", ".sql", ".r", ".swift", ".kt",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabular_files_skip_entities_and_figures() {
        for name in ["data.xlsx", "data.xls", "d.ods", "d.csv", "d.tsv", "d.json", "d.jsonl"] {
            let p = resolve_injest(name);
            assert!(!p.entities, "{name}");
            assert!(!p.figures, "{name}");
            assert!(!p.force_layout, "{name}");
        }
    }

    #[test]
    fn pdf_auto_figures_possible_layout_not_forced() {
        let p = resolve_injest("book.pdf");
        assert!(p.entities);
        assert!(p.figures);
        assert!(!p.force_layout);
    }

    #[test]
    fn pdf_force_layout_chain_forced() {
        let p = resolve_ingest_policy("book.pdf", FigureMode::Force);
        assert!(p.figures);
        assert!(p.force_layout);
    }

    #[test]
    fn pdf_skip_no_figures() {
        let p = resolve_ingest_policy("scan.pdf", FigureMode::Skip);
        assert!(!p.figures);
        assert!(!p.force_layout);
    }

    #[test]
    fn figure_mode_ignored_for_non_pdf() {
        let p = resolve_ingest_policy("data.xlsx", FigureMode::Force);
        assert!(!p.figures);
        assert!(!p.force_layout);
    }

    #[test]
    fn prose_documents_get_entities_no_figures() {
        for name in ["a.docx", "a.doc", "a.pptx", "a.md", "a.txt", "a.html", "a.rtf", "a.epub"] {
            let p = resolve_injest(name);
            assert!(p.entities, "{name}");
            assert!(!p.figures, "{name}");
        }
    }

    #[test]
    fn code_config_images_and_3d_skip_entities() {
        for name in ["a.py", "a.ts", "a.yaml", "a.env", "img.png", "m.glb"] {
            assert!(!resolve_injest(name).entities, "{name}");
        }
    }

    #[test]
    fn extensions_are_case_insensitive() {
        assert!(!resolve_injest("DATA.XLSX").entities);
        assert!(resolve_injest("BOOK.PDF").figures);
    }

    #[test]
    fn parse_figure_mode_accepts_three_values() {
        assert_eq!(parse_figure_mode(Some("auto"), None), FigureMode::Auto);
        assert_eq!(parse_figure_mode(Some("force"), None), FigureMode::Force);
        assert_eq!(parse_figure_mode(Some("skip"), None), FigureMode::Skip);
    }

    #[test]
    fn parse_figure_mode_legacy_force_ocr() {
        assert_eq!(parse_figure_mode(None, Some(true)), FigureMode::Force);
    }

    #[test]
    fn parse_figure_mode_defaults_to_auto_on_garbage() {
        assert_eq!(parse_figure_mode(Some("yes"), None), FigureMode::Auto);
        assert_eq!(parse_figure_mode(None, None), FigureMode::Auto);
        assert_eq!(parse_figure_mode(None, Some(false)), FigureMode::Auto);
    }

    fn resolve_injest(name: &str) -> IngestPolicy {
        resolve_ingest_policy(name, FigureMode::Auto)
    }

    #[test]
    fn mime_registry_knows_pdf() {
        assert_eq!(MimeRegistry::ext_to_mime(".pdf"), Some("application/pdf"));
    }
}