//! Postgres-backed `DocumentStore` and API key resolver.
//!
//! All read paths are filtered by `tenantId` and exclude `deletedAt`. Every
//! value that goes into SQL is bound through sqlx; the only inline value is
//! the sanitised `LIMIT` integer (capped at 200).
//!
//! `DocumentScope` construction requires a sealed token, ensuring this
//! crate and test fakes are the only producers of scopes in the workspace.

pub mod api_keys;
mod auth;
mod documents;
mod graph;
pub(crate) mod graph_queries;
mod jobs;
pub mod migrate;
mod queries;
pub mod reindex;
mod store;

pub use api_keys::{MintedApiKey, NewApiKey, generate_api_key, insert_api_key};
pub use auth::hash_api_key;
pub use documents::row_to_doc_meta;
pub use graph::PgGraphStore;
pub use jobs::PgJobStore;
pub use migrate::{Migration, MigrationReport};
pub use reindex::{ReindexDocument, ReindexTenantCount, plan};
pub use store::{PgDocumentStore, PgSettings, connect_pool};

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
    fn api_key_and_job_statements_exist() {
        // Both stores read the same schema; losing a constant silently would
        // only show up as a runtime 500.
        assert!(queries::SELECT_API_KEY.contains("\"ApiKey\""));
        assert!(queries::CLAIM_NEXT_PENDING.contains("\"IngestJob\""));
        assert!(queries::SET_DOCUMENT_STATUS.contains("\"Document\""));
    }

    #[test]
    fn filter_default_is_empty() {
        let f = DocumentFilter::default();
        assert!(f.knowledge_base_ids.is_empty());
        assert!(f.category.is_none());
    }
}
