//! Figure co-retrieval over SurrealDB.
//!
//! Mirrors `fetchMatchingFigures` in `src/lib/rag/hybrid-search.ts`.

use kb_ports::{DocumentId, DocumentScope, PortError, SearchHit};
use std::collections::HashSet;

use crate::client::{SurrealChunkStore, build_hit, row_meta, row_to_chunk, safe_limit};
use crate::queries::{FIGURES_QUERY, surql_id_list};

const FIG_CAPTION_STOPWORDS: &[&str] = &[
    "yang", "untuk", "pada", "dan", "dengan", "dari", "atau", "adalah", "dalam", "serta", "oleh",
    "hal", "ini", "itu", "para", "kepada", "gambar", "tabel", "raja", "dewa", "kitab", "hari",
    "suci",
];

const FIG_LABELS: &[&str] = &[
    "gambar",
    "tabel",
    "grafik",
    "diagram",
    "foto",
    "bagan",
    "ilustrasi",
    "peta",
];

pub fn fig_caption_meaningful(caption: &str) -> bool {
    let lower = caption.to_lowercase();
    let trimmed = lower.trim_start();
    FIG_LABELS.iter().any(|label| {
        if let Some(rest) = trimmed.strip_prefix(label) {
            rest.chars()
                .next()
                .map(|c| c.is_whitespace() || c == '.' || c == ':')
                .unwrap_or(true)
        } else {
            false
        }
    })
}

pub fn fig_caption_keywords(caption: &str) -> Vec<String> {
    let lower = caption.to_lowercase();
    let mut body = caption.trim_start();

    // Strip label and optional number/dot
    for label in FIG_LABELS {
        if let Some(pos) = lower.find(label) {
            let mut end = pos + label.len();
            let bytes = caption.as_bytes();
            while end < bytes.len() {
                let b = bytes[end];
                if b.is_ascii_whitespace() || b.is_ascii_digit() || b == b'.' || b == b':' {
                    end += 1;
                } else {
                    break;
                }
            }
            body = &caption[end..];
            break;
        }
    }

    body.split(|c: char| !c.is_alphabetic())
        .map(str::to_lowercase)
        .filter(|w| w.len() >= 4 && !FIG_CAPTION_STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Co-retrieve figures belonging to retrieved documents whose anchor or caption matches.
pub async fn execute_figures(
    store: &SurrealChunkStore,
    scope: &DocumentScope,
    query: &str,
    retrieved_text: &str,
    retrieved_doc_ids: &[DocumentId],
    anchored_chunks: &[(DocumentId, u32)],
    limit: u32,
) -> Result<Vec<SearchHit>, PortError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let scope_set: HashSet<&str> = scope.document_ids().iter().map(|d| d.0.as_str()).collect();
    if scope_set.is_empty() {
        return Ok(Vec::new());
    }

    // Intersect retrieved_doc_ids with scope
    let mut target_docs: Vec<String> = retrieved_doc_ids
        .iter()
        .filter(|d| scope_set.contains(d.0.as_str()))
        .map(|d| d.0.clone())
        .collect();
    target_docs.sort();
    target_docs.dedup();

    if target_docs.is_empty() {
        return Ok(Vec::new());
    }

    let anchor_set: HashSet<(String, u32)> = anchored_chunks
        .iter()
        .map(|(d, idx)| (d.0.clone(), *idx))
        .collect();

    let doc_ids_sql = surql_id_list(&target_docs)?;
    let sql = FIGURES_QUERY.replace("{doc_ids}", &doc_ids_sql);

    let envelope = store.query(&sql).await?;
    let rows = SurrealChunkStore::extract_rows(&envelope);

    let q_lower = query.to_lowercase();
    let text_lower = retrieved_text.to_lowercase();

    let mut scored_rows: Vec<(i32, crate::client::ChunkRow)> = Vec::new();
    let mut seen_caption: HashSet<String> = HashSet::new();

    for r in rows {
        let Some(chunk_row) = row_to_chunk(&r) else {
            continue;
        };
        let meta = row_meta(chunk_row.metadata.as_ref());
        let Some(asset_key) = meta.asset_key else {
            continue;
        };
        if asset_key.trim().is_empty() {
            continue;
        }

        let caption = meta.section.as_deref().unwrap_or(&chunk_row.content).trim();
        let cap_key = caption.to_lowercase();

        let anchored = meta
            .anchor_chunk_index
            .map(|idx| anchor_set.contains(&(chunk_row.document_id.clone(), idx)))
            .unwrap_or(false);

        if seen_caption.contains(&cap_key) {
            continue;
        }

        let mut score = 0;
        if anchored {
            score = 3;
        } else {
            if !fig_caption_meaningful(caption) {
                continue;
            }
            let kws = fig_caption_keywords(caption);
            if kws.is_empty() {
                continue;
            }
            if kws.iter().any(|kw| q_lower.contains(kw)) {
                score = 2;
            } else if kws.iter().any(|kw| text_lower.contains(kw)) {
                score = 1;
            }
            if score == 0 {
                continue;
            }
        }

        if !cap_key.is_empty() {
            seen_caption.insert(cap_key);
        }
        scored_rows.push((score, chunk_row));
    }

    scored_rows.sort_by(|a, b| b.0.cmp(&a.0));
    let limit = safe_limit(limit) as usize;
    let mut hits = Vec::with_capacity(limit.min(scored_rows.len()));
    for (_, r) in scored_rows.into_iter().take(limit) {
        hits.push(build_hit(r, u32::MAX, 0.0));
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fig_caption_meaningful_detects_indonesian_prefixes() {
        assert!(fig_caption_meaningful("Gambar 1.15 Posisi usus"));
        assert!(fig_caption_meaningful("tabel 2: Data"));
        assert!(fig_caption_meaningful("Peta persebaran"));
        assert!(!fig_caption_meaningful("Random description"));
    }

    #[test]
    fn fig_caption_keywords_strips_stopwords_and_labels() {
        let kws = fig_caption_keywords("Gambar 1.15 Posisi usus besar dan kecil");
        assert!(kws.contains(&"posisi".to_string()));
        assert!(kws.contains(&"usus".to_string()));
        assert!(kws.contains(&"besar".to_string()));
        assert!(kws.contains(&"kecil".to_string()));
        assert!(!kws.contains(&"gambar".to_string()));
        assert!(!kws.contains(&"dan".to_string())); // stopword
    }
}
