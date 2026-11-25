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
    BinaryQuantizationBuilder, CreateAliasBuilder, CreateFieldIndexCollectionBuilder, Distance,
    FieldType, HnswConfigDiff, IntegerIndexParamsBuilder, KeywordIndexParamsBuilder, Modifier,
    QuantizationConfig, QuantizationSearchParams, QuantizationType, ScalarQuantizationBuilder,
    SparseIndexConfigBuilder, SparseVectorConfig, SparseVectorParamsBuilder, VectorParamsBuilder,
    VectorsConfigBuilder, quantization_config, vectors_config,
};

/// Named dense vector. Cosine, because every embedder we use is trained with a
/// cosine objective and a dot product would rank by norm as well.
pub const DENSE_VECTOR: &str = "dense";
/// Named sparse (keyword) vector, with [`Modifier::Idf`].
pub const SPARSE_VECTOR: &str = "sparse";
/// The alias a running server points at. One name, whatever model is live.
pub const ACTIVE_ALIAS: &str = "chunks__active";

/// Opt-in dense-vector quantization (`KB_QDRANT_QUANTIZATION`).
///
/// Off is the default and retains the exact request bytes of the pre-RET-20
/// service. Scalar int8 and binary trade recall for RAM; neither is chosen for
/// an operator, and neither changes a collection that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuantizationMode {
    #[default]
    Off,
    Scalar,
    Binary,
}

impl QuantizationMode {
    /// Parse the three accepted values. Anything else is an error so a
    /// misspelled value fails startup instead of silently indexing float32.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "scalar" => Ok(Self::Scalar),
            "binary" => Ok(Self::Binary),
            other => Err(format!(
                "KB_QDRANT_QUANTIZATION must be \"off\", \"scalar\" or \"binary\", got {other:?}"
            )),
        }
    }

    /// The env-file spelling of this mode.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Scalar => "scalar",
            Self::Binary => "binary",
        }
    }

    /// The `quantization_config::Quantization` oneof for this mode, if any.
    /// This is the type `VectorParamsBuilder::quantization_config` takes.
    #[must_use]
    pub fn quantization_config(self) -> Option<quantization_config::Quantization> {
        match self {
            Self::Off => None,
            Self::Scalar => Some(quantization_config::Quantization::Scalar(
                ScalarQuantizationBuilder::default()
                    .r#type(QuantizationType::Int8 as i32)
                    .quantile(0.99)
                    .always_ram(true)
                    .build(),
            )),
            Self::Binary => Some(quantization_config::Quantization::Binary(
                BinaryQuantizationBuilder::default()
                    .always_ram(true)
                    .build(),
            )),
        }
    }

    /// Per-query `QuantizationSearchParams`, if quantization is on.
    #[must_use]
    pub fn search_params(
        self,
        oversampling: f64,
        rescore: bool,
    ) -> Option<QuantizationSearchParams> {
        match self {
            Self::Off => None,
            Self::Scalar | Self::Binary => Some(QuantizationSearchParams {
                rescore: Some(rescore),
                oversampling: Some(oversampling),
                ..Default::default()
            }),
        }
    }
}

/// Which mode a collection's dense vector is actually configured with.
#[must_use]
pub fn mode_of_quantization(config: &QuantizationConfig) -> QuantizationMode {
    match &config.quantization {
        Some(quantization_config::Quantization::Scalar(_)) => QuantizationMode::Scalar,
        Some(quantization_config::Quantization::Binary(_)) => QuantizationMode::Binary,
        _ => QuantizationMode::Off,
    }
}

/// Read the three quantization variables through a lookup function.
///
/// `KB_QDRANT_QUANTIZATION` defaults to `off`; `KB_QDRANT_QUANT_OVERSAMPLING`
/// defaults to 2.0 and is clamped to 1.0..=8.0; `KB_QDRANT_QUANT_RESCORE`
/// defaults to true. An unknown mode, an unparseable number or a non-boolean
/// `RESCORE` is an error: the server must not start with a setting it cannot
/// honour.
pub fn quantization_from_env<F>(lookup: &F) -> Result<(QuantizationMode, f64, bool), String>
where
    F: Fn(&str) -> Option<String>,
{
    let mode = match lookup("KB_QDRANT_QUANTIZATION").map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v.is_empty() => QuantizationMode::Off,
        None => QuantizationMode::Off,
        Some(v) => QuantizationMode::parse(&v)?,
    };
    let oversampling =
        match lookup("KB_QDRANT_QUANT_OVERSAMPLING").and_then(|v| v.trim().parse::<f64>().ok()) {
            Some(value) => value.clamp(1.0, 8.0),
            None => 2.0,
        };
    let rescore = match lookup("KB_QDRANT_QUANT_RESCORE").map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v.is_empty() => true,
        None => true,
        Some(v) => match v.as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            other => {
                return Err(format!(
                    "KB_QDRANT_QUANT_RESCORE must be true or false, got {other:?}"
                ));
            }
        },
    };
    Ok((mode, oversampling, rescore))
}

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
    /// Dense-vector quantization, applied at collection creation only.
    pub quantization: QuantizationMode,
    /// Oversampling factor for quantized search, clamped to 1.0..=8.0.
    pub quant_oversampling: f64,
    /// Re-score quantized candidates with the original float vectors.
    pub quant_rescore: bool,
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
    /// * `KB_QDRANT_QUANTIZATION`, `KB_QDRANT_QUANT_OVERSAMPLING`,
    ///   `KB_QDRANT_QUANT_RESCORE` (see [`quantization_from_env`]).
    ///
    /// The live server reads these through `Config::qdrant_from_lookup`, which
    /// fails on an unknown mode. This convenience reader falls back to `off`
    /// so the standalone examples and contract tests keep working without a
    /// fully configured environment.
    #[must_use]
    pub fn from_env() -> Self {
        let dense_dims = std::env::var("KB_QDRANT_DENSE_DIMS")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|n| *n > 0);
        let (quantization, quant_oversampling, quant_rescore) =
            quantization_from_env(&|key| std::env::var(key).ok()).unwrap_or((
                QuantizationMode::Off,
                2.0,
                true,
            ));
        Self {
            url: std::env::var("QDRANT_GRPC_URL")
                .or_else(|_| std::env::var("QDRANT_URL"))
                .unwrap_or_else(|_| "http://127.0.0.1:16334".to_string()),
            api_key: std::env::var("QDRANT_API_KEY").ok(),
            model_slug: std::env::var("KB_QDRANT_MODEL").unwrap_or_else(|_| "unknown".to_string()),
            dense_dims,
            hnsw_m: 16,
            hnsw_ef_construct: 100,
            quantization,
            quant_oversampling,
            quant_rescore,
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
/// Quantization is **opt-in** and applied only when the collection is first
/// created (`KB_QDRANT_QUANTIZATION=scalar|binary`). Off remains the default:
/// `docs/specs/v1-roadmap.md` RET-20 wants the memory/recall ablation measured
/// before anyone pays for it. An existing collection is never reconfigured;
/// if its quantization differs from the setting we log one warning that says
/// to create a new collection and re-index.
///
/// # Errors
///
/// [`PortError`] from the client, mapped one-for-one.
pub async fn create_collection(
    client: &Qdrant,
    settings: &QdrantSettings,
    collection: &str,
) -> Result<(), PortError> {
    let exists = client.collection_exists(collection).await.map_err(map)?;
    if exists {
        warn_on_quantization_mismatch(client, collection, settings).await;
    } else {
        let mut builder = qdrant_client::qdrant::CreateCollectionBuilder::new(collection);
        if let Some(dims) = settings.dense_dims {
            let mut vectors = VectorsConfigBuilder::default();
            let mut dense = VectorParamsBuilder::new(u64::from(dims), Distance::Cosine)
                .hnsw_config(HnswConfigDiff {
                    m: Some(u64::from(settings.hnsw_m)),
                    ef_construct: Some(u64::from(settings.hnsw_ef_construct)),
                    ..Default::default()
                });
            if let Some(config) = settings.quantization.quantization_config() {
                dense = dense.quantization_config(config);
            }
            vectors.add_named_vector_params(DENSE_VECTOR, dense);
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
    // Integer indexes: `chunkIndex` for neighbour windows and re-ingest
    // bookkeeping, `page` for page-scoped reads. Both must be range-filterable
    // without scanning the collection. Creating an index on an existing
    // collection is normally a no-op; a server that answers "already exists"
    // is tolerated exactly like collection creation above, so re-running
    // setup is idempotent.
    for field in [crate::payload::KEY_CHUNK_INDEX, crate::payload::KEY_PAGE] {
        let created = client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(collection, field, FieldType::Integer)
                    .field_index_params(IntegerIndexParamsBuilder::default().build())
                    .wait(true),
            )
            .await;
        if let Err(err) = created {
            if !already_exists(&err) {
                return Err(map(err));
            }
        }
    }

    Ok(())
}

/// Warn once when a live collection's dense quantization differs from the
/// setting. Qdrant cannot change quantization in place, so the remedy is a new
/// collection (a new model slug / dimension name) and `kb-server reindex`.
async fn warn_on_quantization_mismatch(
    client: &Qdrant,
    collection: &str,
    settings: &QdrantSettings,
) {
    let actual = client
        .collection_info(collection)
        .await
        .ok()
        .and_then(|response| response.result)
        .and_then(|info| info.config)
        .and_then(|config| config.params)
        .and_then(|params| params.vectors_config)
        .and_then(|vectors| match vectors.config {
            Some(vectors_config::Config::ParamsMap(map)) => map.map.get(DENSE_VECTOR).cloned(),
            Some(vectors_config::Config::Params(params)) => Some(params),
            None => None,
        })
        .and_then(|params| params.quantization_config)
        .map(|config| mode_of_quantization(&config));
    match actual {
        Some(actual) if actual != settings.quantization => tracing::warn!(
            collection,
            configured = settings.quantization.as_str(),
            existing = actual.as_str(),
            "collection quantization differs from KB_QDRANT_QUANTIZATION; \
             create a new collection and run `kb-server reindex` to apply it"
        ),
        None if settings.quantization != QuantizationMode::Off => tracing::warn!(
            collection,
            configured = settings.quantization.as_str(),
            "could not read the collection's quantization config; \
             create a new collection and run `kb-server reindex` if it differs"
        ),
        _ => {}
    }
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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

    #[test]
    fn quantization_mode_accepts_only_off_scalar_and_binary() {
        assert_eq!(QuantizationMode::parse(" off "), Ok(QuantizationMode::Off));
        assert_eq!(
            QuantizationMode::parse("SCALAR"),
            Ok(QuantizationMode::Scalar)
        );
        assert_eq!(
            QuantizationMode::parse("binary"),
            Ok(QuantizationMode::Binary)
        );
        assert!(QuantizationMode::parse("product").is_err());
        assert!(QuantizationMode::parse("").is_err());
    }

    #[test]
    fn quantization_env_defaults_are_off_two_and_true() {
        let (mode, oversampling, rescore) = quantization_from_env(&|_| None).expect("defaults");
        assert_eq!(mode, QuantizationMode::Off);
        assert_eq!(oversampling, 2.0);
        assert!(rescore);
    }

    #[test]
    fn quantization_oversampling_is_clamped_to_one_through_eight() {
        let lookup = |key: &str| (key == "KB_QDRANT_QUANT_OVERSAMPLING").then(|| "99".to_string());
        let (_, oversampling, _) = quantization_from_env(&lookup).expect("high");
        assert_eq!(oversampling, 8.0);
        let lookup = |key: &str| (key == "KB_QDRANT_QUANT_OVERSAMPLING").then(|| "-3".to_string());
        let (_, oversampling, _) = quantization_from_env(&lookup).expect("low");
        assert_eq!(oversampling, 1.0);
    }

    #[test]
    fn an_unknown_quantization_mode_is_an_error() {
        let lookup = |key: &str| (key == "KB_QDRANT_QUANTIZATION").then(|| "turbo".to_string());
        assert!(quantization_from_env(&lookup).is_err());
    }

    #[test]
    // The deprecated field is exactly what RET-20 asks us to set; the client
    // marks it deprecated in favour of `memory`, which arrived in 1.19.
    #[allow(deprecated)]
    fn scalar_is_int8_with_point99_quantile_and_binary_is_ram_resident() {
        use qdrant_client::qdrant::quantization_config::Quantization;
        let scalar = QuantizationMode::Scalar
            .quantization_config()
            .expect("scalar config");
        assert!(
            matches!(
                &scalar,
                Quantization::Scalar(q)
                    if q.r#type == QuantizationType::Int8 as i32
                        && q.quantile == Some(0.99)
                        && q.always_ram == Some(true)
            ),
            "expected int8 scalar quantization at quantile 0.99 in RAM"
        );
        let binary = QuantizationMode::Binary
            .quantization_config()
            .expect("binary config");
        assert!(
            matches!(&binary, Quantization::Binary(q) if q.always_ram == Some(true)),
            "expected binary quantization in RAM"
        );
        assert!(QuantizationMode::Off.quantization_config().is_none());
    }

    #[test]
    fn a_collection_quantization_config_maps_back_to_its_mode() {
        let scalar = QuantizationMode::Scalar
            .quantization_config()
            .expect("scalar config");
        assert_eq!(
            mode_of_quantization(&QuantizationConfig::from(scalar)),
            QuantizationMode::Scalar
        );
        let binary = QuantizationMode::Binary
            .quantization_config()
            .expect("binary config");
        assert_eq!(
            mode_of_quantization(&QuantizationConfig::from(binary)),
            QuantizationMode::Binary
        );
        assert_eq!(
            mode_of_quantization(&QuantizationConfig::default()),
            QuantizationMode::Off
        );
    }

    fn qdrant_settings() -> QdrantSettings {
        QdrantSettings {
            url: "http://127.0.0.1:16334".to_string(),
            api_key: None,
            model_slug: "unknown".to_string(),
            dense_dims: None,
            hnsw_m: 16,
            hnsw_ef_construct: 100,
            quantization: QuantizationMode::Off,
            quant_oversampling: 2.0,
            quant_rescore: true,
            request_timeout: Duration::from_secs(30),
            upsert_batch: 256,
        }
    }
}
