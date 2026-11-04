//! Extension ↔ mime registry, copied from `src/lib/files/mime-types.ts`.
//!
//! The table is the upload contract: the accepted list, the KB validation and
//! the file-type dispatch all read from it, so a format added in one place is
//! accepted, routed and stored consistently. Order matters — the mime →
//! extension map is first-match-wins over this list, exactly as the TypeScript
//! `MIME_TO_EXT` reduce is.

/// Every mime type the knowledge base accepts (`mime-types.ts:7`).
pub const DOCUMENT_MIME_TYPES: &[&str] = &[
    "application/pdf",
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/heic",
    "text/plain",
    "text/markdown",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "text/csv",
    "text/tab-separated-values",
    "application/json",
    "application/x-ndjson",
    "text/html",
    "application/xml",
    "text/xml",
    "application/rtf",
    "application/epub+zip",
    "text/yaml",
    "application/toml",
    "text/x-python",
    "text/javascript",
    "text/typescript",
    "application/msword",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.oasis.opendocument.spreadsheet",
    "model/gltf+json",
    "model/gltf-binary",
];

/// Extension → mime, in the TypeScript object's own order (`mime-types.ts:58`).
pub const EXT_TO_MIME: &[(&str, &str)] = &[
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
    (".odt", "application/vnd.oasis.opendocument.text"),
    (".ods", "application/vnd.oasis.opendocument.spreadsheet"),
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

/// The mime type for a lowercased extension.
#[must_use]
pub fn ext_to_mime(ext: &str) -> Option<&'static str> {
    EXT_TO_MIME
        .iter()
        .find(|(known, _)| *known == ext)
        .map(|(_, mime)| *mime)
}

/// The canonical extension for a mime type: first match in [`EXT_TO_MIME`],
/// so `application/xml` maps to `.xml` and not to a later alias.
#[must_use]
pub fn mime_to_ext(mime: &str) -> Option<&'static str> {
    EXT_TO_MIME
        .iter()
        .find(|(_, known)| *known == mime)
        .map(|(ext, _)| *ext)
}

/// Whether the knowledge base accepts this mime type (`mime-types.ts:141`).
#[must_use]
pub fn is_document_mime(mime: &str) -> bool {
    DOCUMENT_MIME_TYPES.contains(&mime)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_by_lowercased_extension() {
        assert_eq!(
            ext_to_mime(".docx"),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        );
        assert_eq!(ext_to_mime(".MD"), None, "callers lowercase first");
        assert_eq!(ext_to_mime(".zip"), None);
    }

    #[test]
    fn first_extension_wins_for_a_mime() {
        // `.jpg` precedes `.jpeg` and `.html` precedes `.htm`.
        assert_eq!(mime_to_ext("image/jpeg"), Some(".jpg"));
        assert_eq!(mime_to_ext("text/html"), Some(".html"));
        assert_eq!(mime_to_ext("application/xml"), Some(".xml"));
        assert_eq!(mime_to_ext("application/unknown"), None);
    }

    #[test]
    fn every_accepted_mime_has_an_extension() {
        for mime in DOCUMENT_MIME_TYPES {
            // `text/xml` is accepted for upload but no extension maps to it,
            // because `.xml` maps to `application/xml`. Same in TypeScript,
            // where MIME_TO_EXT is reduced from EXT_TO_MIME only.
            if *mime == "text/xml" {
                assert_eq!(mime_to_ext(mime), None);
                continue;
            }
            assert!(mime_to_ext(mime).is_some(), "{mime}");
        }
    }

    #[test]
    fn accepted_and_rejected_mimes_match_the_typescript_lists() {
        assert!(is_document_mime("application/epub+zip"));
        assert!(is_document_mime("image/heic"));
        assert!(!is_document_mime("application/zip"));
        assert_eq!(DOCUMENT_MIME_TYPES.len(), 32);
    }
}
