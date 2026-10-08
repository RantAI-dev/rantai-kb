//! Postgres-backed `DocumentStore` and API key resolver.
//!
//! All read paths are filtered by `tenantId` and exclude `deletedAt`. Every
//! value that goes into SQL is bound through sqlx; the only inline value is
//! the sanitised `LIMIT` integer (capped at 200).
//!
//! `DocumentScope` construction requires a sealed token, ensuring this
//! crate and test fakes are the only producers of scopes in the workspace.

mod auth;
mod documents;
mod queries;
mod store;

pub use auth::hash_api_key;
pub use documents::row_to_doc_meta;
pub use store::{PgDocumentStore, PgSettings};

#[cfg(test)]
mod tests {
    use super::*;
    use kb_ports::DocumentFilter;

    #[test]
    fn max_limit_is_capped() {
        assert_eq!(queries::MAX_LIMIT, 200);
    }

    #[test]
    fn api_key_hashing_is_deterministic() {
        let h1 = hash_api_key("rkb_demo");
        let h2 = hash_api_key("rkb_demo");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex
    }

    #[test]
    fn filter_default_is_empty() {
        let f = DocumentFilter::default();
        assert!(f.knowledge_base_ids.is_empty());
        assert!(f.category.is_none());
    }
}
