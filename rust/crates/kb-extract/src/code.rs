//! Source files wrapped in a fenced code block.
//!
//! `CODE_EXT_LANGUAGE` and the wrapper are copied from `src/lib/files/parsers.ts:21`
//! and `:308`. The fence is what lets the chunker keep a code file in one piece
//! instead of splitting it at punctuation, so the language tag matters.

/// Fence language per extension, in the TypeScript object's own order.
pub const CODE_EXT_LANGUAGE: &[(&str, &str)] = &[
    (".py", "python"),
    (".ts", "typescript"),
    (".tsx", "typescript"),
    (".js", "javascript"),
    (".jsx", "javascript"),
    (".go", "go"),
    (".rs", "rust"),
    (".java", "java"),
    (".c", "c"),
    (".cpp", "cpp"),
    (".h", "c"),
    (".rb", "ruby"),
    (".php", "php"),
    (".sh", "bash"),
    (".sql", "sql"),
    (".r", "r"),
    (".swift", "swift"),
    (".kt", "kotlin"),
];

/// The fence language for an extension, if the file is a code file.
#[must_use]
pub fn language_of(ext: &str) -> Option<&'static str> {
    CODE_EXT_LANGUAGE
        .iter()
        .find(|(known, _)| *known == ext)
        .map(|(_, language)| *language)
}

/// `[Code: {fileName}]\n```{language}\n{code}\n```` (`parsers.ts:311`).
#[must_use]
pub fn wrap_as_code(file_name: &str, language: &str, code: &str) -> String {
    format!("[Code: {file_name}]\n```{language}\n{code}\n```")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn languages_cover_the_typescript_table() {
        assert_eq!(language_of(".rs"), Some("rust"));
        assert_eq!(language_of(".tsx"), Some("typescript"));
        assert_eq!(language_of(".h"), Some("c"));
        assert_eq!(language_of(".txt"), None);
        assert_eq!(language_of(".json"), None);
    }

    #[test]
    fn the_fence_matches_the_typescript_template() {
        assert_eq!(
            wrap_as_code("main.rs", "rust", "fn main() {}"),
            "[Code: main.rs]\n```rust\nfn main() {}\n```"
        );
    }
}
