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
//!
//! # Two modes (SEC-2a)
//!
//! `Ids` is the original behaviour: every visible document id is materialised
//! and sent to the index as a `match any` filter. `Predicate` keeps only the
//! coarse conditions the index payload can express (tenant, knowledge bases,
//! category) and defers the authoritative visibility decision to Postgres via
//! [`crate::DocumentStore::verify_visible`]. A predicate scope is an
//! optimisation only: every candidate it produces is verified before fusion,
//! and a failed verify fails the search.

use serde::{Deserialize, Serialize};

use crate::ids::{DocumentId, KnowledgeBaseId, TenantId};
use crate::traits::{DocumentFilter, ScopeRequest};

/// Default maximum number of visible ids a scope may materialise before the
/// adapter switches to predicate mode. `KB_SCOPE_ID_LIMIT=0` disables predicate
/// mode entirely.
pub const DEFAULT_SCOPE_ID_LIMIT: usize = 2000;

/// Read `KB_SCOPE_ID_LIMIT`. An unset, blank or malformed value uses the
/// default; `0` means "always Ids mode".
#[must_use]
pub fn scope_id_limit_from_env() -> usize {
    std::env::var("KB_SCOPE_ID_LIMIT")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_SCOPE_ID_LIMIT)
}

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

/// The authoritative filter the verify re-runs in Postgres, restricted to the
/// candidate ids. It is carried verbatim from the `ScopeRequest` that built the
/// scope, so the verify cannot be wider or narrower than `build_scope` was.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifySpec {
    pub tenant: TenantId,
    pub filter: DocumentFilter,
    /// `documentIds` narrowing: the index adds a document-id condition and the
    /// verify still applies the full filter to those ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restricted_ids: Option<Vec<DocumentId>>,
}

impl VerifySpec {
    /// The request to hand to `DocumentStore::verify_visible`.
    #[must_use]
    pub fn request(&self) -> ScopeRequest {
        ScopeRequest {
            tenant: self.tenant.clone(),
            filter: self.filter.clone(),
            force_ids: false,
        }
    }
}

/// The predicate mode a `DocumentStore` adapter may choose (SEC-2a).
///
/// Eligibility is shared by Postgres and the fakes so a test cannot pass where
/// production would stay in Ids mode. A request is eligible only when:
/// - the visible-id count exceeds `limit` (`limit == 0` disables predicate mode);
/// - no RET-15 unit range will be applied (`req.force_ids`);
/// - the filter uses only tenant, knowledge bases and category, so the index
///   payload can express it; and
/// - the tenant has no versioned documents (RET-16), because the payload has no
///   `is_latest` key and the default `isLatest` filter would be unenforceable
///   index-side.
#[must_use]
pub fn predicate_spec_for(
    req: &ScopeRequest,
    visible_count: usize,
    limit: usize,
    tenant_has_versioned_documents: bool,
) -> Option<PredicateSpec> {
    if limit == 0 || visible_count <= limit || req.force_ids || tenant_has_versioned_documents {
        return None;
    }
    if req.filter.external_refs.is_some()
        || req.filter.language.is_some()
        || !req.filter.metadata.is_empty()
    {
        return None;
    }
    let kb_ids = if req.filter.knowledge_base_ids.is_empty() {
        None
    } else {
        Some(req.filter.knowledge_base_ids.clone())
    };
    Some(PredicateSpec {
        kb_ids,
        category: req.filter.category.clone(),
        verify: VerifySpec {
            tenant: req.tenant.clone(),
            filter: req.filter.clone(),
            restricted_ids: None,
        },
    })
}

/// The coarse index conditions of a predicate scope (SEC-2a).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredicateSpec {
    /// `None` means no knowledge-base condition; `Some([])` matches nothing.
    pub kb_ids: Option<Vec<KnowledgeBaseId>>,
    pub category: Option<String>,
    pub verify: VerifySpec,
}

/// Internal mode of a [`DocumentScope`].
#[derive(Debug, Clone, Serialize, Deserialize)]
enum ScopeMode {
    /// Every visible id, materialised by `build_scope`.
    Ids(Vec<DocumentId>),
    /// Coarse index conditions plus the Postgres verify.
    Predicate(PredicateSpec),
}

/// Tenant + visibility scope. Built by `DocumentStore` adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentScope {
    tenant: TenantId,
    mode: ScopeMode,
    /// RET-15: an optional within-document range. It can only ever narrow, and
    /// is only meaningful for a scope holding exactly one document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unit_range: Option<UnitRange>,
}

impl DocumentScope {
    /// Construct a scope from adapter-resolved visibility (Ids mode).
    #[cfg(feature = "scope-adapter")]
    #[must_use]
    pub fn new(
        _token: &adapter::ScopeToken,
        tenant: TenantId,
        document_ids: Vec<DocumentId>,
    ) -> Self {
        Self {
            tenant,
            mode: ScopeMode::Ids(document_ids),
            unit_range: None,
        }
    }

    /// Construct a predicate scope. Only a `DocumentStore` adapter may choose
    /// this mode, and only after checking the eligibility rules documented on
    /// [`DocumentStore::build_scope`](crate::DocumentStore::build_scope).
    #[cfg(feature = "scope-adapter")]
    #[must_use]
    pub fn new_predicate(
        _token: &adapter::ScopeToken,
        tenant: TenantId,
        spec: PredicateSpec,
    ) -> Self {
        Self {
            tenant,
            mode: ScopeMode::Predicate(spec),
            unit_range: None,
        }
    }

    /// Return a copy limited to the given ids. Only ids already in this scope
    /// are kept, so the result can never be wider than `self`.
    ///
    /// In predicate mode the ids cannot be enumerated, so the narrowing is
    /// carried on the scope: the index adds an `document_id any-of ids`
    /// condition and the Postgres verify still applies the original filter.
    /// The result is therefore no wider than `self` as far as the index is
    /// concerned, and no wider than the filter as far as Postgres is concerned.
    #[must_use]
    pub fn narrowed_to(&self, ids: &[DocumentId]) -> Self {
        match &self.mode {
            ScopeMode::Ids(document_ids) => Self {
                tenant: self.tenant.clone(),
                mode: ScopeMode::Ids(
                    document_ids
                        .iter()
                        .filter(|visible| ids.contains(visible))
                        .cloned()
                        .collect(),
                ),
                unit_range: self.unit_range,
            },
            ScopeMode::Predicate(spec) => {
                let mut verify = spec.verify.clone();
                let narrowed: Vec<DocumentId> = match verify.restricted_ids.take() {
                    Some(previous) => previous
                        .into_iter()
                        .filter(|id| ids.contains(id))
                        .collect(),
                    None => ids.to_vec(),
                };
                verify.restricted_ids = Some(narrowed);
                Self {
                    tenant: self.tenant.clone(),
                    mode: ScopeMode::Predicate(PredicateSpec {
                        verify,
                        ..spec.clone()
                    }),
                    unit_range: self.unit_range,
                }
            }
        }
    }

    /// Restrict this scope to one part of its single document (RET-15).
    ///
    /// A page or chunk number has meaning only inside one document, so a range
    /// on a scope with zero or several documents is refused (the scope is
    /// returned unchanged) rather than applied to an ambiguous set. Routes
    /// validate this and answer 400; this keeps adapters safe even if a future
    /// caller forgets. Predicate scopes that were narrowed to one document are
    /// safe too: the index filter has a document-id condition and the verify
    /// still runs the full filter.
    #[must_use]
    pub fn with_unit_range(&self, range: UnitRange) -> Self {
        if self.visible_document_count() != Some(1) {
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
    /// visible to this caller" **only in Ids mode**.
    ///
    /// In predicate mode the ids are deliberately not materialised, so this
    /// returns an empty slice that is NOT a "nothing visible" signal. Every
    /// caller must either check [`DocumentScope::is_predicate`] first or use
    /// [`DocumentScope::is_empty`] / [`DocumentScope::allows_document`].
    #[must_use]
    pub fn document_ids(&self) -> &[DocumentId] {
        match &self.mode {
            ScopeMode::Ids(ids) => ids,
            ScopeMode::Predicate(_) => &[],
        }
    }

    /// True when this scope defers visibility to the Postgres verify.
    #[must_use]
    pub const fn is_predicate(&self) -> bool {
        matches!(self.mode, ScopeMode::Predicate(_))
    }

    /// The predicate spec, when this scope is in predicate mode.
    #[must_use]
    pub const fn predicate(&self) -> Option<&PredicateSpec> {
        match &self.mode {
            ScopeMode::Ids(_) => None,
            ScopeMode::Predicate(spec) => Some(spec),
        }
    }

    /// The Postgres verify spec, when this scope is in predicate mode.
    #[must_use]
    pub fn verify_spec(&self) -> Option<&VerifySpec> {
        self.predicate().map(|spec| &spec.verify)
    }

    /// Ids explicitly narrowed into a predicate scope by `documentIds`, if any.
    #[must_use]
    pub fn restricted_ids(&self) -> Option<&[DocumentId]> {
        self.verify_spec()
            .and_then(|verify| verify.restricted_ids.as_deref())
    }

    /// True when this scope can match no document at all. The only safe
    /// replacement for `document_ids().is_empty()`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match &self.mode {
            ScopeMode::Ids(ids) => ids.is_empty(),
            ScopeMode::Predicate(spec) => spec
                .verify
                .restricted_ids
                .as_ref()
                .is_some_and(Vec::is_empty),
        }
    }

    /// How many documents this scope names, when the ids are materialised or
    /// explicitly narrowed. `None` means "unknown until the verify runs".
    #[must_use]
    pub fn visible_document_count(&self) -> Option<usize> {
        match &self.mode {
            ScopeMode::Ids(ids) => Some(ids.len()),
            ScopeMode::Predicate(spec) => spec.verify.restricted_ids.as_ref().map(Vec::len),
        }
    }

    /// Whether a document id may be looked up through this scope.
    ///
    /// Ids mode answers by membership. Predicate mode cannot deny by id (the
    /// id list was deliberately not materialised), so it denies only an
    /// explicitly-narrowed scope; the index conditions and the Postgres verify
    /// are the actual fence.
    #[must_use]
    pub fn allows_document(&self, id: &DocumentId) -> bool {
        match &self.mode {
            ScopeMode::Ids(ids) => ids.contains(id),
            ScopeMode::Predicate(spec) => spec
                .verify
                .restricted_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(id)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::ids::DocumentId;

    fn ids_scope(documents: &[&str]) -> DocumentScope {
        serde_json::from_value(serde_json::json!({
            "tenant": "tenant-a",
            "mode": { "Ids": documents },
        }))
        .unwrap()
    }

    fn predicate_scope(kb_ids: Option<Vec<&str>>, category: Option<&str>) -> DocumentScope {
        serde_json::from_value(serde_json::json!({
            "tenant": "tenant-a",
            "mode": {
                "Predicate": {
                    "kb_ids": kb_ids.map(|ids| ids.into_iter().map(str::to_string).collect::<Vec<_>>()),
                    "category": category,
                    "verify": {
                        "tenant": "tenant-a",
                        "filter": { "knowledge_base_ids": [], "category": category,
                                    "external_refs": null, "language": null, "metadata": [] },
                    },
                },
            },
        }))
        .unwrap()
    }

    #[test]
    fn a_range_is_applied_only_to_a_single_document_scope() {
        let range = UnitRange {
            pages: Some((1, 5)),
            chunks: Some((40, 57)),
        };
        let one = ids_scope(&["doc-a"]).with_unit_range(range);
        assert_eq!(one.unit_range(), Some(range));
        // A narrowed scope keeps the range: the route narrows before it sets it,
        // but any future reordering must not silently drop the restriction.
        let narrowed = one.narrowed_to(&[DocumentId("doc-a".to_string())]);
        assert_eq!(narrowed.unit_range(), Some(range));
        for documents in [vec![], vec!["doc-a", "doc-b"]] {
            let many = ids_scope(&documents).with_unit_range(range);
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
        let page_scope = ids_scope(&["doc-a"])
            .with_unit_range(range)
            .pages_only();
        assert_eq!(
            page_scope.unit_range(),
            Some(UnitRange {
                pages: Some((12, 15)),
                chunks: None,
            })
        );
    }

    #[test]
    fn a_predicate_scope_does_not_pretend_its_ids_are_visible() {
        let scope = predicate_scope(Some(vec!["kb-1"]), None);
        assert!(scope.is_predicate());
        assert!(scope.document_ids().is_empty(), "ids are not materialised");
        // Empty ids must not be read as "nothing visible" by a caller.
        assert!(!scope.is_empty());
        assert!(scope.visible_document_count().is_none());
        assert!(scope.allows_document(&DocumentId::new_unchecked("doc-a")));
        assert_eq!(scope.verify_spec().unwrap().tenant.as_str(), "tenant-a");
    }

    #[test]
    fn narrowing_a_predicate_scope_keeps_the_filter_and_adds_an_id_condition() {
        let scope = predicate_scope(None, Some("cat-1"));
        let narrowed = scope.narrowed_to(&[DocumentId::new_unchecked("doc-a")]);
        assert!(narrowed.is_predicate());
        assert_eq!(narrowed.restricted_ids().unwrap().len(), 1);
        assert!(!narrowed.allows_document(&DocumentId::new_unchecked("doc-b")));
        assert_eq!(narrowed.visible_document_count(), Some(1));
        let empty = scope.narrowed_to(&[]);
        assert!(empty.is_empty());
    }

    #[test]
    fn a_predicate_scope_narrowed_to_one_document_can_carry_a_range() {
        let scope = predicate_scope(None, None)
            .narrowed_to(&[DocumentId::new_unchecked("doc-a")]);
        let range = UnitRange {
            pages: Some((3, 4)),
            chunks: Some((10, 20)),
        };
        assert_eq!(scope.with_unit_range(range).unit_range(), Some(range));
        assert_eq!(predicate_scope(None, None).with_unit_range(range).unit_range(), None);
    }
}
