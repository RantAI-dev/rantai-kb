//! The SurrealDB DDL the KB needs — the Rust half of
//! `scripts/apply-vector-schema.ts`.
//!
//! Every statement is `IF NOT EXISTS`, so applying it on every deploy is what
//! the TypeScript script does and stays safe. The embedding dimension is the
//! one thing that is *not* idempotent: SurrealDB fixes the vector width when
//! the index is defined, and a width that disagrees with `KB_EMBEDDING_DIM`
//! fails at insert time with a dimension error rather than degrading quietly.

use kb_ports::PortError;

use crate::client::SurrealChunkStore;

/// The statements, in order, for an embedding space of `dim` dimensions.
#[must_use]
pub fn schema_statements(dim: u32) -> Vec<String> {
    vec![
        "DEFINE TABLE IF NOT EXISTS document_chunk SCHEMALESS;".to_string(),
        "DEFINE FIELD IF NOT EXISTS document_id ON document_chunk TYPE string;".to_string(),
        "DEFINE FIELD IF NOT EXISTS content ON document_chunk TYPE string;".to_string(),
        "DEFINE FIELD IF NOT EXISTS chunk_index ON document_chunk TYPE int;".to_string(),
        "DEFINE FIELD IF NOT EXISTS embedding ON document_chunk TYPE array<float>;".to_string(),
        "DEFINE FIELD IF NOT EXISTS contextual_prefix ON document_chunk TYPE option<string>;".to_string(),
        "DEFINE FIELD IF NOT EXISTS embedding_model ON document_chunk TYPE option<string>;".to_string(),
        "DEFINE INDEX IF NOT EXISTS document_id_idx ON document_chunk FIELDS document_id;".to_string(),
        "DEFINE ANALYZER IF NOT EXISTS kb_en TOKENIZERS class FILTERS lowercase, snowball(english);".to_string(),
        "DEFINE INDEX IF NOT EXISTS content_search_idx ON document_chunk FIELDS content SEARCH ANALYZER kb_en BM25(1.2, 0.75) HIGHLIGHTS;".to_string(),
        format!("DEFINE INDEX IF NOT EXISTS embedding_idx ON document_chunk FIELDS embedding HNSW DIMENSION {dim} DIST COSINE;"),
    ]
}

/// Apply the schema, one statement per request.
///
/// `SurrealChunkStore::query` fails on any statement whose envelope status is
/// not `OK`, which is the same rule the TypeScript script enforces: a DDL
/// statement that errors must stop the run rather than be logged past.
///
/// # Errors
///
/// Propagates the first failing statement's error.
pub async fn apply_schema(store: &SurrealChunkStore, dim: u32) -> Result<usize, PortError> {
    let statements = schema_statements(dim);
    for sql in &statements {
        store.query(sql).await?;
    }
    Ok(statements.len())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_statement_is_idempotent() {
        // A statement without IF NOT EXISTS fails on the second deploy, which
        // is the case the container entrypoint hits on every restart.
        for sql in schema_statements(8) {
            assert!(sql.contains("IF NOT EXISTS"), "not idempotent: {sql}");
        }
    }

    #[test]
    fn the_vector_index_carries_the_requested_dimension() {
        let statements = schema_statements(1536);
        let index = statements
            .iter()
            .find(|s| s.contains("embedding_idx"))
            .expect("vector index");
        assert!(index.contains("DIMENSION 1536"), "{index}");
        assert!(index.contains("HNSW"), "{index}");
        assert!(index.contains("DIST COSINE"), "{index}");
    }

    #[test]
    fn the_analyzer_matches_the_typescript_definition() {
        let statements = schema_statements(8);
        assert!(statements.iter().any(|s| s == "DEFINE ANALYZER IF NOT EXISTS kb_en TOKENIZERS class FILTERS lowercase, snowball(english);"));
        assert!(statements.iter().any(|s| s.contains("BM25(1.2, 0.75)")));
    }

    #[test]
    fn the_statement_count_matches_the_typescript_script() {
        assert_eq!(schema_statements(8).len(), 11);
    }
}
