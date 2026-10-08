//! Neighbour chunk retrieval over SurrealDB.

use kb_ports::{DocumentScope, NeighbourAnchor, PortError, SearchHit};
use std::collections::HashSet;

use crate::client::{SurrealChunkStore, build_hit, row_to_chunk};
use crate::queries::{NEIGHBOURS_QUERY, surql_id_list, surql_u32_array};

/// Fetch neighbouring chunks within `window` offsets of anchors.
///
/// Filtered by `scope` to ensure strict tenant boundaries.
pub async fn execute_neighbours(
    store: &SurrealChunkStore,
    scope: &DocumentScope,
    anchors: &[NeighbourAnchor],
    window: u32,
) -> Result<Vec<SearchHit>, PortError> {
    if anchors.is_empty() || window == 0 {
        return Ok(Vec::new());
    }

    let allowed_ids: HashSet<&str> = scope.document_ids().iter().map(|d| d.0.as_str()).collect();
    if allowed_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut targets: HashSet<(String, u32)> = HashSet::new();
    let mut ddocs: HashSet<String> = HashSet::new();

    for a in anchors {
        if !allowed_ids.contains(a.document_id.0.as_str()) {
            continue;
        }
        ddocs.insert(a.document_id.0.clone());
        for d in 1..=window as i64 {
            let pos = a.chunk_index as i64 + d;
            if pos >= 0 {
                targets.insert((a.document_id.0.clone(), pos as u32));
            }
            let neg = a.chunk_index as i64 - d;
            if neg >= 0 {
                targets.insert((a.document_id.0.clone(), neg as u32));
            }
        }
    }

    if ddocs.is_empty() || targets.is_empty() {
        return Ok(Vec::new());
    }

    let mut indices: Vec<u32> = targets.iter().map(|(_, i)| *i).collect();
    indices.sort_unstable();
    indices.dedup();

    let mut doc_ids: Vec<String> = ddocs.into_iter().collect();
    doc_ids.sort();

    let doc_ids_sql = surql_id_list(&doc_ids)?;
    let indices_sql = surql_u32_array(&indices);

    let sql = NEIGHBOURS_QUERY
        .replace("{doc_ids}", &doc_ids_sql)
        .replace("{indices}", &indices_sql);

    let envelope = store.query(&sql).await?;
    let rows = SurrealChunkStore::extract_rows(&envelope);

    let anchor_ids: HashSet<String> = anchors.iter().map(|a| a.chunk_id.0.clone()).collect();
    let mut hits = Vec::with_capacity(rows.len());
    let mut seen: HashSet<String> = HashSet::new();

    for row in rows {
        let Some(r) = row_to_chunk(&row) else {
            continue;
        };
        if anchor_ids.contains(&r.id) || !seen.insert(r.id.clone()) {
            continue;
        }
        hits.push(build_hit(r, u32::MAX, 0.0));
    }

    Ok(hits)
}
