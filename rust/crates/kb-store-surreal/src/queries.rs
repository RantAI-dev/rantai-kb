//! SurrealQL query templates and sanitisation helpers.

use kb_ports::PortError;

/// Vector similarity search template.
pub const VECTOR_SEARCH_QUERY: &str = "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix, \
     vector::similarity::cosine(embedding, {embedding}) AS similarity \
     FROM document_chunk \
     WHERE document_id IN {doc_ids} \
     ORDER BY similarity DESC \
     LIMIT {limit};";

/// BM25 full-text search template. `$q` is bound via query parameter, never spliced.
pub const BM25_SEARCH_QUERY: &str = "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix, \
     search::score(0) AS score \
     FROM document_chunk \
     WHERE content @@ $q AND document_id IN {doc_ids} \
     ORDER BY score DESC, id ASC \
     LIMIT {limit};";

/// Unscoped BM25 full-text search template matching TypeScript `bm25Search`.
pub const BM25_SEARCH_QUERY_UNSCOPED: &str = "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix, \
     search::score(0) AS score \
     FROM document_chunk \
     WHERE content @@ $q \
     ORDER BY score DESC, id ASC \
     LIMIT {limit};";

/// Neighbour chunk lookup template.
pub const NEIGHBOURS_QUERY: &str = "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix \
     FROM document_chunk \
     WHERE document_id IN {doc_ids} AND chunk_index IN {indices};";

/// Figure lookup template.
pub const FIGURES_QUERY: &str = "SELECT id, document_id, content, chunk_index, metadata, contextual_prefix \
     FROM document_chunk \
     WHERE document_id IN {doc_ids} AND metadata.chunkType = 'figure';";

/// Validate that a document id strictly matches `^[A-Za-z0-9_-]{1,64}$`.
pub fn validate_document_id(id: &str) -> Result<(), PortError> {
    if id.is_empty() || id.len() > 64 {
        return Err(PortError::Malformed(format!(
            "invalid document id length: {id}"
        )));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(PortError::Malformed(format!(
            "invalid document id characters: {id}"
        )));
    }
    Ok(())
}

/// Render a list of document ids as a SurrealQL array of string literals after validation.
pub fn surql_id_list(ids: &[String]) -> Result<String, PortError> {
    for id in ids {
        validate_document_id(id)?;
    }
    let parts: Vec<String> = ids.iter().map(|id| format!("\"{id}\"")).collect();
    Ok(format!("[{}]", parts.join(", ")))
}

/// Render an array of u32 integers as a SurrealQL array literal.
#[must_use]
pub fn surql_u32_array(nums: &[u32]) -> String {
    let parts: Vec<String> = nums.iter().map(|n| n.to_string()).collect();
    format!("[{}]", parts.join(", "))
}

/// Render an array of f32 floats as a SurrealQL array literal.
#[must_use]
pub fn surql_f32_array(v: &[f32]) -> String {
    let parts: Vec<String> = v.iter().map(|x| format!("{x}")).collect();
    format!("[{}]", parts.join(", "))
}

/// Render a string as a SurrealQL double-quoted literal.
///
/// `serde_json` escaping is a superset of SurrealQL's inside a quoted string,
/// and it is the only escaping in this module that has been reasoned about
/// against arbitrary input: a `"` becomes `\"` and a newline becomes `\n`, so
/// no caller-supplied text can terminate the literal and append a statement.
#[must_use]
pub fn surql_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

/// Render a JSON value as a SurrealQL literal.
#[must_use]
pub fn surql_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn valid_ids_pass() {
        assert!(validate_document_id("doc_123-abc").is_ok());
        assert!(validate_document_id("a").is_ok());
    }

    #[test]
    fn invalid_ids_fail() {
        assert!(validate_document_id("").is_err());
        assert!(validate_document_id("has space").is_err());
        assert!(validate_document_id("semi;colon").is_err());
        assert!(validate_document_id("quote\"").is_err());
        let too_long = "a".repeat(65);
        assert!(validate_document_id(&too_long).is_err());
    }

    #[test]
    fn id_list_renders_properly() {
        let ids = vec!["doc1".to_string(), "doc2".to_string()];
        let s = surql_id_list(&ids).unwrap();
        assert_eq!(s, "[\"doc1\", \"doc2\"]");
    }
}
