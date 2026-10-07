//! Source-dedupe key and source-numbering helpers used by the engine when it
//! builds the context block. Mirrors `retriever.ts::sourceDedupeKey` and
//! `numberedSourceList`.

/// Inputs for [`source_dedupe_key`].
#[derive(Debug, Clone, Default)]
pub struct SourceDedupKey<'a> {
    pub document_title: Option<&'a str>,
    pub section: Option<&'a str>,
    pub asset_key: Option<&'a str>,
}

/// One source entry after numbering — the engine hands these to the wire DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberedSource<'a> {
    pub number: u32,
    pub document_title: &'a str,
    pub section: Option<&'a str>,
    pub figure: bool,
}

/// Stable dedupe key for a source. Figures are distinct by asset; text chunks
/// collapse by `documentTitle-section`. Mirrors the TypeScript
/// `sourceDedupeKey` so excerpt labels, the Sources list and the UI cards all
/// share the same `[N]`.
#[must_use]
pub fn source_dedupe_key(k: &SourceDedupKey<'_>) -> String {
    if let Some(asset) = k.asset_key {
        return format!("asset:{asset}");
    }
    let title = k.document_title.unwrap_or("Document");
    let section = k.section.unwrap_or("");
    format!("{title}-{section}")
}

/// Render the numbered source list (the "Sources:" footer in the context block).
/// Figure sources are tagged `[FIGURE]` so the model can embed them via
/// `[figure:N]` markers.
#[must_use]
pub fn numbered_source_list(sources: &[(&str, Option<&str>, bool)]) -> String {
    let mut out = String::new();
    for (i, (title, section, figure)) in sources.iter().enumerate() {
        let n = i + 1;
        let tag = if *figure { "[FIGURE] " } else { "" };
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("{n}. {tag}{title}"));
        if let Some(sec) = section {
            out.push_str(&format!(" — {sec}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_key_takes_precedence() {
        let k = SourceDedupKey {
            document_title: Some("Doc"),
            section: Some("sec"),
            asset_key: Some("a/b"),
        };
        assert_eq!(source_dedupe_key(&k), "asset:a/b");
    }

    #[test]
    fn text_uses_title_and_section() {
        let k = SourceDedupKey {
            document_title: Some("Apple 10-K"),
            section: Some("Income Statement"),
            asset_key: None,
        };
        assert_eq!(source_dedupe_key(&k), "Apple 10-K-Income Statement");
    }

    #[test]
    fn missing_title_is_document() {
        let k = SourceDedupKey::default();
        assert_eq!(source_dedupe_key(&k), "Document-");
    }

    #[test]
    fn numbered_list_includes_figure_tag() {
        let sources = vec![
            ("Apple 10-K", Some("Income Statement"), false),
            ("Digestive system", Some("Figure 1.15"), true),
        ];
        let out = numbered_source_list(
            &sources
                .iter()
                .map(|(t, s, f)| (*t, *s, *f))
                .collect::<Vec<_>>(),
        );
        assert!(out.contains("1. Apple 10-K — Income Statement"));
        assert!(out.contains("2. [FIGURE] Digestive system — Figure 1.15"));
    }
}