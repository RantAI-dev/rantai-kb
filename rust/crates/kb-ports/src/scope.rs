//! Tenant + visibility scope.
//!
//! The visibility rule: every `ChunkStore` search method takes
//! `&DocumentScope`, and the only place in the workspace that constructs one
//! is a `DocumentStore` implementation (`kb-store-pg`). The chunk store never
//! sees knowledge-base ids or category joins; it gets a concrete, resolved
//! `Vec<DocumentId>` that has already been filtered by tenant and exclusion
//! of `deletedAt`. Cross-tenant access therefore requires forging a scope, and
//! that requires lying to Postgres — which the test suite proves is impossible
//! by attempting tenant A → tenant B from a tenant-B key.
//!
//! `new` is `pub` because `kb-store-pg` is a sibling crate and `pub(crate)`
//! doesn't span crates. The contract that only `DocumentStore` impls call it
//! is enforced by code review and the integration test in `kb-server`.

use serde::{Deserialize, Serialize};

use crate::ids::{DocumentId, TenantId};

/// Tenant + visibility scope. Built only by `kb-store-pg`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentScope {
    tenant: TenantId,
    /// Resolved, tenant-filtered, alive (non-deleted) document ids. Empty
    /// means "no documents are visible to this caller" — chunk store searches
    /// MUST short-circuit on this case.
    document_ids: Vec<DocumentId>,
}

impl DocumentScope {
    /// Construct a scope. Callers MUST be a `DocumentStore` implementation
    /// and MUST have already resolved the filter against the tenant's data.
    #[must_use]
    pub fn new(tenant: TenantId, document_ids: Vec<DocumentId>) -> Self {
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