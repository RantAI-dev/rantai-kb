//! Tenant + visibility scope.
//!
//! The whole point of this type is the visibility rule: every `ChunkStore`
//! search method takes `&DocumentScope`, and the only thing in the workspace that
//! can build a `DocumentScope` is a [`DocumentStore`] implementation. A
//! tenant-unscoped search therefore does not compile, which is what removes the
//! cross-tenant leak findings 1 and 3 of `docs/porting-plan.md` describe.

use serde::{Deserialize, Serialize};

use crate::ids::{DocumentId, TenantId};

/// What the chunk store is allowed to look at: every visible document id under
/// the active tenant. Constructed only by [`crate::DocumentStore`] impls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentScope {
    tenant: TenantId,
    document_ids: Vec<DocumentId>,
}

impl DocumentScope {
    /// Construct a scope from a tenant and an ordered set of visible document
    /// ids. Only [`crate::DocumentStore`] impls are supposed to call this —
    /// server-side request handlers must not be able to fabricate a scope.
    pub(crate) fn new(tenant: TenantId, document_ids: Vec<DocumentId>) -> Self {
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
    /// visible to this caller" — chunk store searches must short-circuit.
    #[must_use]
    pub fn document_ids(&self) -> &[DocumentId] {
        &self.document_ids
    }
}