//! Qdrant adapter for `kb-ports::RetrievalIndex`.
//!
//! # Architecture layout (`docs/architecture-target.md` §2)
//!
//! - **One collection per (representation, embedding model)**:
//!   `chunks__<model-slug>__<dim>`, with alias `chunks__active`.
//! - **Named vectors**:
//!   - `dense`: Cosine distance, HNSW indexing.
//!   - `sparse`: Sparse vectors with [`qdrant_client::qdrant::Modifier::Idf`].
//! - **Payload indexes**:
//!   - `tenant_id`: keyword with `is_tenant: true`.
//!   - `document_id`, `knowledge_base_id`, `category`, `unit_id`: keyword.
//!   - `chunk_index`: integer.
//! - **Deterministic point ids**: UUIDv5 of (tenant, document, chunk index).
//! - **Strict scope isolation**: every search, fetch, and neighbour expansion
//!   is scoped to tenant and document IDs.
//! - **Single round-trip hybrid search**: executed via Qdrant's Query API with
//!   prefetching and RRF fusion.

pub mod ids;
pub mod index;
pub mod payload;
pub mod settings;

pub use ids::{POINT_NAMESPACE, point_id, point_id_of};
pub use index::QdrantIndex;
pub use settings::{
    ACTIVE_ALIAS, DENSE_VECTOR, QdrantSettings, SPARSE_VECTOR, create_collection,
    create_if_missing, drop_collection,
};
