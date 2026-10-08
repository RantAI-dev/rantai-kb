//! Per-document chunk reads: counts and ordered chunk listing.
//!
//! Chunks live in SurrealDB (`document_chunk`), not Postgres. Mirrors
//! `getDocumentChunkCounts` and the chunk listing in `src/lib/rag/vector-store.ts`.

use kb_ports::{ChunkMetadata, DocumentId, PortError};
use serde_json::Value;

use crate::client::{SurrealChunkStore, row_meta, row_to_chunk};
use crate::queries::{surql_id_list, validate_document_id};

/// Chunk counts for many documents, grouped in one query.
/// Documents with no chunks are omitted from the result.
pub async fn execute_chunk_counts(
    store: &SurrealChunkStore,
    document_ids: &[DocumentId],
) -> Result<Vec<(DocumentId, u32)>, PortError> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = document_ids.iter().map(|d| d.0.clone()).collect();
    let ids_sql = surql_id_list(&ids)?;
    let sql = format!(
        "SELECT document_id, count() AS n FROM document_chunk \
         WHERE document_id IN {ids_sql} GROUP BY document_id;"
    );
    let rows = store.query(&sql).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let doc = row.get("document_id").and_then(Value::as_str);
        let n = row.get("n").and_then(Value::as_u64);
        if let (Some(doc), Some(n)) = (doc, n) {
            out.push((DocumentId::new_unchecked(doc.to_string()), n as u32));
        }
    }
    Ok(out)
}

/// Ordered chunks for one document: `(chunk_index, content, metadata)`.
pub async fn execute_ordered_chunks(
    store: &SurrealChunkStore,
    document_id: &DocumentId,
) -> Result<Vec<(u32, String, ChunkMetadata)>, PortError> {
    validate_document_id(&document_id.0)?;
    let sql = format!(
        "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix \
         FROM document_chunk WHERE document_id = \"{}\" ORDER BY chunk_index ASC;",
        document_id.0
    );
    let rows = store.query(&sql).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(chunk) = row_to_chunk(&row) else {
            return Err(PortError::Malformed(
                "surreal ordered_chunks: bad row".into(),
            ));
        };
        let index = u32::try_from(chunk.chunk_index)
            .map_err(|_| PortError::Malformed("surreal ordered_chunks: negative index".into()))?;
        let meta = row_meta(chunk.metadata.as_ref());
        out.push((index, chunk.content, meta));
    }
    Ok(out)
}
