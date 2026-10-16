//! The payload layout and the scope filter.
//!
//! Every filter this adapter sends is built here, so "there is no code path
//! that queries Qdrant without the scope filter" is a property of one file
//! rather than a convention spread over the call sites.
//!
//! # Why the payload carries the engine's own ids
//!
//! Qdrant point ids are UUIDv5 (see [`crate::ids`]) and are not reversible, but
//! [`RetrievalIndex::fetch`] is handed [`ChunkId`]s and must answer with
//! [`ChunkId`]s. So the engine's unit id is stored verbatim in the payload and
//! is what `Candidate::unit_id` is read back from. The UUID is an internal
//! storage detail; it never leaks into engine code.
//!
//! [`RetrievalIndex::fetch`]: kb_ports::RetrievalIndex::fetch

use kb_ports::{
    ArmRanks, ArmScores, Candidate, ChunkId, ChunkType, DocumentId, DocumentScope, IndexUnit,
    TenantId,
};
use qdrant_client::Payload;
use qdrant_client::qdrant::{
    Condition, Filter, Range, RepeatedStrings, Value, r#match::MatchValue, value,
};

/// The engine's unit id, stored verbatim. See the module docs.
pub const KEY_UNIT_ID: &str = "unit_id";
/// Keyword index, `is_tenant: true` — the multitenant anchor.
pub const KEY_TENANT_ID: &str = "tenant_id";
/// Keyword index, and the second half of every scope filter.
pub const KEY_DOCUMENT_ID: &str = "document_id";
/// Keyword index, for the future `kb_index`-free KB-scoped query.
pub const KEY_KNOWLEDGE_BASE_ID: &str = "knowledge_base_id";
/// Keyword index.
pub const KEY_CATEGORY: &str = "category";
/// Integer index, for neighbour windows and re-ingest bookkeeping.
pub const KEY_CHUNK_INDEX: &str = "chunk_index";
/// The page this chunk was printed on, when it has one.
pub const KEY_PAGE: &str = "page";
/// `text`, `table`, `figure`, …
pub const KEY_CHUNK_TYPE: &str = "chunk_type";
/// The unit's text, returned verbatim by `search` and `fetch`.
pub const KEY_CONTENT: &str = "content";
/// What was embedded (`title > section path > chunk`).
pub const KEY_CONTEXTUAL_PREFIX: &str = "contextual_prefix";
/// The caller's opaque metadata JSON.
pub const KEY_METADATA: &str = "metadata";

/// Every payload key, for the payload-index pass and for documentation.
pub const PAYLOAD_KEYS: &[&str] = &[
    KEY_UNIT_ID,
    KEY_TENANT_ID,
    KEY_DOCUMENT_ID,
    KEY_KNOWLEDGE_BASE_ID,
    KEY_CATEGORY,
    KEY_CHUNK_INDEX,
    KEY_PAGE,
    KEY_CHUNK_TYPE,
    KEY_CONTENT,
    KEY_CONTEXTUAL_PREFIX,
    KEY_METADATA,
];

/// The scope filter: this tenant **and** one of these documents.
///
/// Both halves are required. The tenant alone is not a scope (a tenant has many
/// knowledge bases and categories the caller may not see), and the document list
/// alone is not one either (document ids are tenant-unique only inside a
/// tenant).
///
/// The document list is a `match any` keyword list. Qdrant pushes it into the
/// payload index rather than scanning; `docs/eval-baseline.md` §6 measures the
/// worst case (every document in a 2,933-document corpus) against a 10-document
/// scope.
#[must_use]
pub fn scope_filter(scope: &DocumentScope) -> Filter {
    let documents: Vec<String> = scope
        .document_ids()
        .iter()
        .map(|d| d.as_str().to_string())
        .collect();
    document_filter(scope.tenant(), &documents)
}

/// `tenant == t AND document_id in documents`.
#[must_use]
pub fn document_filter(tenant: &TenantId, documents: &[String]) -> Filter {
    Filter::must([
        Condition::matches(
            KEY_TENANT_ID,
            MatchValue::Keyword(tenant.as_str().to_string()),
        ),
        Condition::matches(
            KEY_DOCUMENT_ID,
            MatchValue::Keywords(RepeatedStrings {
                strings: documents.to_vec(),
            }),
        ),
    ])
}

/// `tenant == t AND document_id == d`. The delete and neighbour-expansion
/// filter; never used for a search, where the caller's document list applies.
#[must_use]
pub fn one_document_filter(tenant: &TenantId, document: &DocumentId) -> Filter {
    Filter::must([
        Condition::matches(
            KEY_TENANT_ID,
            MatchValue::Keyword(tenant.as_str().to_string()),
        ),
        Condition::matches(
            KEY_DOCUMENT_ID,
            MatchValue::Keyword(document.as_str().to_string()),
        ),
    ])
}

/// `tenant == t AND document_id == d AND lo <= chunk_index <= hi`.
///
/// A numeric range, not a set membership: a document's neighbours are a
/// contiguous index range, and an indexed integer range is what Qdrant can
/// answer from the `chunk_index` index without touching the rest of the
/// document.
#[must_use]
pub fn chunk_range_filter(tenant: &TenantId, document: &DocumentId, lo: u32, hi: u32) -> Filter {
    Filter::must([
        Condition::matches(
            KEY_TENANT_ID,
            MatchValue::Keyword(tenant.as_str().to_string()),
        ),
        Condition::matches(
            KEY_DOCUMENT_ID,
            MatchValue::Keyword(document.as_str().to_string()),
        ),
        Condition::range(
            KEY_CHUNK_INDEX,
            Range {
                gte: Some(f64::from(lo)),
                lte: Some(f64::from(hi)),
                ..Default::default()
            },
        ),
    ])
}

/// Ids belong to this scope: the scope filter **and** the id list.
///
/// `fetch` is handed engine [`ChunkId`]s, which are not point ids — a point id
/// is a UUIDv5 of (tenant, document, chunk index) and cannot be recomputed from
/// a chunk id alone. So the id list is matched against the `unit_id` keyword in
/// the payload, which is why that field is indexed like the others.
#[must_use]
pub fn scoped_ids_filter(scope: &DocumentScope, ids: &[ChunkId]) -> Filter {
    let wanted: Vec<String> = ids.iter().map(|id| id.as_str().to_string()).collect();
    let mut filter = scope_filter(scope);
    filter.must.push(Condition::matches(
        KEY_UNIT_ID,
        MatchValue::Keywords(RepeatedStrings { strings: wanted }),
    ));
    filter
}

/// The payload of one unit, as Qdrant stores it.
#[must_use]
pub fn unit_payload(unit: &IndexUnit) -> Payload {
    let mut payload = Payload::new();
    payload.insert(KEY_UNIT_ID, unit.id.as_str());
    payload.insert(KEY_TENANT_ID, unit.tenant.as_str());
    payload.insert(KEY_DOCUMENT_ID, unit.document_id.as_str());
    if let Some(kb) = &unit.knowledge_base_id {
        payload.insert(KEY_KNOWLEDGE_BASE_ID, kb.as_str());
    }
    if let Some(category) = &unit.category {
        payload.insert(KEY_CATEGORY, category.clone());
    }
    payload.insert(KEY_CHUNK_INDEX, i64::from(unit.chunk_index));
    if let Some(page) = unit.page {
        payload.insert(KEY_PAGE, i64::from(page));
    }
    payload.insert(KEY_CHUNK_TYPE, unit.chunk_type.as_storage());
    payload.insert(KEY_CONTENT, unit.content.clone());
    if let Some(prefix) = &unit.contextual_prefix {
        payload.insert(KEY_CONTEXTUAL_PREFIX, prefix.clone());
    }
    payload.insert(KEY_METADATA, unit.metadata.clone());
    payload
}

/// Read a unit back out of a point's payload, or `None` when the payload is not
/// one of ours (a point written by another tool into a shared collection).
#[must_use]
pub fn candidate_from(
    payload: &std::collections::HashMap<String, Value>,
    scores: ArmScores,
    ranks: ArmRanks,
) -> Option<Candidate> {
    let unit_id = string(payload, KEY_UNIT_ID)?;
    let document_id = string(payload, KEY_DOCUMENT_ID)?;
    let tenant = string(payload, KEY_TENANT_ID)?;
    Some(Candidate {
        unit_id: ChunkId::new_unchecked(unit_id),
        document_id: DocumentId::new_unchecked(document_id),
        tenant: TenantId::new_unchecked(tenant),
        chunk_index: integer(payload, KEY_CHUNK_INDEX).unwrap_or(0) as u32,
        page: integer(payload, KEY_PAGE).map(|v| v as u32),
        chunk_type: ChunkType::from_storage(string(payload, KEY_CHUNK_TYPE).as_deref()),
        content: string(payload, KEY_CONTENT).unwrap_or_default(),
        contextual_prefix: string(payload, KEY_CONTEXTUAL_PREFIX),
        metadata: payload
            .get(KEY_METADATA)
            .map_or(serde_json::Value::Null, json_of),
        scores,
        ranks,
    })
}

/// One payload value as a string, or `None` if it is missing or another type.
#[must_use]
pub fn string(payload: &std::collections::HashMap<String, Value>, key: &str) -> Option<String> {
    match payload.get(key)?.kind.as_ref()? {
        value::Kind::StringValue(s) => Some(s.clone()),
        _ => None,
    }
}

/// One payload value as an integer, or `None`.
#[must_use]
pub fn integer(payload: &std::collections::HashMap<String, Value>, key: &str) -> Option<i64> {
    match payload.get(key)?.kind.as_ref()? {
        value::Kind::IntegerValue(i) => Some(*i),
        value::Kind::DoubleValue(d) if d.is_finite() => Some(*d as i64),
        _ => None,
    }
}

/// One payload value as JSON, for the caller's opaque metadata.
#[must_use]
pub fn json_of(value: &Value) -> serde_json::Value {
    match value.kind.as_ref() {
        Some(value::Kind::NullValue(_)) | None => serde_json::Value::Null,
        Some(value::Kind::BoolValue(b)) => serde_json::Value::Bool(*b),
        Some(value::Kind::DoubleValue(d)) => serde_json::Value::from(*d),
        Some(value::Kind::IntegerValue(i)) => serde_json::Value::from(*i),
        Some(value::Kind::StringValue(s)) => serde_json::Value::String(s.clone()),
        Some(value::Kind::ListValue(list)) => {
            serde_json::Value::Array(list.values.iter().map(json_of).collect())
        }
        Some(value::Kind::StructValue(s)) => serde_json::Value::Object(
            s.fields
                .iter()
                .map(|(k, v)| (k.clone(), json_of(v)))
                .collect(),
        ),
    }
}

/// A `match any` over keyword values, written out for readability at call sites.
#[must_use]
pub fn any_of(field: &str, values: Vec<String>) -> Condition {
    Condition::matches(
        field,
        MatchValue::Keywords(RepeatedStrings { strings: values }),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use kb_engine::fakes::make_fake_scope;
    use kb_ports::KnowledgeBaseId;

    fn test_scope(docs: &[&str]) -> DocumentScope {
        make_fake_scope(
            TenantId::new_unchecked("tenant-a"),
            docs.iter().map(|d| DocumentId::new_unchecked(*d)).collect(),
        )
    }

    fn unit(tenant: &str, document: &str, chunk_index: u32) -> IndexUnit {
        IndexUnit {
            id: ChunkId::new_unchecked(format!("{document}_{chunk_index}")),
            document_id: DocumentId::new_unchecked(document),
            tenant: TenantId::new_unchecked(tenant),
            knowledge_base_id: Some(KnowledgeBaseId::new_unchecked("kb-1")),
            category: Some("kategori-1".to_string()),
            chunk_index,
            page: Some(7),
            chunk_type: ChunkType::Figure,
            content: "sebuah gambar".to_string(),
            contextual_prefix: Some("judul > bagian".to_string()),
            metadata: serde_json::json!({"documentTitle": "d", "n": 3}),
            dense: Vec::new(),
            sparse: kb_core::sparse::SparseVector::empty(),
        }
    }

    #[test]
    fn a_unit_round_trips_through_the_payload() {
        let stored = unit("tenant-a", "doc-a1", 2);
        let payload = unit_payload(&stored);
        let map: std::collections::HashMap<String, Value> = payload.clone().into();
        let back = candidate_from(&map, ArmScores::default(), ArmRanks::default());
        let back = back.expect("payload round-trip");
        assert_eq!(back.unit_id.as_str(), stored.id.as_str());
        assert_eq!(back.document_id.as_str(), "doc-a1");
        assert_eq!(back.tenant.as_str(), "tenant-a");
        assert_eq!(back.chunk_index, 2);
        assert_eq!(back.page, Some(7));
        assert_eq!(back.chunk_type, ChunkType::Figure);
        assert_eq!(back.content, "sebuah gambar");
        assert_eq!(back.contextual_prefix.as_deref(), Some("judul > bagian"));
        assert_eq!(back.metadata, stored.metadata);
    }

    #[test]
    fn a_payload_without_a_unit_id_is_not_one_of_ours() {
        let map =
            std::collections::HashMap::from([(KEY_DOCUMENT_ID.to_string(), Value::from("doc"))]);
        assert!(candidate_from(&map, ArmScores::default(), ArmRanks::default()).is_none());
    }

    #[test]
    fn optional_keys_are_absent_rather_than_null() {
        let mut stored = unit("t", "d", 0);
        stored.page = None;
        stored.contextual_prefix = None;
        stored.category = None;
        stored.knowledge_base_id = None;
        let payload = unit_payload(&stored);
        let map: std::collections::HashMap<String, Value> = payload.clone().into();
        assert!(!map.contains_key(KEY_PAGE));
        assert!(!map.contains_key(KEY_CONTEXTUAL_PREFIX));
        assert!(!map.contains_key(KEY_CATEGORY));
        assert!(!map.contains_key(KEY_KNOWLEDGE_BASE_ID));
        let back = candidate_from(&map, ArmScores::default(), ArmRanks::default());
        let back = back.expect("payload round-trip");
        assert_eq!(back.page, None);
        assert_eq!(back.contextual_prefix, None);
        assert_eq!(back.metadata, stored.metadata);
    }

    #[test]
    fn a_scope_filter_has_both_halves() {
        let scope = test_scope(&["doc-a1", "doc-a2"]);
        let filter = scope_filter(&scope);
        let conditions = filter.must;
        assert_eq!(conditions.len(), 2, "tenant and documents, never one");
    }

    #[test]
    fn an_empty_document_list_still_produces_a_filter() {
        // The caller short-circuits an empty scope before building one, but if
        // this ever stopped happening the filter would deny everything rather
        // than allow everything: an `any` with no values matches no point.
        let scope = test_scope(&[]);
        let filter = scope_filter(&scope);
        assert_eq!(filter.must.len(), 2);
    }

    #[test]
    fn every_payload_key_is_listed_for_the_index_pass() {
        for key in [
            KEY_TENANT_ID,
            KEY_DOCUMENT_ID,
            KEY_KNOWLEDGE_BASE_ID,
            KEY_CATEGORY,
            KEY_CHUNK_INDEX,
        ] {
            assert!(PAYLOAD_KEYS.contains(&key), "{key}");
        }
    }
}
