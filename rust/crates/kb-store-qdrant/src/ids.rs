//! Deterministic point ids.
//!
//! Qdrant point ids are UUIDs or integers. This adapter uses **UUIDv5** of
//! `(tenant, document id, chunk index)` so that:
//!
//! * re-ingesting the same document overwrites its points instead of
//!   duplicating them, and
//! * a re-ingest that produces *fewer* chunks leaves no stale tail, because the
//!   ids of the surviving chunks are unchanged and the tail is deleted by
//!   [`RetrievalIndex::delete_document`].
//!
//! The namespace is a constant and part of the on-disk contract: changing it
//! would make every stored point unreachable and silently re-index the corpus.
//! It is a v5-shaped UUID rather than a random v4 so that it is obvious it was
//! chosen for UUIDv5, not generated.
//!
//! [`RetrievalIndex::delete_document`]: kb_ports::RetrievalIndex::delete_document

use kb_ports::{DocumentId, IndexUnit, TenantId};
use uuid::Uuid;

/// Namespace for every chunk point id. Never change this.
pub const POINT_NAMESPACE: Uuid = Uuid::from_bytes([
    0x1b, 0x8e, 0x2f, 0x6a, 0x5c, 0x40, 0x5d, 0x9e, 0x9a, 0x2f, 0x0b, 0x4e, 0x64, 0x7c, 0x11, 0x35,
]);

/// The name hashed for a unit. `U+001F` (unit separator) joins the parts, which
/// cannot appear in a tenant or document id and therefore cannot make two
/// different triples hash the same string.
fn point_name(tenant: &TenantId, document_id: &DocumentId, chunk_index: u32) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{chunk_index}",
        tenant.as_str(),
        document_id.as_str()
    )
}

/// The point id for one chunk of one document in one tenant.
#[must_use]
pub fn point_id(tenant: &TenantId, document_id: &DocumentId, chunk_index: u32) -> Uuid {
    Uuid::new_v5(
        &POINT_NAMESPACE,
        point_name(tenant, document_id, chunk_index).as_bytes(),
    )
}

/// [`point_id`] for a unit the engine handed us.
#[must_use]
pub fn point_id_of(unit: &IndexUnit) -> Uuid {
    point_id(&unit.tenant, &unit.document_id, unit.chunk_index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_ports::ChunkId;

    fn unit(tenant: &str, document: &str, chunk_index: u32) -> IndexUnit {
        IndexUnit {
            id: ChunkId::new_unchecked(format!("{document}_{chunk_index}")),
            document_id: DocumentId::new_unchecked(document),
            tenant: TenantId::new_unchecked(tenant),
            knowledge_base_id: None,
            category: None,
            chunk_index,
            page: None,
            chunk_type: kb_ports::ChunkType::Text,
            content: String::new(),
            contextual_prefix: None,
            metadata: serde_json::Value::Null,
            dense: Vec::new(),
            sparse: kb_core::sparse::SparseVector::empty(),
        }
    }

    #[test]
    fn the_point_id_is_pinned() {
        // Pinned so a namespace or separator change is a failing test rather
        // than a silently re-indexed corpus.
        assert_eq!(
            point_id_of(&unit("tenant-a", "doc-a1", 0)).to_string(),
            "792cb109-63aa-5c96-9fdb-155b809e54ac",
            "update this constant only with a re-index plan"
        );
    }

    #[test]
    fn the_id_depends_on_tenant_document_and_chunk() {
        let a = point_id_of(&unit("t1", "d1", 0));
        assert_ne!(a, point_id_of(&unit("t2", "d1", 0)), "tenant");
        assert_ne!(a, point_id_of(&unit("t1", "d2", 0)), "document");
        assert_ne!(a, point_id_of(&unit("t1", "d1", 1)), "chunk index");
    }

    #[test]
    fn the_separator_stops_a_concatenation_collision() {
        // Without U+001F, ("ab", "c", 0) and ("a", "bc", 0) would hash the same
        // string and silently share a point.
        assert_ne!(
            point_id_of(&unit("ab", "c", 0)),
            point_id_of(&unit("a", "bc", 0))
        );
    }

    #[test]
    fn the_same_unit_always_hashes_to_the_same_id() {
        assert_eq!(
            point_id_of(&unit("t1", "d1", 3)),
            point_id_of(&unit("t1", "d1", 3))
        );
    }
}
