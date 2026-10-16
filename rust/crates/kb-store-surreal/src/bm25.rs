//! BM25 full-text search over SurrealDB.

use kb_ports::{DocumentScope, PortError, SearchHit};

use crate::client::{SurrealChunkStore, build_hit, row_to_chunk, safe_limit};
use crate::queries::{BM25_SEARCH_QUERY, BM25_SEARCH_QUERY_UNSCOPED, surql_id_list};

fn is_exp_or_terms_enabled() -> bool {
    std::env::var("KB_EXP_BM25_OR_TERMS")
        .map(|v| v.trim() == "true" || v.trim() == "1")
        .unwrap_or(false)
}

fn extract_query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for word in query.split(|c: char| !c.is_alphanumeric()) {
        let clean: String = word.trim().to_lowercase();
        if !clean.is_empty() && seen.insert(clean.clone()) {
            terms.push(clean);
        }
    }
    terms.truncate(10);
    terms
}

/// Execute BM25 full-text search against SurrealDB.
///
/// Search text is never spliced into the query string; it is passed as a query
/// parameter `$q` on the HTTP request.
pub async fn execute_bm25_search(
    store: &SurrealChunkStore,
    scope: &DocumentScope,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, PortError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = scope.document_ids().iter().map(|d| d.0.clone()).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let limit = safe_limit(limit);
    let doc_ids_sql = surql_id_list(&ids)?;

    let envelope = if is_exp_or_terms_enabled() {
        let terms = extract_query_terms(trimmed);
        if terms.is_empty() {
            let sql = BM25_SEARCH_QUERY
                .replace("{doc_ids}", &doc_ids_sql)
                .replace("{limit}", &limit.to_string());
            store.query_with_params(&sql, &[("q", trimmed)]).await?
        } else {
            let score_expr = (0..terms.len())
                .map(|i| format!("(search::score({i}) ?? 0.0)"))
                .collect::<Vec<_>>()
                .join(" + ");
            let where_or = (0..terms.len())
                .map(|i| format!("content @{i}@ $t{i}"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let sql = format!(
                "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix, \
                 ({score_expr}) AS score \
                 FROM document_chunk \
                 WHERE ({where_or}) AND document_id IN {doc_ids_sql} \
                 ORDER BY score DESC, id ASC \
                 LIMIT {limit};"
            );
            let param_keys: Vec<String> = (0..terms.len()).map(|i| format!("t{i}")).collect();
            let params: Vec<(&str, &str)> = param_keys
                .iter()
                .zip(terms.iter())
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            store.query_with_params(&sql, &params).await?
        }
    } else {
        let sql = BM25_SEARCH_QUERY
            .replace("{doc_ids}", &doc_ids_sql)
            .replace("{limit}", &limit.to_string());
        store.query_with_params(&sql, &[("q", trimmed)]).await?
    };

    let rows = SurrealChunkStore::extract_rows(&envelope);
    let mut hits = Vec::with_capacity(rows.len());
    for (i, row) in rows.into_iter().enumerate() {
        if let Some(r) = row_to_chunk(&row) {
            let score = r.score.unwrap_or(0.0);
            hits.push(build_hit(r, i as u32, score));
        }
    }
    Ok(hits)
}

/// Execute unscoped BM25 full-text search across all documents in SurrealDB.
///
/// Matches TypeScript `bm25Search`: results are subsequent to verification
/// against alive tenant documents in Postgres.
pub async fn execute_bm25_search_global(
    store: &SurrealChunkStore,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, PortError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let limit = safe_limit(limit);

    let envelope = if is_exp_or_terms_enabled() {
        let terms = extract_query_terms(trimmed);
        if terms.is_empty() {
            let sql = BM25_SEARCH_QUERY_UNSCOPED.replace("{limit}", &limit.to_string());
            store.query_with_params(&sql, &[("q", trimmed)]).await?
        } else {
            let score_expr = (0..terms.len())
                .map(|i| format!("(search::score({i}) ?? 0.0)"))
                .collect::<Vec<_>>()
                .join(" + ");
            let where_or = (0..terms.len())
                .map(|i| format!("content @{i}@ $t{i}"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let sql = format!(
                "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix, \
                 ({score_expr}) AS score \
                 FROM document_chunk \
                 WHERE ({where_or}) \
                 ORDER BY score DESC, id ASC \
                 LIMIT {limit};"
            );
            let param_keys: Vec<String> = (0..terms.len()).map(|i| format!("t{i}")).collect();
            let params: Vec<(&str, &str)> = param_keys
                .iter()
                .zip(terms.iter())
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            store.query_with_params(&sql, &params).await?
        }
    } else {
        let sql = BM25_SEARCH_QUERY_UNSCOPED.replace("{limit}", &limit.to_string());
        store.query_with_params(&sql, &[("q", trimmed)]).await?
    };

    let rows = SurrealChunkStore::extract_rows(&envelope);
    let mut hits = Vec::with_capacity(rows.len());
    for (i, row) in rows.into_iter().enumerate() {
        if let Some(r) = row_to_chunk(&row) {
            let score = r.score.unwrap_or(0.0);
            hits.push(build_hit(r, i as u32, score));
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_terms_extracts_unique_alphanumeric_lowercased() {
        let terms = extract_query_terms("Siapakah yang menulis Candy Candy?");
        assert_eq!(terms, vec!["siapakah", "yang", "menulis", "candy"]);
    }

    #[test]
    fn exp_or_flag_defaults_to_false() {
        assert!(!is_exp_or_terms_enabled());
    }
}
