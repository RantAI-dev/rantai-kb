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

/// Tenant + visibility scope. Built by `DocumentStore` adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentScope {
    tenant: TenantId,
    /// Resolved, tenant-filtered, alive (non-deleted) document ids. Empty
    /// means "no documents are visible to this caller" and chunk store searches
    /// short-circuit on it.
    document_ids: Vec<DocumentId>,
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
