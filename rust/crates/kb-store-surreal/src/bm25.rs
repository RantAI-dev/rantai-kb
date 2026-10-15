//! BM25 full-text search over SurrealDB.

use kb_ports::{DocumentScope, PortError, SearchHit};

use crate::client::{SurrealChunkStore, build_hit, row_to_chunk, safe_limit};
use crate::queries::{BM25_SEARCH_QUERY, BM25_SEARCH_QUERY_UNSCOPED, surql_id_list};

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

    let sql = BM25_SEARCH_QUERY
        .replace("{doc_ids}", &doc_ids_sql)
        .replace("{limit}", &limit.to_string());

    let envelope = store.query_with_params(&sql, &[("q", trimmed)]).await?;
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
    let sql = BM25_SEARCH_QUERY_UNSCOPED.replace("{limit}", &limit.to_string());

    let envelope = store.query_with_params(&sql, &[("q", trimmed)]).await?;
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
