//! Tenant + visibility scope.
//!
//! The visibility rule: every `ChunkStore` search method takes
//! `&DocumentScope`, and the only place in the workspace that constructs one
//! is a `DocumentStore` implementation (`kb-store-pg` or in-memory test fakes).
//! The chunk store never sees knowledge-base ids or category joins; it gets a
//! concrete, resolved `Vec<DocumentId>` that has already been filtered by tenant
//! and exclusion of `deletedAt`. Cross-tenant access therefore requires forging a
//! scope, which is prevented by requiring a sealed `ScopeToken` only constructible
//! by adapters. Tenant isolation is verified by integration tests in `kb-server`
//! where a tenant-B key searches data belonging to tenant A.

use serde::{Deserialize, Serialize};

use crate::ids::{DocumentId, TenantId};

/// Adapter token module. Prevents arbitrary callers from freely constructing
/// [`DocumentScope`].
pub mod adapter {
    /// Token proving that the caller is a `DocumentStore` adapter implementation.
    #[derive(Debug)]
    pub struct ScopeToken {
        _private: (),
    }

    impl ScopeToken {
        /// Create a token. Restricted by convention to adapter crates (`kb-store-pg`, fakes).
        #[must_use]
        pub const fn new_for_adapter() -> Self {
            Self { _private: () }
        }
    }
}

/// Tenant + visibility scope. Built only by `DocumentStore` adapters using [`adapter::ScopeToken`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentScope {
    tenant: TenantId,
    /// Resolved, tenant-filtered, alive (non-deleted) document ids. Empty
    /// means "no documents are visible to this caller" — chunk store searches
    /// MUST short-circuit on this case.
    document_ids: Vec<DocumentId>,
}

impl DocumentScope {
    /// Construct a scope. Requires a [`adapter::ScopeToken`] proving the caller
    /// is an adapter that resolved visibility against tenant storage.
    #[must_use]
    pub fn new(
        _token: &adapter::ScopeToken,
        tenant: TenantId,
        document_ids: Vec<DocumentId>,
    ) -> Self {
        Self {
            tenant,
            document_ids,
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
