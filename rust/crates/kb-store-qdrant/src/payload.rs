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
    ArmRanks, ArmScores, Candidate, ChunkId, ChunkMeta, ChunkRange, ChunkType, DocumentId,
    DocumentScope, IndexUnit, TenantId, UnitRange,
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
/// Character span fields, written on chunks ingested with `ING-13`. They are
/// also mirrored inside [`KEY_METADATA`]; old chunks carry neither.
pub const KEY_CHAR_START: &str = "charStart";
pub const KEY_CHAR_END: &str = "charEnd";
pub const KEY_OVERLAP_PREFIX_LEN: &str = "overlapPrefixLen";

/// Where a chunk sits in the chunker's normalised source. Every field is
/// `None` for chunks written before spans existed, never a misleading `0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkSpans {
    pub char_start: Option<u32>,
    pub char_end: Option<u32>,
    pub overlap_prefix_len: Option<u32>,
}

/// Read spans from the opaque metadata JSON the engine writes.
#[must_use]
pub fn spans_from_metadata(metadata: &serde_json::Value) -> ChunkSpans {
    let u32_at = |key: &str| {
        metadata
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    ChunkSpans {
        char_start: u32_at(KEY_CHAR_START),
        char_end: u32_at(KEY_CHAR_END),
        overlap_prefix_len: u32_at(KEY_OVERLAP_PREFIX_LEN),
    }
}

/// Read the mirrored span fields from a Qdrant payload. Missing keys (and
/// values that are not unsigned 32-bit integers) read as `None`.
#[must_use]
pub fn spans_from_payload(payload: &std::collections::HashMap<String, Value>) -> ChunkSpans {
    let u32_at = |key: &str| integer(payload, key).and_then(|n| u32::try_from(n).ok());
    ChunkSpans {
        char_start: u32_at(KEY_CHAR_START),
        char_end: u32_at(KEY_CHAR_END),
        overlap_prefix_len: u32_at(KEY_OVERLAP_PREFIX_LEN),
    }
}

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
    KEY_CHAR_START,
    KEY_CHAR_END,
    KEY_OVERLAP_PREFIX_LEN,
];

/// The scope filter: this tenant **and** one of these documents.
///
/// Both halves are required. The tenant alone is not a scope (a tenant has many
/// knowledge bases and categories the caller may not see), and the document list
/// alone is not one either (document ids are tenant-unique only inside a
/// tenant).
///
/// In Ids mode the document list is a `match any` keyword list. Qdrant pushes it
/// into the payload index rather than scanning; `docs/eval-baseline.md` §6
/// measures the worst case (every document in a 2,933-document corpus) against a
/// 10-document scope. In Predicate mode (SEC-2a) the list is replaced by the
/// tenant / knowledge-base / category conditions and a narrower `document_id`
/// condition only when the scope was explicitly narrowed by `documentIds`.
/// The index filter is an optimisation: the engine verifies every candidate
/// against Postgres before it can be returned.
#[must_use]
pub fn scope_filter(scope: &DocumentScope) -> Filter {
    let mut filter = if let Some(predicate) = scope.predicate() {
        let mut conditions = vec![Condition::matches(
            KEY_TENANT_ID,
            MatchValue::Keyword(scope.tenant().as_str().to_string()),
        )];
        if let Some(kb_ids) = &predicate.kb_ids {
            conditions.push(any_of(
                KEY_KNOWLEDGE_BASE_ID,
                kb_ids.iter().map(|kb| kb.0.clone()).collect(),
            ));
        }
        if let Some(category) = &predicate.category {
            conditions.push(Condition::matches(
                KEY_CATEGORY,
                MatchValue::Keyword(category.clone()),
            ));
        }
        if let Some(ids) = scope.restricted_ids() {
            conditions.push(any_of(
                KEY_DOCUMENT_ID,
                ids.iter().map(|id| id.as_str().to_string()).collect(),
            ));
        }
        Filter::must(conditions)
    } else {
        let documents: Vec<String> = scope
            .document_ids()
            .iter()
            .map(|d| d.as_str().to_string())
            .collect();
        document_filter(scope.tenant(), &documents)
    };
    append_unit_range(&mut filter, scope.unit_range());
    filter
}

/// Add a predicate scope's knowledge-base and category conditions to a filter
/// that already names its tenant and one document. The document-id condition is
/// the primary fence; these repeat the coarse conditions so an index that
/// returns a stale payload cannot widen a predicate search before the verify.
fn append_predicate_conditions(filter: &mut Filter, scope: &DocumentScope) {
    if let Some(predicate) = scope.predicate() {
        if let Some(kb_ids) = &predicate.kb_ids {
            filter.must.push(any_of(
                KEY_KNOWLEDGE_BASE_ID,
                kb_ids.iter().map(|kb| kb.0.clone()).collect(),
            ));
        }
        if let Some(category) = &predicate.category {
            filter.must.push(Condition::matches(
                KEY_CATEGORY,
                MatchValue::Keyword(category.clone()),
            ));
        }
    }
}

/// Add the RET-15 range as index conditions, not as a post-filter.
///
/// Both halves are added when both are set, so top-k is spent only on units
/// inside the requested part. The integer `page` and `chunkIndex` payload
/// indexes (ING-13) make these ranges index lookups.
fn append_unit_range(filter: &mut Filter, range: Option<UnitRange>) {
    let Some(range) = range else {
        return;
    };
    if let Some((lo, hi)) = range.pages {
        filter.must.push(Condition::range(
            KEY_PAGE,
            Range {
                gte: Some(f64::from(lo)),
                lte: Some(f64::from(hi)),
                ..Default::default()
            },
        ));
    }
    if let Some((lo, hi)) = range.chunks {
        filter.must.push(Condition::range(
            KEY_CHUNK_INDEX,
            Range {
                gte: Some(f64::from(lo)),
                lte: Some(f64::from(hi)),
                ..Default::default()
            },
        ));
    }
}

/// `tenant == t AND document_id in documents AND chunk_type == "figure"`, plus
/// the caller's unit range when one is set.
#[must_use]
pub fn figures_filter(scope: &DocumentScope, documents: &[String]) -> Filter {
    let mut filter = Filter::must([
        Condition::matches(
            KEY_TENANT_ID,
            MatchValue::Keyword(scope.tenant().as_str().to_string()),
        ),
        Condition::matches(
            KEY_DOCUMENT_ID,
            MatchValue::Keywords(RepeatedStrings {
                strings: documents.to_vec(),
            }),
        ),
        Condition::matches(KEY_CHUNK_TYPE, MatchValue::Keyword("figure".to_string())),
    ]);
    append_predicate_conditions(&mut filter, scope);
    append_unit_range(&mut filter, scope.unit_range());
    filter
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

/// `tenant == t AND document_id == d` plus the integer range conditions of a
/// [`ChunkRange`] (`EVAL-9`). The vector-free `/content` read uses this so a
/// page or chunk selection is answered from the `page` / `chunkIndex` payload
/// indexes rather than by scrolling the whole document.
///
/// Both halves are required, exactly as in [`one_document_filter`]: the tenant
/// alone is not scope and the range alone could cross documents. This mirrors
/// the scope check of `list_chunk_meta`, which is repeated by the caller.
#[must_use]
pub fn chunk_meta_range_filter(
    scope: &DocumentScope,
    document: &DocumentId,
    range: ChunkRange,
) -> Filter {
    let mut filter = one_document_filter(scope.tenant(), document);
    if let Some((lo, hi)) = range.pages {
        filter.must.push(Condition::range(
            KEY_PAGE,
            Range {
                gte: Some(f64::from(lo)),
                lte: Some(f64::from(hi)),
                ..Default::default()
            },
        ));
    }
    if let Some((lo, hi)) = range.chunks {
        filter.must.push(Condition::range(
            KEY_CHUNK_INDEX,
            Range {
                gte: Some(f64::from(lo)),
                lte: Some(f64::from(hi)),
                ..Default::default()
            },
        ));
    }
    filter
}

/// `tenant == t AND document_id == d AND lo <= chunk_index <= hi`.
///
/// A numeric range, not a set membership: a document's neighbours are a
/// contiguous index range, and an indexed integer range is what Qdrant can
/// answer from the `chunk_index` index without touching the rest of the
/// document.
#[must_use]
pub fn chunk_range_filter(
    scope: &DocumentScope,
    document: &DocumentId,
    lo: u32,
    hi: u32,
) -> Filter {
    let mut filter = Filter::must([
        Condition::matches(
            KEY_TENANT_ID,
            MatchValue::Keyword(scope.tenant().as_str().to_string()),
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
    ]);
    append_predicate_conditions(&mut filter, scope);
    append_unit_range(&mut filter, scope.unit_range());
    filter
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

/// `tenant == t AND document_id == d AND page IN (there, pages)`.
///
/// Every half is required, exactly like [`chunk_range_filter`]: the tenant
/// alone is not scope, and the page list alone could cross into another
/// document. The caller (the tree arm) only passes documents it read from a
/// [`DocumentScope`], and this filter repeats the tenant/document condition so
/// a bug in that caller cannot widen the read. The caller's unit range is
/// intersected here too, so the tree arm cannot fetch a page outside the part
/// of the document being searched.
#[must_use]
pub fn pages_filter(scope: &DocumentScope, document: &DocumentId, pages: &[u32]) -> Filter {
    let mut filter = Filter::must([
        Condition::matches(
            KEY_TENANT_ID,
            MatchValue::Keyword(scope.tenant().as_str().to_string()),
        ),
        Condition::matches(
            KEY_DOCUMENT_ID,
            MatchValue::Keyword(document.as_str().to_string()),
        ),
        Condition::matches(
            KEY_PAGE,
            MatchValue::Integers(qdrant_client::qdrant::RepeatedIntegers {
                integers: pages.iter().map(|page| i64::from(*page)).collect(),
            }),
        ),
    ]);
    append_predicate_conditions(&mut filter, scope);
    append_unit_range(&mut filter, scope.unit_range());
    filter
}

/// The payload of one unit, as Qdrant stores it.
#[must_use]
pub fn unit_payload(unit: &IndexUnit) -> Payload {
    let mut payload = Payload::new();
    payload.insert(KEY_UNIT_ID, unit.id.as_str());
    payload.insert(KEY_TENANT_ID, unit.tenant.as_str());
    payload.insert(KEY_DOCUMENT_ID, unit.document_id.as_str());
    // A document may sit in several knowledge bases. Ingest writes the whole
    // list into `metadata.knowledgeBaseIds`; the singular `IndexUnit` field is
    // kept for callers that have only one. Qdrant stores a keyword array under
    // the same key, which `any_of` matches in either shape.
    let kb_ids = knowledge_base_ids_from_metadata(&unit.metadata);
    if !kb_ids.is_empty() {
        payload.insert(KEY_KNOWLEDGE_BASE_ID, kb_ids);
    } else if let Some(kb) = &unit.knowledge_base_id {
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
    // `ING-13`: mirror the engine's metadata spans as first-class payload
    // values, just like `page` and `chunkIndex`, so a reader can ask for them
    // without walking the opaque metadata object. Absent stays absent.
    let spans = spans_from_metadata(&unit.metadata);
    if let Some(start) = spans.char_start {
        payload.insert(KEY_CHAR_START, i64::from(start));
    }
    if let Some(end) = spans.char_end {
        payload.insert(KEY_CHAR_END, i64::from(end));
    }
    if let Some(overlap) = spans.overlap_prefix_len {
        payload.insert(KEY_OVERLAP_PREFIX_LEN, i64::from(overlap));
    }
    payload.insert(KEY_METADATA, unit.metadata.clone());
    payload
}

/// The payload written by a KB-membership repair (SEC-2b).
///
/// A Qdrant `set_payload` replaces only the keys it names, so this leaves the
/// tenant, document, category and text fields alone. An empty list is written
/// deliberately for a document that was moved out of every KB: an `any_of`
/// over an empty list matches nothing, which fails closed until an operator
/// re-enables a broader scope.
#[must_use]
pub fn knowledge_base_payload(kb_ids: &[String]) -> Payload {
    let mut payload = Payload::new();
    payload.insert(KEY_KNOWLEDGE_BASE_ID, kb_ids.to_vec());
    payload
}

/// Every knowledge base id the engine wrote for this unit. Old units (and the
/// stateless path) carry none; the singular `IndexUnit::knowledge_base_id` is
/// then the only source.
#[must_use]
pub fn knowledge_base_ids_from_metadata(metadata: &serde_json::Value) -> Vec<String> {
    metadata
        .get("knowledgeBaseIds")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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

/// Read vector-free chunk metadata from a scrolled point's payload. The
/// mirrored `charStart`/`charEnd`/`overlapPrefixLen` fields are preferred;
/// older payloads that only carry them inside `metadata` still read correctly.
/// A point that is not one of ours (no tenant or document id) is skipped.
#[must_use]
pub fn chunk_meta_from_payload(
    payload: &std::collections::HashMap<String, Value>,
) -> Option<ChunkMeta> {
    string(payload, KEY_TENANT_ID)?;
    string(payload, KEY_DOCUMENT_ID)?;
    let metadata = payload
        .get(KEY_METADATA)
        .map_or(serde_json::Value::Null, json_of);
    let mirrored = spans_from_payload(payload);
    let nested = spans_from_metadata(&metadata);
    Some(ChunkMeta {
        chunk_index: integer(payload, KEY_CHUNK_INDEX).unwrap_or(0) as u32,
        page: integer(payload, KEY_PAGE).map(|value| value as u32),
        section: metadata
            .get("section")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        content: string(payload, KEY_CONTENT).unwrap_or_default(),
        char_start: mirrored.char_start.or(nested.char_start),
        char_end: mirrored.char_end.or(nested.char_end),
        overlap_prefix_len: mirrored.overlap_prefix_len.or(nested.overlap_prefix_len),
        chunk_type: ChunkType::from_storage(string(payload, KEY_CHUNK_TYPE).as_deref().or_else(
            || {
                metadata
                    .get("chunkType")
                    .and_then(serde_json::Value::as_str)
            },
        )),
        asset_key: metadata
            .get("assetKey")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
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
    use kb_engine::fakes::{make_fake_predicate_scope, make_fake_scope};
    use kb_ports::{DocumentFilter, KnowledgeBaseId, PredicateSpec, VerifySpec};

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
    fn chunk_spans_round_trip_through_the_payload() {
        let mut stored = unit("tenant-a", "doc-a1", 2);
        stored.metadata = serde_json::json!({"documentTitle": "d", "charStart": 120, "charEnd": 400, "overlapPrefixLen": 24});
        let payload = unit_payload(&stored);
        let map: std::collections::HashMap<String, Value> = payload.clone().into();
        assert_eq!(integer(&map, KEY_CHAR_START), Some(120));
        assert_eq!(integer(&map, KEY_CHAR_END), Some(400));
        assert_eq!(integer(&map, KEY_OVERLAP_PREFIX_LEN), Some(24));
        let spans = spans_from_payload(&map);
        assert_eq!(spans.char_start, Some(120));
        assert_eq!(spans.char_end, Some(400));
        assert_eq!(spans.overlap_prefix_len, Some(24));
        let back = candidate_from(&map, ArmScores::default(), ArmRanks::default())
            .expect("payload round-trip");
        assert_eq!(spans_from_metadata(&back.metadata), spans);
        assert_eq!(back.metadata, stored.metadata);
    }

    #[test]
    fn old_chunks_have_no_spans_not_zero() {
        // Chunks written before ING-13 carry no span keys: they must read as
        // absent, never as a span of 0 that would claim to be a real offset.
        let stored = unit("tenant-a", "doc-a1", 2);
        let payload = unit_payload(&stored);
        let map: std::collections::HashMap<String, Value> = payload.clone().into();
        assert!(!map.contains_key(KEY_CHAR_START));
        assert!(!map.contains_key(KEY_CHAR_END));
        assert!(!map.contains_key(KEY_OVERLAP_PREFIX_LEN));
        assert_eq!(spans_from_payload(&map), ChunkSpans::default());
        let back = candidate_from(&map, ArmScores::default(), ArmRanks::default())
            .expect("payload round-trip");
        assert_eq!(spans_from_metadata(&back.metadata), ChunkSpans::default());
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
        assert!(!map.contains_key(KEY_CHAR_START));
        assert!(!map.contains_key(KEY_CHAR_END));
        assert!(!map.contains_key(KEY_OVERLAP_PREFIX_LEN));
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
    fn a_pages_filter_names_tenant_document_and_page() {
        let scope = test_scope(&["doc-a1"]);
        let document = DocumentId::new_unchecked("doc-a1");
        let filter = pages_filter(&scope, &document, &[3, 4]);
        assert_eq!(filter.must.len(), 3, "tenant, document and pages");
    }

    #[test]
    fn a_unit_range_adds_index_range_conditions_to_every_search_filter() {
        // Every path that queries the chunk store builds its filter here. All
        // five must carry the range as conditions, not as a post-filter.
        let scope = test_scope(&["doc-a1"]).with_unit_range(UnitRange {
            pages: Some((1, 5)),
            chunks: Some((40, 57)),
        });
        let document = DocumentId::new_unchecked("doc-a1");
        let filters = [
            scope_filter(&scope),
            scoped_ids_filter(&scope, &[ChunkId::new_unchecked("doc-a1_3")]),
            figures_filter(&scope, &["doc-a1".to_string()]),
            pages_filter(&scope, &document, &[3]),
            chunk_range_filter(&scope, &document, 40, 57),
        ];
        for filter in filters {
            let debug = format!("{filter:?}");
            assert!(debug.contains(KEY_PAGE), "no page range in {debug}");
            assert!(debug.contains(KEY_CHUNK_INDEX), "no chunk range in {debug}");
        }
    }

    #[test]
    fn a_chunk_meta_range_filter_has_both_integer_range_conditions() {
        // EVAL-9: the vector-free /content read is a range scroll, not a whole
        // document scroll. Both integer indexes must be named as conditions,
        // and the tenant/document fence must still be there.
        let scope = test_scope(&["doc-a1"]);
        let document = DocumentId::new_unchecked("doc-a1");
        let filter = chunk_meta_range_filter(
            &scope,
            &document,
            ChunkRange {
                chunks: Some((10, 20)),
                pages: Some((2, 4)),
            },
        );
        assert_eq!(filter.must.len(), 4, "tenant, document and two ranges");
        let debug = format!("{filter:?}");
        assert!(debug.contains(KEY_CHUNK_INDEX), "no chunk range: {debug}");
        assert!(debug.contains(KEY_PAGE), "no page range: {debug}");
        assert!(debug.contains(KEY_TENANT_ID), "no tenant: {debug}");
        assert!(debug.contains(KEY_DOCUMENT_ID), "no document: {debug}");
        // A chunk-only range must not smuggle in a page condition, or vice versa.
        let chunks_only = chunk_meta_range_filter(
            &scope,
            &document,
            ChunkRange {
                chunks: Some((1, 2)),
                pages: None,
            },
        );
        let debug = format!("{chunks_only:?}");
        assert!(debug.contains(KEY_CHUNK_INDEX));
        assert!(!debug.contains(KEY_PAGE), "unwanted page range: {debug}");
    }

    #[test]
    fn a_page_only_range_adds_only_the_page_condition() {
        let scope = test_scope(&["doc-a1"]).with_unit_range(UnitRange {
            pages: Some((1, 5)),
            chunks: None,
        });
        let debug = format!("{:?}", scope_filter(&scope));
        assert!(debug.contains(KEY_PAGE), "no page range in {debug}");
        assert!(
            !debug.contains(KEY_CHUNK_INDEX),
            "unwanted chunk range: {debug}"
        );
    }

    #[test]
    fn no_unit_range_leaves_the_filter_untouched() {
        let scope = test_scope(&["doc-a1"]);
        let filter = scope_filter(&scope);
        assert_eq!(filter.must.len(), 2, "tenant and documents only");
        let debug = format!("{filter:?}");
        assert!(!debug.contains(KEY_PAGE));
        assert!(!debug.contains(KEY_CHUNK_INDEX));
    }

    fn predicate_scope(kb_ids: Option<Vec<&str>>, category: Option<&str>) -> DocumentScope {
        make_fake_predicate_scope(
            TenantId::new_unchecked("tenant-a"),
            PredicateSpec {
                kb_ids: kb_ids.map(|ids| {
                    ids.into_iter()
                        .map(KnowledgeBaseId::new_unchecked)
                        .collect()
                }),
                category: category.map(str::to_string),
                verify: VerifySpec {
                    tenant: TenantId::new_unchecked("tenant-a"),
                    filter: DocumentFilter::default(),
                    restricted_ids: None,
                },
            },
        )
    }

    #[test]
    fn a_predicate_filter_has_no_id_list_and_names_tenant_kb_and_category() {
        let scope = predicate_scope(Some(vec!["kb-1", "kb-2"]), Some("cat-1"));
        let filter = scope_filter(&scope);
        let debug = format!("{filter:?}");
        assert!(!debug.contains(KEY_DOCUMENT_ID), "{debug}");
        assert!(debug.contains(KEY_TENANT_ID), "{debug}");
        assert!(debug.contains(KEY_KNOWLEDGE_BASE_ID), "{debug}");
        assert!(debug.contains("kb-1"), "{debug}");
        assert!(debug.contains("kb-2"), "{debug}");
        assert!(debug.contains(KEY_CATEGORY), "{debug}");
        assert!(debug.contains("cat-1"), "{debug}");
    }

    #[test]
    fn a_narrowed_predicate_filter_adds_the_requested_documents_back() {
        let scope = predicate_scope(None, None).narrowed_to(&[DocumentId::new_unchecked("doc-a")]);
        let debug = format!("{:?}", scope_filter(&scope));
        assert!(debug.contains(KEY_DOCUMENT_ID), "{debug}");
        assert!(debug.contains("doc-a"), "{debug}");
        assert!(!debug.contains(KEY_KNOWLEDGE_BASE_ID), "{debug}");
    }

    #[test]
    fn knowledge_base_ids_from_metadata_cover_a_document_in_several_kbs() {
        let mut unit = unit("tenant-a", "doc-a", 0);
        unit.metadata = serde_json::json!({"knowledgeBaseIds": ["kb-1", "kb-2"]});
        let payload = unit_payload(&unit);
        let map: std::collections::HashMap<String, Value> = payload.into();
        assert_eq!(
            knowledge_base_ids_from_metadata(&unit.metadata),
            vec!["kb-1", "kb-2"]
        );
        assert!(map.contains_key(KEY_KNOWLEDGE_BASE_ID));
    }

    #[test]
    fn a_repair_payload_replaces_only_the_kb_key() {
        let payload = knowledge_base_payload(&["kb-2".to_string(), "kb-3".to_string()]);
        let map: std::collections::HashMap<String, Value> = payload.into();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(KEY_KNOWLEDGE_BASE_ID));
        assert!(!map.contains_key(KEY_TENANT_ID));

        let empty: std::collections::HashMap<String, Value> = knowledge_base_payload(&[]).into();
        assert!(
            empty.contains_key(KEY_KNOWLEDGE_BASE_ID),
            "clear, do not omit"
        );
    }

    #[test]
    fn every_payload_key_is_listed_for_the_index_pass() {
        for key in [
            KEY_TENANT_ID,
            KEY_DOCUMENT_ID,
            KEY_KNOWLEDGE_BASE_ID,
            KEY_CATEGORY,
            KEY_CHUNK_INDEX,
            KEY_PAGE,
            KEY_CHAR_START,
            KEY_CHAR_END,
            KEY_OVERLAP_PREFIX_LEN,
        ] {
            assert!(PAYLOAD_KEYS.contains(&key), "{key}");
        }
    }
}
