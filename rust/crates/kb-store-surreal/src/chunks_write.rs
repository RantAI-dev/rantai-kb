//! Chunk writes into SurrealDB `document_chunk`.
//!
//! Mirrors `storeChunks` in `src/lib/rag/vector-store.ts`: one `CREATE` per
//! chunk under the deterministic id `${document_id}_${i}`, so re-running a
//! document's ingest replaces its chunks instead of duplicating them.
//!
//! Two contracts from the TS path are kept here because they protect the vector
//! index rather than the caller:
//!
//! * every embedding in a batch must have the same non-zero length — the MTREE
//!   index is dimension-bound at `DEFINE` time and drift corrupts it;
//! * `contextual_prefix` and `embedding_model` are omitted rather than set to
//!   NULL, because SurrealDB rejects a literal NULL for an `option<T>` field.

use kb_ports::{ChunkToStore, DocumentId, PortError};

use crate::client::SurrealChunkStore;
use crate::queries::{surql_f32_array, surql_json, surql_string, validate_document_id};

/// Chunks per `/sql` request. TS bounds its fan-out with
/// `STORE_CHUNKS_CONCURRENCY` for the same reason: a 400-chunk book must not
/// turn into one request the size of the book.
const BATCH: usize = 64;

/// Store a batch of chunks for one document.
///
/// Ids are rendered from an already-validated document id plus an integer, and
/// every other value is a JSON-escaped SurrealQL literal, so no caller-supplied
/// text can change the shape of the statement.
pub async fn execute_store_chunks(
    store: &SurrealChunkStore,
    document_id: &DocumentId,
    chunks: &[ChunkToStore],
    embedding_model: Option<&str>,
) -> Result<(), PortError> {
    validate_document_id(&document_id.0)?;
    if chunks.is_empty() {
        return Ok(());
    }
    check_dimensions(document_id, chunks)?;
    let model = embedding_model.filter(|m| !m.is_empty());

    let mut index: u32 = 0;
    for batch in chunks.chunks(BATCH) {
        let mut statements = Vec::with_capacity(batch.len());
        for chunk in batch {
            statements.push(create_statement(document_id, chunk, index, model));
            index = index.saturating_add(1);
        }
        // One request per batch. A statement SurrealDB rejects fails the whole
        // request, so a partial write can only leave whole batches behind.
        store.query(&statements.join("\n")).await?;
    }
    Ok(())
}

/// The `CREATE document_chunk SET ...` statement for one chunk.
fn create_statement(
    document_id: &DocumentId,
    chunk: &ChunkToStore,
    index: u32,
    embedding_model: Option<&str>,
) -> String {
    let mut sets = vec![
        format!(
            "id = {}",
            surql_string(&chunk_record_id(&document_id.0, index))
        ),
        format!("document_id = {}", surql_string(&document_id.0)),
        format!("content = {}", surql_string(&chunk.content)),
        format!("chunk_index = {index}"),
        format!("embedding = {}", surql_f32_array(&chunk.embedding)),
        format!("metadata = {}", surql_json(&chunk.metadata)),
        "created_at = time::now()".to_string(),
    ];
    // SurrealDB rejects an explicit NULL for `option<T>`, so an absent value is
    // left out of the assignment list entirely.
    if let Some(prefix) = chunk.contextual_prefix.as_deref().filter(|p| !p.is_empty()) {
        sets.push(format!("contextual_prefix = {}", surql_string(prefix)));
    }
    if let Some(model) = embedding_model {
        sets.push(format!("embedding_model = {}", surql_string(model)));
    }
    format!("CREATE document_chunk SET {};", sets.join(", "))
}

/// Record id for one chunk. Safe to embed in SurrealQL because both halves are
/// already restricted to `[A-Za-z0-9_-]`.
#[must_use]
pub fn chunk_record_id(document_id: &str, index: u32) -> String {
    format!("{document_id}_{index}")
}

/// Reject an empty or ragged embedding batch before it reaches the index.
fn check_dimensions(document_id: &DocumentId, chunks: &[ChunkToStore]) -> Result<(), PortError> {
    let first = chunks[0].embedding.len();
    if first == 0 {
        return Err(PortError::Malformed(format!(
            "embedding batch for {document_id} starts with an empty vector"
        )));
    }
    for (i, chunk) in chunks.iter().enumerate() {
        if chunk.embedding.len() != first {
            return Err(PortError::Malformed(format!(
                "embedding dim drift at chunk {i} for {document_id}: {} vs {first}",
                chunk.embedding.len()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn chunk(index: u32, dim: usize) -> ChunkToStore {
        ChunkToStore {
            content: format!("body {index}"),
            chunk_index: index,
            embedding: vec![0.5; dim],
            metadata: serde_json::json!({ "chunkIndex": index, "documentTitle": "t" }),
            contextual_prefix: None,
        }
    }

    #[test]
    fn record_ids_follow_the_typescript_shape() {
        assert_eq!(chunk_record_id("doc_1", 0), "doc_1_0");
        assert_eq!(chunk_record_id("doc_1", 42), "doc_1_42");
    }

    #[test]
    fn dimension_drift_is_rejected() {
        let doc = DocumentId::new_unchecked("doc_1");
        assert!(check_dimensions(&doc, &[chunk(0, 4), chunk(1, 2)]).is_err());
        assert!(check_dimensions(&doc, &[chunk(0, 0)]).is_err());
        assert!(check_dimensions(&doc, &[chunk(0, 4), chunk(1, 4)]).is_ok());
    }

    #[test]
    fn strings_are_quoted_not_spliced() {
        // A semicolon in content must not be able to close the statement.
        let rendered = surql_string("a\"; DELETE document_chunk; --");
        assert_eq!(rendered, "\"a\\\"; DELETE document_chunk; --\"");
        assert!(!rendered.contains("';"));
    }

    #[test]
    fn statement_writes_the_typescript_column_set() {
        let doc = DocumentId::new_unchecked("doc_1");
        let mut c = chunk(3, 2);
        c.contextual_prefix = Some("context".to_string());
        c.metadata = serde_json::json!({"chunkIndex": 3, "documentTitle": "t"});
        let stmt = create_statement(&doc, &c, 3, Some("bge-m3"));

        assert!(stmt.starts_with("CREATE document_chunk SET "));
        assert!(stmt.ends_with(';'));
        assert!(stmt.contains("id = \"doc_1_3\""));
        assert!(stmt.contains("document_id = \"doc_1\""));
        assert!(stmt.contains("content = \"body 3\""));
        assert!(stmt.contains("chunk_index = 3"));
        assert!(stmt.contains("embedding = [0.5, 0.5]"));
        assert!(stmt.contains("metadata = {\"chunkIndex\":3,\"documentTitle\":\"t\"}"));
        assert!(stmt.contains("created_at = time::now()"));
        assert!(stmt.contains("contextual_prefix = \"context\""));
        assert!(stmt.contains("embedding_model = \"bge-m3\""));
    }

    #[test]
    fn optional_columns_are_omitted_not_nulled() {
        // SurrealDB rejects a literal NULL for `option<string>`, so an absent
        // value must not appear in the assignment list at all.
        let doc = DocumentId::new_unchecked("doc_1");
        let stmt = create_statement(&doc, &chunk(0, 2), 0, None);
        assert!(!stmt.contains("contextual_prefix"));
        assert!(!stmt.contains("embedding_model"));
        assert!(!stmt.to_lowercase().contains("null"));
    }

    #[test]
    fn content_cannot_close_the_statement() {
        let doc = DocumentId::new_unchecked("doc_1");
        let mut c = chunk(0, 2);
        c.content = "x\"; DELETE document_chunk; --".to_string();
        let stmt = create_statement(&doc, &c, 0, None);
        assert_eq!(stmt.matches("CREATE document_chunk").count(), 1);
        assert!(stmt.contains(r#"content = "x\"; DELETE document_chunk; --""#));
    }
}
