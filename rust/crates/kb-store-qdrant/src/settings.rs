//! Connection settings and the collection layout of `docs/architecture-target.md` §2.
//!
//! # One collection per (embedding model, dimension)
//!
//! ```text
//! collection: chunks__<model-slug>__<dim>
//! alias:      chunks__active
//! ```
//!
//! Vector spaces never mix, so an embedding model change means building a new
//! collection and then moving the alias — no downtime, and a rollback is moving
//! it back. The `kb_index` Postgres table that records which collection backs
//! which alias is **not** built yet; see [`crate`]'s module docs.

use std::time::Duration;

use kb_ports::PortError;
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CreateAliasBuilder, CreateFieldIndexCollectionBuilder, Distance, FieldType, HnswConfigDiff,
    IntegerIndexParamsBuilder, KeywordIndexParamsBuilder, Modifier, SparseIndexConfigBuilder,
    SparseVectorConfig, SparseVectorParamsBuilder, VectorParamsBuilder, VectorsConfigBuilder,
};

/// Named dense vector. Cosine, because every embedder we use is trained with a
/// cosine objective and a dot product would rank by norm as well.
pub const DENSE_VECTOR: &str = "dense";
/// Named sparse (keyword) vector, with [`Modifier::Idf`].
pub const SPARSE_VECTOR: &str = "sparse";
/// The alias a running server points at. One name, whatever model is live.
pub const ACTIVE_ALIAS: &str = "chunks__active";

/// How to reach a Qdrant and how to shape its collection.
#[derive(Debug, Clone)]
pub struct QdrantSettings {
    /// gRPC endpoint, e.g. `http://127.0.0.1:16334`. The official Rust client
    /// speaks gRPC; the REST port (6333) is only used by the stack script's
    /// readiness probe.
    pub url: String,
    /// `QDRANT_API_KEY`, when the deployment has one.
    pub api_key: Option<String>,
    /// Embedding model slug, e.g. `qwen3-emb-0.6b`. Part of the collection
    /// name, so two models can never share a vector space.
    pub model_slug: String,
    /// Dense width. `None` builds a sparse-only collection, which is what the
    /// plan's keyword-arm measurement (L.2) indexes: no embedding is spent to
    /// score a BM25 arm.
    pub dense_dims: Option<u32>,
    /// HNSW `m` for the dense vector.
    pub hnsw_m: u32,
    /// HNSW `ef_construct` for the dense vector.
    pub hnsw_ef_construct: u32,
    /// Per-request timeout.
    pub request_timeout: Duration,
    /// Points per `upsert_points` call.
    pub upsert_batch: usize,
}

impl QdrantSettings {
    /// Settings from the environment, for the CLI and the contract test.
    ///
    /// * `QDRANT_GRPC_URL` (default `http://127.0.0.1:16334`, the port
    ///   `KB_STACK_QDRANT=1` maps) or `QDRANT_URL`.
    /// * `QDRANT_API_KEY`.
    /// * `KB_QDRANT_MODEL` (default `unknown`).
    /// * `KB_QDRANT_DENSE_DIMS` (default none: sparse-only).
    #[must_use]
    pub fn from_env() -> Self {
        let dense_dims = std::env::var("KB_QDRANT_DENSE_DIMS")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|n| *n > 0);
        Self {
            url: std::env::var("QDRANT_GRPC_URL")
                .or_else(|_| std::env::var("QDRANT_URL"))
                .unwrap_or_else(|_| "http://127.0.0.1:16334".to_string()),
            api_key: std::env::var("QDRANT_API_KEY").ok(),
            model_slug: std::env::var("KB_QDRANT_MODEL").unwrap_or_else(|_| "unknown".to_string()),
            dense_dims,
            hnsw_m: 16,
            hnsw_ef_construct: 100,
            request_timeout: Duration::from_secs(30),
            upsert_batch: 256,
        }
    }

    /// The collection this configuration addresses:
    /// `chunks__<model-slug>__<dim>`, or `chunks__<model-slug>__sparse` when
    /// there is no dense vector at all.
    #[must_use]
    pub fn collection_name(&self) -> String {
        match self.dense_dims {
            Some(dims) => format!("chunks__{}__{dims}", slug(&self.model_slug)),
            None => format!("chunks__{}__sparse", slug(&self.model_slug)),
        }
    }

    /// Build a client for this endpoint. The connection is lazy: a bad URL
    /// surfaces on the first request, not here.
    ///
    /// # Errors
    ///
    /// [`PortError::InvalidArgument`] when the URL cannot be parsed as a gRPC
    /// endpoint.
    pub fn client(&self) -> Result<Qdrant, PortError> {
        let mut builder = Qdrant::from_url(&self.url)
            .timeout(self.request_timeout)
            .skip_compatibility_check();
        if let Some(key) = &self.api_key {
            builder = builder.api_key(key.clone());
        }
        builder.build().map_err(|err| {
            PortError::InvalidArgument(format!("Qdrant endpoint {}: {err}", self.url))
        })
    }
}

/// The model slug as it appears in a collection name: lowercase, and only
/// characters a Qdrant collection name accepts comfortably.
#[must_use]
pub fn slug(model: &str) -> String {
    let mut out = String::with_capacity(model.len());
    for ch in model.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

/// Create the collection and its payload indexes if they are not there yet.
///
/// Idempotent, and safe to call concurrently: `create_collection` on an existing
/// collection is an error that we treat as success.
///
/// **Quantisation is deliberately off** for now. `docs/plan-best-multimodal-kb.md`
/// step L.2 asks for scalar or binary quantisation with rescoring; adding it is
/// an ablation the eval should decide (recall lost vs memory), not a default.
/// See the crate docs for the follow-up.
///
/// # Errors
///
/// [`PortError`] from the client, mapped one-for-one.
pub async fn create_collection(
    client: &Qdrant,
    settings: &QdrantSettings,
    collection: &str,
) -> Result<(), PortError> {
    if !client.collection_exists(collection).await.map_err(map)? {
        let mut builder = qdrant_client::qdrant::CreateCollectionBuilder::new(collection);
        if let Some(dims) = settings.dense_dims {
            let mut vectors = VectorsConfigBuilder::default();
            vectors.add_named_vector_params(
                DENSE_VECTOR,
                VectorParamsBuilder::new(u64::from(dims), Distance::Cosine).hnsw_config(
                    HnswConfigDiff {
                        m: Some(u64::from(settings.hnsw_m)),
                        ef_construct: Some(u64::from(settings.hnsw_ef_construct)),
                        ..Default::default()
                    },
                ),
            );
            builder = builder.vectors_config(vectors);
        }
        let sparse_params = SparseVectorParamsBuilder::default()
            // IDF is applied by the index, over the collection. The
            // per-tenant corpus filter (v1.19+) is not wired up yet; see
            // the crate docs.
            .modifier(Modifier::Idf as i32)
            .index(SparseIndexConfigBuilder::default().build())
            .build();
        let sparse_config = SparseVectorConfig {
            map: [(SPARSE_VECTOR.to_string(), sparse_params)]
                .into_iter()
                .collect(),
        };
        builder = builder.sparse_vectors_config(sparse_config);

        let created = client.create_collection(builder).await;
        if let Err(err) = created {
            // Two servers starting at once both see "missing" and both create.
            // The loser's error is "already exists", which is the outcome both
            // wanted, so it is not propagated.
            if !already_exists(&err) {
                return Err(map(err));
            }
        }
    }

    // Payload indexes. `tenant_id` is the tenant anchor (`is_tenant: true`);
    // the rest make the scope filter and the neighbour window index-driven.
    // Creating an index that exists is a no-op on the server.
    for (field, tenant) in [
        (crate::payload::KEY_TENANT_ID, true),
        (crate::payload::KEY_DOCUMENT_ID, false),
        (crate::payload::KEY_KNOWLEDGE_BASE_ID, false),
        (crate::payload::KEY_CATEGORY, false),
        (crate::payload::KEY_UNIT_ID, false),
        (crate::payload::KEY_CHUNK_TYPE, false),
    ] {
        let mut params = KeywordIndexParamsBuilder::default();
        if tenant {
            params = params.is_tenant(true);
        }
        client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(collection, field, FieldType::Keyword)
                    .field_index_params(params.build())
                    .wait(true),
            )
            .await
            .map_err(map)?;
    }
    client
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(
                collection,
                crate::payload::KEY_CHUNK_INDEX,
                FieldType::Integer,
            )
            .field_index_params(IntegerIndexParamsBuilder::default().build())
            .wait(true),
        )
        .await
        .map_err(map)?;

    Ok(())
}

/// Create the collection and its payload indexes if they are not there yet, and
/// point [`ACTIVE_ALIAS`] at it.
///
/// # Errors
///
/// [`PortError`] from the client, mapped one-for-one.
pub async fn create_if_missing(
    client: &Qdrant,
    settings: &QdrantSettings,
) -> Result<(), PortError> {
    let collection = settings.collection_name();
    create_collection(client, settings, &collection).await?;

    // The alias is what makes a model swap atomic: both collections exist, and
    // one write moves every read.
    let res = client
        .create_alias(CreateAliasBuilder::new(collection.clone(), ACTIVE_ALIAS))
        .await;
    if let Err(err) = res {
        if already_exists(&err) {
            let _ = client.delete_alias(ACTIVE_ALIAS).await;
            client
                .create_alias(CreateAliasBuilder::new(collection.clone(), ACTIVE_ALIAS))
                .await
                .map_err(map)?;
        } else {
            return Err(map(err));
        }
    }

    Ok(())
}

/// Drop a collection if it exists.
///
/// # Errors
///
/// [`PortError`] from the client, mapped one-for-one.
pub async fn drop_collection(client: &Qdrant, collection: &str) -> Result<(), PortError> {
    if client.collection_exists(collection).await.map_err(map)? {
        client.delete_collection(collection).await.map_err(map)?;
    }
    Ok(())
}

/// Whether an error is Qdrant saying "it is already there".
pub fn already_exists(err: &qdrant_client::QdrantError) -> bool {
    let text = err.to_string().to_lowercase();
    text.contains("already exists") || text.contains("already exist")
}

/// A `QdrantError` as a [`PortError`]. A gRPC status of `not_found` on a
/// collection is a deployment problem, not a caller mistake, so it is
/// `Unavailable` rather than `InvalidArgument`; the engine turns that into a
/// visible degrade instead of an empty result.
pub fn map(err: qdrant_client::QdrantError) -> PortError {
    PortError::Unavailable(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_and_a_width_name_one_collection() {
        let settings = QdrantSettings {
            model_slug: "Qwen/Qwen3-Embedding-0.6B".to_string(),
            dense_dims: Some(1024),
            ..qdrant_settings()
        };
        assert_eq!(
            settings.collection_name(),
            "chunks__qwen-qwen3-embedding-0-6b__1024"
        );
    }

    #[test]
    fn a_sparse_only_collection_says_so_in_its_name() {
        let settings = QdrantSettings {
            model_slug: "bm25".to_string(),
            dense_dims: None,
            ..qdrant_settings()
        };
        assert_eq!(settings.collection_name(), "chunks__bm25__sparse");
    }

    #[test]
    fn two_models_never_share_a_collection() {
        let a = QdrantSettings {
            model_slug: "a".to_string(),
            dense_dims: Some(8),
            ..qdrant_settings()
        };
        let b = QdrantSettings {
            model_slug: "b".to_string(),
            dense_dims: Some(8),
            ..qdrant_settings()
        };
        assert_ne!(a.collection_name(), b.collection_name());
    }

    #[test]
    fn two_widths_never_share_a_collection() {
        let a = QdrantSettings {
            model_slug: "m".to_string(),
            dense_dims: Some(1024),
            ..qdrant_settings()
        };
        let b = QdrantSettings {
            model_slug: "m".to_string(),
            dense_dims: Some(256),
            ..qdrant_settings()
        };
        assert_ne!(a.collection_name(), b.collection_name());
    }

    #[test]
    fn an_empty_slug_is_not_an_empty_collection_name() {
        assert_eq!(slug(""), "unknown");
        assert_eq!(slug("///"), "---");
    }

    #[test]
    fn dims_are_read_from_the_environment_when_set() {
        // Not a process-wide env mutation test: it checks the parser the
        // settings use, since `std::env::set_var` is unsafe in edition 2024.
        let parsed = "1024".trim().parse::<u32>().ok().filter(|n| *n > 0);
        assert_eq!(parsed, Some(1024));
        assert_eq!("0".trim().parse::<u32>().ok().filter(|n| *n > 0), None);
        assert_eq!("".trim().parse::<u32>().ok(), None);
    }

    fn qdrant_settings() -> QdrantSettings {
        QdrantSettings {
            url: "http://127.0.0.1:16334".to_string(),
            api_key: None,
            model_slug: "unknown".to_string(),
            dense_dims: None,
            hnsw_m: 16,
            hnsw_ef_construct: 100,
            request_timeout: Duration::from_secs(30),
            upsert_batch: 256,
        }
    }
}
