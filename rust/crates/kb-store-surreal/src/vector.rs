//! Vector similarity search over SurrealDB.

use kb_ports::{DocumentScope, PortError, SearchHit};

use crate::client::{SurrealChunkStore, build_hit, row_to_chunk, safe_limit};
use crate::queries::{VECTOR_SEARCH_QUERY, surql_f32_array, surql_id_list};

/// Execute vector similarity search against SurrealDB.
pub async fn execute_vector_search(
    store: &SurrealChunkStore,
    scope: &DocumentScope,
    embedding: &[f32],
    limit: u32,
) -> Result<Vec<SearchHit>, PortError> {
    if embedding.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = scope.document_ids().iter().map(|d| d.0.clone()).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let limit = safe_limit(limit);
    let doc_ids_sql = surql_id_list(&ids)?;
    let emb_sql = surql_f32_array(embedding);

    let sql = VECTOR_SEARCH_QUERY
        .replace("{embedding}", &emb_sql)
        .replace("{doc_ids}", &doc_ids_sql)
        .replace("{limit}", &limit.to_string());

    let envelope = store.query(&sql).await?;
    let rows = SurrealChunkStore::extract_rows(&envelope);
    let mut hits = Vec::with_capacity(rows.len());
    for (i, row) in rows.into_iter().enumerate() {
        if let Some(r) = row_to_chunk(&row) {
            let sim = r.similarity.unwrap_or(0.0);
            hits.push(build_hit(r, i as u32, sim));
        }
    }
    Ok(hits)
}
