//! Tenant + visibility scope.
//!
//! Every `ChunkStore` search takes `&DocumentScope`. A scope is resolved by a
//! `DocumentStore` adapter (tenant filter plus soft-delete exclusion) and can
//! afterwards only shrink through [`DocumentScope::narrowed_to`].
//!
//! # Scope Sealing Guarantee
//!
//! Constructing a scope requires an [`adapter::ScopeToken`]. Note that because
//! Cargo feature unification enables `scope-adapter` across all crates in a
//! workspace compilation unit whenever `kb-store-pg` is linked, Cargo features
//! alone cannot prevent `kb-server` from seeing `ScopeToken` at compile time.
//!
//! What is guaranteed:
//! 1. `ScopeToken` exists only when the `scope-adapter` feature is enabled on `kb-ports`.
//! 2. A CI grep check guarantees that `new_for_adapter` is never called outside
//!    `kb-store-pg` and the engine test fakes (`kb-engine/src/fakes.rs`).

use serde::{Deserialize, Serialize};

use crate::ids::{DocumentId, TenantId};

/// Adapter token. Exists only when the `scope-adapter` feature is enabled, so
/// that a crate depending on `kb-ports` without that feature cannot name it.
#[cfg(feature = "scope-adapter")]
pub mod adapter {
    /// Proof that the caller is a `DocumentStore` adapter.
    #[derive(Debug)]
    pub struct ScopeToken {
        _private: (),
    }

    impl ScopeToken {
        /// Create a token. Only adapter crates and test fakes may call this.
        #[must_use]
        pub const fn new_for_adapter() -> Self {
            Self { _private: () }
        }
    }
}

/// An optional restriction to one part of a *single* document (RET-15).
///
/// Both halves are inclusive. `pages` is the unit the page-image arm and any
/// page-bearing chunk understands; `chunks` is the index range a section maps
/// to. A caller that resolves a section can set both (a section has a page
/// range too), so the page arm can be filtered on pages rather than on an
/// unrelated chunk index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRange {
    /// Inclusive `(start, end)` page numbers, when the caller restricted by
    /// page. Absent means "no page restriction".
    pub pages: Option<(u32, u32)>,
    /// Inclusive `(start, end)` chunk indices, when the caller restricted by
    /// section or chunk index. Absent means "no chunk restriction".
    pub chunks: Option<(u32, u32)>,
}

impl UnitRange {
    /// Whether a unit carrying this page is inside the range.
    ///
    /// A unit with no page is **outside** a page range, matching Qdrant's
    /// range condition (a point without the field cannot match it).
    #[must_use]
    pub fn allows_page(&self, page: Option<u32>) -> bool {
        match (self.pages, page) {
            (Some((lo, hi)), Some(p)) => (lo..=hi).contains(&p),
            (Some(_), None) => false,
            (None, _) => true,
        }
    }

    /// Whether a unit at this chunk index is inside the range.
    #[must_use]
    pub fn allows_chunk(&self, chunk_index: u32) -> bool {
        self.chunks
            .is_none_or(|(lo, hi)| (lo..=hi).contains(&chunk_index))
    }

    /// Both conditions at once: the same AND a Qdrant filter applies.
    #[must_use]
    pub fn allows(&self, page: Option<u32>, chunk_index: u32) -> bool {
        self.allows_page(page) && self.allows_chunk(chunk_index)
    }
}

/// Tenant + visibility scope. Built by `DocumentStore` adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentScope {
    tenant: TenantId,
    /// Resolved, tenant-filtered, alive (non-deleted) document ids. Empty
    /// means "no documents are visible to this caller" and chunk store searches
    /// short-circuit on it.
    document_ids: Vec<DocumentId>,
    /// RET-15: an optional within-document range. It can only ever narrow, and
    /// is only meaningful for a scope holding exactly one document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unit_range: Option<UnitRange>,
}

impl DocumentScope {
    /// Construct a scope from adapter-resolved visibility.
    #[cfg(feature = "scope-adapter")]
    #[must_use]
    pub fn new(
        _token: &adapter::ScopeToken,
        tenant: TenantId,
        document_ids: Vec<DocumentId>,
    ) -> Self {
        Self {
            tenant,
            document_ids,
            unit_range: None,
        }
    }

    /// Return a copy limited to the given ids. Only ids already in this scope
    /// are kept, so the result can never be wider than `self`.
    #[must_use]
    pub fn narrowed_to(&self, ids: &[DocumentId]) -> Self {
        let document_ids = self
            .document_ids
            .iter()
            .filter(|visible| ids.contains(visible))
            .cloned()
            .collect();
        Self {
            tenant: self.tenant.clone(),
            document_ids,
            unit_range: self.unit_range,
        }
    }

    /// Restrict this scope to one part of its single document (RET-15).
    ///
    /// A page or chunk number has meaning only inside one document, so a range
    /// on a scope with zero or several documents is refused (the scope is
    /// returned unchanged) rather than applied to an ambiguous set. Routes
    /// validate this and answer 400; this keeps adapters safe even if a future
    /// caller forgets.
    #[must_use]
    pub fn with_unit_range(&self, range: UnitRange) -> Self {
        if self.document_ids.len() != 1 {
            return self.clone();
        }
        Self {
            unit_range: Some(range),
            ..self.clone()
        }
    }

    /// The within-document range, when one was set.
    #[must_use]
    pub const fn unit_range(&self) -> Option<UnitRange> {
        self.unit_range
    }

    /// A copy for reads against the *page-image* index, where `chunkIndex` is
    /// the page number rather than a text chunk's index. Keeping the chunk
    /// condition there would exclude every page of a section-scoped search.
    #[must_use]
    pub fn pages_only(&self) -> Self {
        let Some(range) = self.unit_range else {
            return self.clone();
        };
        Self {
            unit_range: Some(UnitRange {
                pages: range.pages,
                chunks: None,
            }),
            ..self.clone()
        }
    }

    /// Tenant this scope belongs to.
    #[must_use]
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// Visible document ids, in arbitrary order. Empty means "no documents are
    /// visible to this caller".
    #[must_use]
    pub fn document_ids(&self) -> &[DocumentId] {
        &self.document_ids
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn scope(documents: &[&str]) -> DocumentScope {
        serde_json::from_value(serde_json::json!({
            "tenant": "tenant-a",
            "document_ids": documents,
        }))
        .unwrap()
    }

    #[test]
    fn a_range_is_applied_only_to_a_single_document_scope() {
        let range = UnitRange {
            pages: Some((1, 5)),
            chunks: Some((40, 57)),
        };
        let one = scope(&["doc-a"]).with_unit_range(range);
        assert_eq!(one.unit_range(), Some(range));
        // A narrowed scope keeps the range: the route narrows before it sets it,
        // but any future reordering must not silently drop the restriction.
        let narrowed = one.narrowed_to(&[DocumentId("doc-a".to_string())]);
        assert_eq!(narrowed.unit_range(), Some(range));
        for documents in [vec![], vec!["doc-a", "doc-b"]] {
            let many = scope(&documents).with_unit_range(range);
            assert_eq!(many.unit_range(), None, "{documents:?}");
        }
    }

    #[test]
    fn a_page_without_a_number_is_outside_a_page_range() {
        let range = UnitRange {
            pages: Some((1, 5)),
            chunks: None,
        };
        assert!(range.allows(Some(3), 0));
        assert!(!range.allows(Some(30), 0));
        assert!(!range.allows(None, 0));
    }

    #[test]
    fn pages_only_drops_the_chunk_condition_for_the_page_index() {
        let range = UnitRange {
            pages: Some((12, 15)),
            chunks: Some((40, 57)),
        };
        let page_scope = scope(&["doc-a"]).with_unit_range(range).pages_only();
        assert_eq!(
            page_scope.unit_range(),
            Some(UnitRange {
                pages: Some((12, 15)),
                chunks: None,
            })
        );
    }
}
