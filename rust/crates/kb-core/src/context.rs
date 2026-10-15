//! Source-dedupe key and source-numbering helpers used by the engine when it
//! builds the context block. Mirrors `retriever.ts::sourceDedupeKey` and
//! `numberedSourceList`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Inputs for [`source_dedupe_key`].
#[derive(Debug, Clone, Default)]
pub struct SourceDedupKey<'a> {
    pub document_title: Option<&'a str>,
    pub section: Option<&'a str>,
    pub asset_key: Option<&'a str>,
}

/// One source entry after numbering — the engine hands these to the wire DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEntry {
    pub document_id: Option<String>,
    pub document_title: String,
    pub section: Option<String>,
    pub categories: Vec<String>,
    pub asset_key: Option<String>,
    pub page: Option<u32>,
    pub chunk_type: Option<String>,
}

/// One source entry for simple display.
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

/// Input chunk for building context string and sources.
#[derive(Debug, Clone)]
pub struct ContextItem<'a> {
    pub document_id: Option<&'a str>,
    pub document_title: &'a str,
    pub section: Option<&'a str>,
    pub categories: &'a [String],
    pub asset_key: Option<&'a str>,
    pub page: Option<u32>,
    pub chunk_type: Option<&'a str>,
    pub content: &'a str,
    pub contextual_prefix: Option<&'a str>,
}

/// Build the formatted context string and deduplicated sources list.
/// Mirrors `retrieveContext` in `src/lib/rag/retriever.ts`.
#[must_use]
pub fn build_context_and_sources(chunks: &[ContextItem<'_>]) -> (String, Vec<SourceEntry>) {
    if chunks.is_empty() {
        return (String::new(), Vec::new());
    }

    let mut source_keys: Vec<String> = Vec::new();
    let mut sources: Vec<SourceEntry> = Vec::new();
    let mut key_to_number: HashMap<String, usize> = HashMap::new();

    for c in chunks {
        let key = source_dedupe_key(&SourceDedupKey {
            document_title: Some(c.document_title),
            section: c.section,
            asset_key: c.asset_key,
        });
        if !key_to_number.contains_key(&key) {
            let n = source_keys.len() + 1;
            key_to_number.insert(key.clone(), n);
            source_keys.push(key);
            sources.push(SourceEntry {
                document_id: c.document_id.map(str::to_string),
                document_title: c.document_title.to_string(),
                section: c.section.map(str::to_string),
                categories: c.categories.to_vec(),
                asset_key: c.asset_key.map(str::to_string),
                page: c.page,
                chunk_type: c.chunk_type.map(str::to_string),
            });
        }
    }

    let mut context_parts: Vec<String> = Vec::with_capacity(chunks.len());
    for c in chunks {
        let key = source_dedupe_key(&SourceDedupKey {
            document_title: Some(c.document_title),
            section: c.section,
            asset_key: c.asset_key,
        });
        let n = key_to_number.get(&key).copied().unwrap_or(1);
        let label = match c.section {
            Some(sec) if !sec.trim().is_empty() => {
                format!("[{n}] {} — {sec}", c.document_title)
            }
            _ => format!("[{n}] {}", c.document_title),
        };
        let prefix = match c.contextual_prefix {
            Some(p) if !p.trim().is_empty() => format!("{}\n\n", p.trim()),
            _ => String::new(),
        };
        context_parts.push(format!("{label}\n{prefix}{}", c.content));
    }

    let context = context_parts.join("\n\n---\n\n");
    (context, sources)
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
        let sources = [
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

    #[test]
    fn build_context_and_sources_empty() {
        let (context, sources) = build_context_and_sources(&[]);
        assert!(context.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn build_context_and_sources_matches_retriever_format() {
        let cat = vec!["finance".to_string()];
        let items = vec![
            ContextItem {
                document_id: Some("doc-1"),
                document_title: "Apple 10-K",
                section: Some("Income"),
                categories: &cat,
                asset_key: None,
                page: Some(12),
                chunk_type: Some("text"),
                content: "Revenue was $100B.",
                contextual_prefix: Some("Q4 summary"),
            },
            ContextItem {
                document_id: Some("doc-1"),
                document_title: "Apple 10-K",
                section: Some("Income"),
                categories: &cat,
                asset_key: None,
                page: Some(13),
                chunk_type: Some("text"),
                content: "Net income was $30B.",
                contextual_prefix: None,
            },
        ];
        let (context, sources) = build_context_and_sources(&items);
        assert_eq!(sources.len(), 1); // deduplicated by title-section
        assert_eq!(sources[0].document_title, "Apple 10-K");
        assert!(context.contains("[1] Apple 10-K — Income\nQ4 summary\n\nRevenue was $100B."));
        assert!(context.contains("\n\n---\n\n"));
        assert!(context.contains("[1] Apple 10-K — Income\nNet income was $30B."));
    }
}
