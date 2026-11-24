//! Stateless processor-only HTTP mode (`kb-server serve --stateless` or
//! `KB_MODE=stateless`).
//!
//! Runs `rantai-kb` as a pure document-processing microservice with **zero**
//! Postgres, Qdrant, or S3 connections. Callers that own their own database
//! and Qdrant server upload files to `POST /v1/process` and receive extracted
//! text, smart chunks, structured tables, figure anchors (and optional PNG
//! bytes), Qdrant-ready BM25 sparse vectors, optional dense embeddings, and
//! optional entities/relations in one synchronous JSON response.
//!
//! `POST /v1/encode` encodes query or passage strings with the exact same
//! FNV-1a BM25 sparse tokenizer and dense embedding model/prefixes so external
//! Qdrant retrieval matches `rantai-kb`'s indexing space bit-for-bit.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::extract::{DefaultBodyLimit, FromRequest, FromRequestParts, Multipart, Request, State};
use axum::http::header::RETRY_AFTER;
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use kb_core::policy::{FigureMode, parse_figure_mode};
use kb_engine::{
    CancelToken, DocumentProcessor, EncodeMode, ProcessError, ProcessRequest,
    ProcessorCapabilities, ProcessorSettings,
};
use kb_extract::ExtractConfig;
use kb_inference::ExtractorSettings;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use utoipa::{OpenApi, ToSchema};

use crate::auth::extract_raw_key;
use crate::backends::{BackendHealth, BackendProbe, BackendTargets, Endpoint};
use crate::config::{
    CacheConfig, Config, EmbedderConfig, EntityExtractorChoice, RerankerConfig, bool_flag,
    extract_outbound_urls, int_var, read_reranker, read_seats,
};
use crate::error::{ApiError, ErrorBody};
use crate::openapi::{SecurityAddon, VersionAddon};
use crate::routes::structure::SectionDto;
use crate::routes::upload::is_accepted_extension;
use crate::routes::{
    FormatsResponse, RouteSpec, cors, max_file_bytes, payload_too_large_message, upload_body_limit,
};
use crate::state::{build_reranker, embedder_settings_from, reranker_fingerprint};

// ─── Auth ────────────────────────────────────────────────────────────────────

/// Authentication policy for stateless mode.
///
/// Because stateless mode has no Postgres `ApiKey` table, callers authenticate
/// against static keys from `KB_STATELESS_API_KEY` (comma-separated, compared
/// in constant time on their SHA-256 digests). Unauthenticated mode requires
/// explicit opt-in via `KB_STATELESS_ALLOW_UNAUTHENTICATED=true` (for private
/// VPC / sidecar deployments).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatelessAuth {
    /// One or more SHA-256 digests of accepted bearer / `X-API-Key` tokens.
    Keys(Vec<[u8; 32]>),
    /// Explicitly opted into unauthenticated access on a trusted network.
    Unauthenticated,
}

impl StatelessAuth {
    /// Build a key-checking policy from raw plaintext keys.
    #[must_use]
    pub fn from_keys<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let digests = keys
            .into_iter()
            .map(|k| k.as_ref().trim().to_string())
            .filter(|k| !k.is_empty())
            .map(|k| sha256_bytes(k.as_bytes()))
            .collect();
        Self::Keys(digests)
    }

    /// Verify a presented raw token.
    #[must_use]
    pub fn verify(&self, candidate: Option<&str>) -> bool {
        match self {
            Self::Unauthenticated => true,
            Self::Keys(digests) => {
                let Some(raw) = candidate else {
                    return false;
                };
                let candidate_hash = sha256_bytes(raw.as_bytes());
                // Check every configured key without short-circuiting so timing
                // does not leak which slot matched.
                let mut matched = 0u8;
                for digest in digests {
                    if ct_eq_32(&candidate_hash, digest) {
                        matched |= 1;
                    }
                }
                matched == 1
            }
        }
    }
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Axum extractor enforcing [`StatelessAuth`].
pub struct StatelessAuthed;

#[axum::async_trait]
impl FromRequestParts<Arc<StatelessState>> for StatelessAuthed {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<StatelessState>,
    ) -> Result<Self, Self::Rejection> {
        let raw = extract_raw_key(parts);
        if state.auth.verify(raw) {
            Ok(Self)
        } else {
            Err(ApiError::Unauthorized)
        }
    }
}

// ─── Configuration ───────────────────────────────────────────────────────────

/// Typed configuration for stateless processor-only mode.
///
/// Requires zero database, Qdrant, or S3 settings. An embedder is optional:
/// when no embedding configuration is present, extraction, chunking, tables,
/// figures, and BM25 sparse vectors still work with zero external model calls.
#[derive(Clone, Debug)]
pub struct StatelessConfig {
    pub bind_addr: String,
    pub auth: StatelessAuth,
    pub extract: ExtractConfig,
    pub embedder: Option<EmbedderConfig>,
    pub reranker: RerankerConfig,
    pub entity: ExtractorSettings,
    pub entity_extractor: EntityExtractorChoice,
    pub seats: kb_engine::SeatCapacity,
    /// Max concurrent `/v1/process` requests admitted before shedding with 503.
    /// `0` disables the cap. Defaults to `max(seats.extract * 4, 4)`.
    pub max_inflight_process: usize,
    pub chunk_size: u32,
    pub chunk_overlap: u32,
    pub strip_page_furniture: bool,
    pub embedding_max_chars: usize,
    pub quality: Option<kb_core::quality::QualityOptions>,
    pub cache: Option<CacheConfig>,
    pub version: String,
    pub tier: Option<crate::tier::TierPlan>,
}

impl StatelessConfig {
    /// Read stateless configuration from process environment variables.
    ///
    /// # Errors
    ///
    /// Fails if auth is unconfigured or any numeric/enum env var is invalid.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Read stateless configuration from a lookup function.
    ///
    /// # Errors
    ///
    /// Fails if neither `KB_STATELESS_API_KEY` nor
    /// `KB_STATELESS_ALLOW_UNAUTHENTICATED=true` is set, or if a configured
    /// setting is invalid.
    pub fn from_lookup<F>(lookup: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let tier = crate::tier::Tier::from_lookup(&lookup)?;
        if tier.is_some() {
            crate::tier::Embedders::from_lookup(&lookup)?;
            crate::tier::EmbedPrecision::from_lookup(&lookup)?;
        }
        let tier_plan = tier.map(|t| crate::tier::TierPlan::resolve(t, &lookup));
        let lookup = crate::tier::with_preset(tier, &lookup);

        // Auth: static key(s) or explicit unauthenticated opt-in.
        let raw_keys: Vec<String> = lookup("KB_STATELESS_API_KEY")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let allow_unauth = bool_flag(&lookup, "KB_STATELESS_ALLOW_UNAUTHENTICATED", false);
        let auth = if !raw_keys.is_empty() {
            StatelessAuth::from_keys(raw_keys)
        } else if allow_unauth {
            StatelessAuth::Unauthenticated
        } else {
            bail!(
                "stateless mode requires authentication: set KB_STATELESS_API_KEY=<secret> \
                 (comma-separated for multiple keys), or explicitly opt out on a private network \
                 with KB_STATELESS_ALLOW_UNAUTHENTICATED=true"
            );
        };

        let port: u16 = lookup("PORT").and_then(|v| v.parse().ok()).unwrap_or(8080);
        let bind_addr = format!("0.0.0.0:{port}");

        // Optional embedder. Off when `KB_STATELESS_EMBED=false`, or when no
        // tier / base URL / API key is present.
        let embed_enabled = bool_flag(&lookup, "KB_STATELESS_EMBED", true);
        let embedder = if embed_enabled {
            read_optional_embedder(&lookup, tier)?
        } else {
            None
        };

        // Optional reranker. Mirrors stateful `KB_RERANK_*` and `KB_TIER` presets
        // (including cloud Cohere/Voyage/OpenRouter and local TEI/vLLM/llama.cpp).
        // Set `KB_STATELESS_RERANK=false` to disable even when a tier preset enables it.
        let rerank_enabled = bool_flag(&lookup, "KB_STATELESS_RERANK", true);
        let reranker = if rerank_enabled {
            read_reranker(&lookup)?
        } else {
            RerankerConfig::Disabled
        };

        let extract = ExtractConfig::from_lookup(&lookup);
        let entity = ExtractorSettings::from_lookup(&lookup);
        // Default to free local heuristic extractor in stateless mode so
        // opt-in `entities=true` works without an LLM key (AGENTS rule 7).
        let entity_extractor = match lookup("KB_ENTITY_EXTRACTOR")
            .map(|v| v.trim().to_lowercase())
            .as_deref()
        {
            None | Some("") | Some("heuristic") | Some("local") | Some("free") => {
                EntityExtractorChoice::Heuristic
            }
            Some("llm") | Some("openrouter") => EntityExtractorChoice::Llm,
            Some(other) => {
                bail!("KB_ENTITY_EXTRACTOR must be \"heuristic\" or \"llm\", got {other:?}")
            }
        };

        let seats = read_seats(&lookup)?;
        let default_max_inflight = seats.extract.saturating_mul(4).max(4);
        let max_inflight_process =
            int_var(&lookup, "KB_STATELESS_MAX_INFLIGHT", default_max_inflight)?;
        let chunk_size = int_var(&lookup, "KB_CHUNK_SIZE", 800u32)?.max(100);
        let chunk_overlap = int_var(&lookup, "KB_CHUNK_OVERLAP", 200u32)?;
        let strip_page_furniture = bool_flag(&lookup, "KB_STRIP_PAGE_FURNITURE", false);
        let embedding_max_chars = int_var(&lookup, "KB_EMBEDDING_MAX_CHARS", 0usize)?;
        let quality = extract.quality_options();

        let cache = lookup("KB_CACHE_DIR")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(|dir| CacheConfig {
                dir: dir.into(),
                embedding_tag: lookup("KB_EMBEDDING_CACHE_TAG").unwrap_or_default(),
            });

        let version = lookup("KB_VERSION")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| crate::state::DEFAULT_VERSION.to_string());

        Ok(Self {
            bind_addr,
            auth,
            extract,
            embedder,
            reranker,
            entity,
            entity_extractor,
            seats,
            max_inflight_process,
            chunk_size,
            chunk_overlap,
            strip_page_furniture,
            embedding_max_chars,
            quality,
            cache,
            version,
            tier: tier_plan,
        })
    }

    /// Print one INFO line describing what `KB_TIER` filled, when a tier is set.
    pub fn log_tier(&self) {
        if let Some(plan) = self.tier.as_ref() {
            plan.log();
        }
    }

    /// Outbound model/OCR URLs this stateless config may contact.
    #[must_use]
    pub fn outbound_content_urls(&self) -> Vec<String> {
        let mut urls = Vec::new();
        if let Some(embedder) = &self.embedder {
            urls.push(embedder.base_url.clone());
        }
        match &self.reranker {
            RerankerConfig::Disabled => {}
            RerankerConfig::Llm { .. } => {
                urls.push(kb_inference::OPENROUTER_CHAT_URL.to_string());
            }
            RerankerConfig::Tei { base_url, .. } | RerankerConfig::Vllm { base_url, .. } => {
                urls.push(base_url.clone());
            }
            RerankerConfig::Cohere { endpoint, .. } => urls.push(
                endpoint
                    .clone()
                    .unwrap_or_else(|| kb_inference::DEFAULT_COHERE_ENDPOINT.to_string()),
            ),
        }
        urls.extend(extract_outbound_urls(&self.extract));
        if self.entity_extractor == EntityExtractorChoice::Llm
            && !self.entity.api_key.trim().is_empty()
        {
            urls.push(self.entity.base_url.clone());
        }
        urls
    }

    /// Non-local hosts reported by `GET /health` as `egress`.
    #[must_use]
    pub fn egress_hosts(&self) -> Vec<String> {
        crate::egress::non_local_hosts(self.outbound_content_urls())
    }

    /// Backend probe targets for `GET /health`.
    #[must_use]
    pub fn backend_targets(&self) -> BackendTargets {
        let embedder = self
            .embedder
            .as_ref()
            .map(|e| Endpoint::with_key(e.base_url.clone(), e.api_key.clone().unwrap_or_default()));
        let reranker = match &self.reranker {
            RerankerConfig::Disabled => None,
            RerankerConfig::Llm { api_key, .. } => Some(Endpoint::with_key(
                kb_inference::OPENROUTER_CHAT_URL,
                api_key.clone(),
            )),
            RerankerConfig::Tei { base_url, .. } | RerankerConfig::Vllm { base_url, .. } => Some(
                Endpoint::anonymous(format!("{}/rerank", base_url.trim_end_matches('/'))),
            ),
            RerankerConfig::Cohere {
                api_key, endpoint, ..
            } => Some(Endpoint::with_key(
                endpoint
                    .as_deref()
                    .unwrap_or(kb_inference::DEFAULT_COHERE_ENDPOINT),
                api_key.clone(),
            )),
        };
        let ocr = if !self.extract.ocr_base_url.trim().is_empty() {
            Some(Endpoint::with_key(
                self.extract.ocr_base_url.clone(),
                self.extract.ocr_api_key.clone(),
            ))
        } else if !self.extract.mistral_ocr_key.trim().is_empty() {
            Some(Endpoint::with_key(
                format!(
                    "{}/v1/chat/completions",
                    self.extract.mistral_ocr_base.trim_end_matches('/')
                ),
                self.extract.mistral_ocr_key.clone(),
            ))
        } else {
            None
        };
        BackendTargets {
            embedder,
            reranker,
            ocr,
        }
    }
}

fn reranker_model_name(cfg: &RerankerConfig) -> Option<String> {
    match cfg {
        RerankerConfig::Disabled => None,
        RerankerConfig::Llm { model, .. }
        | RerankerConfig::Tei { model, .. }
        | RerankerConfig::Vllm { model, .. }
        | RerankerConfig::Cohere { model, .. } => Some(model.clone()),
    }
}

fn read_optional_embedder<F>(
    lookup: &F,
    tier: Option<crate::tier::Tier>,
) -> Result<Option<EmbedderConfig>>
where
    F: Fn(&str) -> Option<String>,
{
    let explicit_base_url = lookup("KB_EMBEDDING_BASE_URL").filter(|s| !s.trim().is_empty());
    let explicit_embed_key = lookup("KB_EMBEDDING_API_KEY").filter(|s| !s.trim().is_empty());
    let openrouter_key = lookup("OPENROUTER_API_KEY").filter(|s| !s.trim().is_empty());

    let api_key = explicit_embed_key.clone().or_else(|| {
        let allows_openrouter = match tier {
            Some(t) => t.needs_openrouter_key(),
            None => explicit_base_url
                .as_deref()
                .is_none_or(|url| url.to_ascii_lowercase().contains("openrouter.ai")),
        };
        if allows_openrouter {
            openrouter_key.clone()
        } else {
            None
        }
    });

    // Decide whether an embedder is configured at all.
    let should_wire = match tier {
        Some(crate::tier::Tier::Cloud) => {
            if api_key.is_none() {
                bail!(
                    "KB_TIER=cloud needs OPENROUTER_API_KEY (or KB_EMBEDDING_API_KEY) for \
                     embeddings; set one, or set KB_STATELESS_EMBED=false to run without dense \
                     embeddings"
                );
            }
            true
        }
        Some(_) => true,
        None => explicit_base_url.is_some() || api_key.is_some(),
    };

    if !should_wire {
        return Ok(None);
    }

    let base_url =
        explicit_base_url.unwrap_or_else(|| "https://openrouter.ai/api/v1/embeddings".to_string());
    let model = lookup("KB_EMBEDDING_MODEL")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "qwen/qwen3-embedding-8b".to_string());
    let dimension = Config::embedding_dim(lookup);

    let embed_concurrency = if lookup("KB_EMBEDDING_CONCURRENCY").is_some() {
        int_var(lookup, "KB_EMBEDDING_CONCURRENCY", 4u32)?
    } else {
        int_var(lookup, "KB_EMBED_CONCURRENCY", 4u32)?
    }
    .clamp(1, 32);

    let format = match lookup("KB_EMBEDDING_FORMAT")
        .unwrap_or_else(|| "openai".into())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "openai" | "" => kb_inference::EmbedFormat::OpenAi,
        "omniembed" | "v2" => kb_inference::EmbedFormat::OmniEmbed,
        other => bail!("KB_EMBEDDING_FORMAT must be openai or omniembed, got {other:?}"),
    };

    Ok(Some(EmbedderConfig {
        base_url,
        model,
        api_key,
        dimension,
        batch_size: int_var(lookup, "KB_EMBEDDING_BATCH_SIZE", 32)?.max(1),
        embed_concurrency,
        query_prefix: lookup("KB_EMBEDDING_QUERY_PREFIX").unwrap_or_default(),
        passage_prefix: lookup("KB_EMBEDDING_PASSAGE_PREFIX").unwrap_or_default(),
        send_input_type: bool_flag(lookup, "KB_EMBEDDING_INPUT_TYPE", false),
        format,
        request_timeout: Duration::from_millis(
            int_var(lookup, "KB_EMBEDDING_TIMEOUT_MS", 15_000u64)?.max(1_000),
        ),
    }))
}

// ─── State ───────────────────────────────────────────────────────────────────

/// Shared state for stateless HTTP handlers. Zero database, Qdrant, or S3 clients.
pub struct StatelessState {
    pub processor: DocumentProcessor,
    pub auth: StatelessAuth,
    pub version: String,
    pub tier: Option<String>,
    pub egress: Vec<String>,
    pub backends: BackendProbe,
    pub limits: StatelessLimitsDto,
    inflight_process: Option<Arc<Semaphore>>,
}

impl StatelessState {
    /// Wire the production extractor, optional embedder, optional reranker,
    /// optional disk caches, and entity extractor from a [`StatelessConfig`].
    ///
    /// # Errors
    ///
    /// Fails if the extractor, configured embedder, reranker, or LLM entity
    /// extractor cannot be initialised.
    pub fn from_config(config: StatelessConfig) -> Result<Self> {
        let tier = config
            .tier
            .as_ref()
            .map(|plan| plan.tier.as_str().to_string());
        let egress = config.egress_hosts();
        let backends = BackendProbe::new(config.backend_targets());
        let limits = StatelessLimitsDto {
            extract_seats: config.seats.extract,
            embed_seats: config.seats.embed,
            max_inflight_process: config.max_inflight_process,
        };
        let inflight_process = (config.max_inflight_process > 0)
            .then(|| Arc::new(Semaphore::new(config.max_inflight_process)));

        let mut extractor = kb_extract::build_production_extractor_with_cache(
            &config.extract,
            config.cache.as_ref().map(|cache| {
                Arc::new(kb_cache::DiskOcrPageCache::new(kb_cache::DiskCache::new(
                    &cache.dir,
                ))) as Arc<dyn kb_ports::OcrPageCache>
            }),
        )
        .map_err(|e| anyhow::anyhow!("extractor: {e}"))?
        .shared();
        if let Some(cache) = config.cache.as_ref() {
            let fingerprint = config.extract.cache_fingerprint();
            tracing::info!(dir = %cache.dir.display(), "extraction disk cache on");
            extractor = Arc::new(kb_cache::CachedExtractor::new(
                extractor,
                kb_cache::DiskCache::new(&cache.dir),
                fingerprint,
            ));
        }

        let embedding_model = config.embedder.as_ref().map(|e| e.model.clone());
        let settings = ProcessorSettings {
            chunking: kb_core::chunk::SmartChunkingOptions {
                max_chunk_size: Some(config.chunk_size),
                overlap_size: Some(config.chunk_overlap),
                ..kb_engine::INGEST_CHUNKING
            },
            strip_page_furniture: config.strip_page_furniture,
            embedding_model,
            embedding_max_chars: config.embedding_max_chars,
            seats: config.seats,
            quality: config.quality,
        };

        let mut processor = DocumentProcessor::new(extractor, settings);

        if let Some(embed_cfg) = config.embedder {
            let mut embedder: Arc<dyn kb_ports::Embedder> = Arc::new(
                kb_inference::EmbedderImpl::new(embedder_settings_from(embed_cfg.clone()))
                    .map_err(|e| anyhow::anyhow!("embedder: {e}"))?,
            );
            if let Some(cache) = config.cache.as_ref() {
                let fingerprint = embed_cfg.cache_fingerprint(&cache.embedding_tag);
                let (query_prefix, passage_prefix) = embed_cfg.effective_prefixes();
                tracing::info!(dir = %cache.dir.display(), %fingerprint, "embedding disk cache on");
                embedder = Arc::new(
                    kb_cache::CachedEmbedder::new(
                        embedder,
                        kb_cache::DiskCache::new(&cache.dir),
                        fingerprint,
                    )
                    .with_prefixes(query_prefix, passage_prefix),
                );
            }
            processor = processor.with_embedder(embedder);
        }

        if !matches!(config.reranker, RerankerConfig::Disabled) {
            let mut reranker: Arc<dyn kb_ports::Reranker> =
                build_reranker(&config.reranker).map_err(|e| anyhow::anyhow!("reranker: {e}"))?;
            if let (Some(cache), Some(fingerprint)) = (
                config.cache.as_ref(),
                reranker_fingerprint(&config.reranker),
            ) {
                tracing::info!(dir = %cache.dir.display(), %fingerprint, "rerank disk cache on");
                reranker = Arc::new(kb_cache::CachedReranker::new(
                    reranker,
                    kb_cache::DiskCache::new(&cache.dir),
                    fingerprint,
                ));
            }
            processor = processor.with_reranker(reranker, reranker_model_name(&config.reranker));
        }

        let entity_extractor: Arc<dyn kb_ports::EntityExtractor> = match config.entity_extractor {
            EntityExtractorChoice::Heuristic => {
                Arc::new(kb_inference::HeuristicExtractor::default())
            }
            EntityExtractorChoice::Llm => Arc::new(
                kb_inference::LlmCombinedExtractor::new(config.entity)
                    .map_err(|e| anyhow::anyhow!("entity extractor: {e}"))?,
            ),
        };
        processor = processor.with_entity_extractor(entity_extractor);

        Ok(Self {
            processor,
            auth: config.auth,
            version: config.version,
            tier,
            egress,
            backends,
            limits,
            inflight_process,
        })
    }

    /// Construct a stateless state from explicit components (used in tests).
    #[must_use]
    pub fn from_parts(processor: DocumentProcessor, auth: StatelessAuth, version: String) -> Self {
        let seats = kb_engine::SeatCapacity::default();
        let default_max_inflight = seats.extract.saturating_mul(4).max(4);
        Self {
            processor,
            auth,
            version,
            tier: None,
            egress: Vec::new(),
            backends: BackendProbe::default(),
            limits: StatelessLimitsDto {
                extract_seats: seats.extract,
                embed_seats: seats.embed,
                max_inflight_process: default_max_inflight,
            },
            inflight_process: Some(Arc::new(Semaphore::new(default_max_inflight))),
        }
    }

    /// Override the maximum concurrent `/v1/process` admission cap (`0` = unlimited).
    #[must_use]
    pub fn with_max_inflight(mut self, max_inflight: usize) -> Self {
        self.limits.max_inflight_process = max_inflight;
        self.inflight_process = (max_inflight > 0).then(|| Arc::new(Semaphore::new(max_inflight)));
        self
    }
}

// ─── Route Table & Router ────────────────────────────────────────────────────

/// Every route exposed in stateless mode (checked against [`StatelessApiDoc`]).
pub const STATELESS_ROUTES: &[RouteSpec] = &[
    RouteSpec {
        method: Method::GET,
        path: "/health",
        tag: "health",
    },
    RouteSpec {
        method: Method::GET,
        path: "/v1/health",
        tag: "health",
    },
    RouteSpec {
        method: Method::GET,
        path: "/v1/formats",
        tag: "health",
    },
    RouteSpec {
        method: Method::POST,
        path: "/v1/process",
        tag: "process",
    },
    RouteSpec {
        method: Method::POST,
        path: "/v1/encode",
        tag: "encode",
    },
    RouteSpec {
        method: Method::POST,
        path: "/v1/rerank",
        tag: "rerank",
    },
];

/// Build the Axum router for stateless processor-only mode.
pub fn build_stateless_router(state: StatelessState) -> Router {
    let shared = Arc::new(state);
    let public = Router::new()
        .route("/health", axum::routing::get(stateless_health))
        .route("/v1/health", axum::routing::get(stateless_health_v1))
        .route("/v1/formats", axum::routing::get(stateless_formats))
        .merge(stateless_openapi_router());

    let api = Router::new()
        .route("/v1/process", axum::routing::post(process_document))
        .route("/v1/encode", axum::routing::post(encode_texts))
        .route("/v1/rerank", axum::routing::post(rerank_documents))
        .layer(DefaultBodyLimit::max(upload_body_limit()));

    public
        .merge(api)
        .fallback(any(stateless_not_found))
        .layer(axum::middleware::from_fn(cors))
        .with_state(shared)
}

/// Bind and serve stateless mode until SIGINT or SIGTERM arrives.
///
/// # Errors
///
/// Fails if the bind address is invalid, state wiring fails, or the listener
/// cannot bind.
pub async fn run_stateless(config: StatelessConfig) -> Result<()> {
    let bind_addr: SocketAddr = config.bind_addr.parse().context("invalid bind address")?;
    let has_embedder = config.embedder.is_some();
    let has_reranker = !matches!(config.reranker, RerankerConfig::Disabled);
    let max_inflight = config.max_inflight_process;
    let unauth = config.auth == StatelessAuth::Unauthenticated;
    let state = StatelessState::from_config(config).context("stateless state")?;
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .context("could not bind")?;
    tracing::info!(
        mode = "stateless",
        embedder = has_embedder,
        reranker = has_reranker,
        max_inflight_process = max_inflight,
        unauthenticated = unauth,
        "starting kb-server in stateless processor-only mode (no Postgres, Qdrant, or S3)"
    );
    crate::runtime::serve(
        build_stateless_router(state),
        listener,
        None,
        None,
        None,
        CancelToken::new(),
    )
    .await
}

async fn stateless_not_found(State(state): State<Arc<StatelessState>>, req: Request) -> Response {
    let (mut parts, _body) = req.into_parts();
    if StatelessAuthed::from_request_parts(&mut parts, &state)
        .await
        .is_err()
    {
        return ApiError::Unauthorized.into_response();
    }
    ApiError::NotFound.into_response()
}

// ─── Wire DTOs & OpenAPI Schemas ─────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct StatelessHealthChecks {
    /// Whether the in-process document extractor is ready.
    pub extractor: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CapabilitiesDto {
    pub embed: bool,
    pub rerank: bool,
    pub entities: bool,
    pub sparse: bool,
    pub tables: bool,
    pub figures: bool,
}

impl From<ProcessorCapabilities> for CapabilitiesDto {
    fn from(c: ProcessorCapabilities) -> Self {
        Self {
            embed: c.embed,
            rerank: c.rerank,
            entities: c.entities,
            sparse: c.sparse,
            tables: c.tables,
            figures: c.figures,
        }
    }
}

/// Active concurrency seat and admission caps on this stateless instance.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatelessLimitsDto {
    /// Max concurrent local extraction jobs (`KB_SEATS` extract).
    pub extract_seats: usize,
    /// Max concurrent bulk embedding jobs (`KB_SEATS` embed).
    pub embed_seats: usize,
    /// Max concurrent `/v1/process` uploads admitted before shedding with `503 Retry-After` (`KB_STATELESS_MAX_INFLIGHT`; `0` = unlimited).
    pub max_inflight_process: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StatelessHealthResponse {
    /// True when the processor is ready to accept documents.
    pub ok: bool,
    /// Always `"stateless"`.
    pub mode: &'static str,
    /// Component checks (no database or vector store in stateless mode).
    pub checks: StatelessHealthChecks,
    /// Which processing stages are wired on this instance.
    pub capabilities: CapabilitiesDto,
    /// Active seat and in-flight admission limits on this instance.
    pub limits: StatelessLimitsDto,
    /// Server version (`KB_VERSION`).
    pub version: String,
    /// Active `KB_TIER` preset, if any.
    pub tier: Option<String>,
    /// Non-local model/OCR hosts configured on this instance.
    pub egress: Vec<String>,
    /// Live status of optional remote/local model backends (embedder, reranker, OCR).
    pub backends: BackendHealth,
}

/// Multipart form accepted by `POST /v1/process`.
#[derive(Debug, ToSchema)]
pub struct ProcessForm {
    /// Document file to process. Accepted extensions are listed by `GET /v1/formats`.
    #[schema(value_type = String, format = Binary)]
    pub file: String,
    /// Optional document title override. Defaults to extracted title or filename stem.
    #[schema(value_type = Option<String>)]
    pub title: Option<String>,
    /// Optional category label echoed on every returned chunk.
    #[schema(value_type = Option<String>)]
    pub category: Option<String>,
    /// Optional subcategory label echoed on every returned chunk.
    #[schema(value_type = Option<String>)]
    pub subcategory: Option<String>,
    /// Figure extraction mode: `auto` (default), `all`, `none`, or `off`.
    #[schema(value_type = Option<String>)]
    pub figures: Option<String>,
    /// Optional extractor hint (e.g. `invoice`, `contract`).
    #[schema(value_type = Option<String>, rename = "documentType")]
    pub document_type: Option<String>,
    /// Compute dense embeddings (`true`/`false`). Defaults to `true` when an embedder is configured.
    #[schema(value_type = Option<bool>)]
    pub embed: Option<bool>,
    /// Compute Qdrant-compatible BM25 sparse vectors (`true`/`false`, default `true`).
    #[schema(value_type = Option<bool>)]
    pub sparse: Option<bool>,
    /// Extract structured tables and `table_summary`/`table_row` chunks (`true`/`false`, default `true`).
    #[schema(value_type = Option<bool>)]
    pub tables: Option<bool>,
    /// Extract entities and relations (`true`/`false`, default `false`).
    #[schema(value_type = Option<bool>)]
    pub entities: Option<bool>,
    /// Include base64-encoded PNG bytes for each extracted figure (`true`/`false`, default `false`).
    #[schema(value_type = Option<bool>, rename = "includeFigureBytes")]
    pub include_figure_bytes: Option<bool>,
    /// Optional chunk size override in characters (floor 100).
    #[schema(value_type = Option<u32>, rename = "chunkSize")]
    pub chunk_size: Option<u32>,
    /// Optional chunk overlap override in characters.
    #[schema(value_type = Option<u32>, rename = "chunkOverlap")]
    pub chunk_overlap: Option<u32>,
}

/// Qdrant-ready sparse vector schema.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SparseVectorSchema {
    /// Sorted ascending FNV-1a hashed term indices.
    pub indices: Vec<u32>,
    /// Term frequencies matching `indices`.
    pub values: Vec<f32>,
}

/// Stage timing breakdown schema.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessTimingsSchema {
    pub extract_ms: u64,
    pub chunk_ms: u64,
    pub tables_ms: u64,
    pub figures_ms: u64,
    pub entities_ms: u64,
    pub embed_ms: u64,
    pub total_ms: u64,
}

/// Processed chunk schema for OpenAPI documentation.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessedChunkSchema {
    pub chunk_index: u32,
    pub content: String,
    pub chunk_type: String,
    pub section: Option<String>,
    pub heading_path: Option<String>,
    pub page: Option<u32>,
    pub category: String,
    pub subcategory: Option<String>,
    pub anchor_chunk_index: Option<u32>,
    pub figure_index: Option<u32>,
    pub table_index: Option<u32>,
    pub embedding: Option<Vec<f32>>,
    pub sparse_vector: Option<SparseVectorSchema>,
    /// Character span in the chunker's normalised source; absent when the
    /// chunk carries no span.
    pub char_start: Option<u32>,
    pub char_end: Option<u32>,
    pub overlap_prefix_len: Option<u32>,
}

/// One page's change-detection digest in the `POST /v1/process` response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessedPageSchema {
    /// Zero-based page index, matching `pageMap[].page`.
    pub page: u32,
    /// Lowercase hex SHA-256 of `pageMap[].text` (UTF-8 bytes).
    pub page_sha256: String,
}

/// Processed figure schema for OpenAPI documentation.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessedFigureSchema {
    pub index: u32,
    pub page: u32,
    pub kind: String,
    pub caption: String,
    pub bbox: [f32; 4],
    pub anchor_chunk_index: Option<u32>,
    pub png_base64: Option<String>,
}

/// Response schema of `POST /v1/process`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResponseSchema {
    pub title: String,
    pub file_type: String,
    pub sha256: String,
    pub file_size: usize,
    pub content: String,
    #[schema(value_type = Vec<Object>)]
    pub page_map: Vec<serde_json::Value>,
    /// Outline v2 version, always `2`.
    pub outline_version: u32,
    /// Reuses the `/v1/documents/:id/structure` section schema verbatim.
    pub sections: Vec<SectionDto>,
    /// Per-page digests for cheap change detection.
    pub pages: Vec<ProcessedPageSchema>,
    #[schema(value_type = Option<Object>)]
    pub outline: Option<serde_json::Value>,
    #[schema(value_type = Option<Object>)]
    pub ocr: Option<serde_json::Value>,
    #[schema(value_type = Object)]
    pub quality: serde_json::Value,
    pub degrades: Vec<String>,
    pub embedding_model: Option<String>,
    pub chunks: Vec<ProcessedChunkSchema>,
    #[schema(value_type = Vec<Object>)]
    pub tables: Vec<serde_json::Value>,
    pub figures: Vec<ProcessedFigureSchema>,
    #[schema(value_type = Vec<Object>)]
    pub entities: Vec<serde_json::Value>,
    #[schema(value_type = Vec<Object>)]
    pub relations: Vec<serde_json::Value>,
    pub timings: ProcessTimingsSchema,
}

/// JSON request body for `POST /v1/encode`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EncodeRequest {
    /// Non-empty list of strings to encode.
    pub texts: Vec<String>,
    /// `"query"` (default) or `"passage"`. Controls dense prefixes and BM25 query/document encoding.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub mode: Option<EncodeMode>,
    /// Compute dense embeddings. Defaults to `true` when an embedder is configured.
    #[serde(default)]
    pub dense: Option<bool>,
    /// Compute Qdrant-compatible BM25 sparse vectors (default `true`).
    #[serde(default)]
    pub sparse: Option<bool>,
}

/// Response schema of `POST /v1/encode`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EncodeResponseSchema {
    pub mode: String,
    pub embedding_model: Option<String>,
    pub embeddings: Option<Vec<Vec<f32>>>,
    pub sparse_vectors: Option<Vec<SparseVectorSchema>>,
}

/// JSON request body for `POST /v1/rerank`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RerankRequest {
    /// Search query to score each candidate document against.
    pub query: String,
    /// Non-empty list of candidate passages to rerank.
    pub documents: Vec<String>,
    /// Maximum number of top results to return (defaults to all candidates).
    #[serde(default)]
    pub top_k: Option<u32>,
    /// When `true`, echoes each candidate's text back on its result item (default `false`).
    #[serde(default)]
    pub return_documents: bool,
}

/// One scored document in a `POST /v1/rerank` response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RerankItemSchema {
    /// Zero-based index into the request's `documents` array.
    pub index: u32,
    /// Relevance score returned by the reranker (higher is better).
    pub score: f32,
    /// Zero-based rank in the returned ordering.
    pub rank: u32,
    /// Candidate text, present only when `returnDocuments` was `true`.
    pub document: Option<String>,
}

/// Response schema of `POST /v1/rerank`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RerankResponseSchema {
    pub model: Option<String>,
    pub results: Vec<RerankItemSchema>,
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// Liveness, readiness, and capability probe for stateless mode.
#[utoipa::path(
    get,
    path = "/health",
    tag = "health",
    responses(
        (status = 200, description = "Stateless processor is ready", body = StatelessHealthResponse),
    ),
    security(()),
)]
pub async fn stateless_health(State(state): State<Arc<StatelessState>>) -> Response {
    let backends = state.backends.health().await;
    let body = StatelessHealthResponse {
        ok: true,
        mode: "stateless",
        checks: StatelessHealthChecks { extractor: true },
        capabilities: state.processor.capabilities().into(),
        limits: state.limits,
        version: state.version.clone(),
        tier: state.tier.clone(),
        egress: state.egress.clone(),
        backends,
    };
    (StatusCode::OK, Json(body)).into_response()
}

/// Versioned liveness, readiness, and capability probe for stateless mode.
#[utoipa::path(
    get,
    path = "/v1/health",
    tag = "health",
    responses(
        (status = 200, description = "Stateless processor is ready", body = StatelessHealthResponse),
    ),
    security(()),
)]
pub async fn stateless_health_v1(State(state): State<Arc<StatelessState>>) -> Response {
    stateless_health(State(state)).await
}

/// Accepted upload formats and max file size.
#[utoipa::path(
    get,
    path = "/v1/formats",
    tag = "health",
    responses(
        (status = 200, description = "The accepted extensions and size limit", body = FormatsResponse),
    ),
    security(()),
)]
pub async fn stateless_formats() -> Json<FormatsResponse> {
    crate::routes::formats().await
}

/// Process a document synchronously without storing anything.
///
/// Extracts text, OCRs scanned pages when configured, splits into smart chunks,
/// structures tables, anchors figures, computes Qdrant-ready BM25 sparse
/// vectors, and optionally computes dense embeddings and entities/relations.
#[utoipa::path(
    post,
    path = "/v1/process",
    tag = "process",
    request_body(content = ProcessForm, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Structured processing result", body = ProcessResponseSchema),
        (status = 400, description = "Missing file or invalid option", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 413, description = "Uploaded file exceeds KB_MAX_FILE_BYTES", body = ErrorBody),
        (status = 415, description = "Unsupported file extension", body = ErrorBody),
        (status = 422, description = "Extraction failed or quality gate flagged document for OCR", body = ErrorBody),
        (status = 503, description = "Processor at max in-flight capacity (Retry-After) or configured model/OCR backend unavailable", body = ErrorBody),
        (status = 500, description = "Unexpected internal processing failure", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn process_document(
    State(state): State<Arc<StatelessState>>,
    _auth: StatelessAuthed,
    request: Request,
) -> Response {
    // Admission gate BEFORE buffering multipart bytes into RAM so bursts shed
    // cleanly with 503 + Retry-After instead of OOM-ing the VM.
    let _inflight_permit = if let Some(sem) = &state.inflight_process {
        match Arc::clone(sem).try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(RETRY_AFTER, "2")],
                    Json(ErrorBody {
                        error: format!(
                            "Processor is at capacity ({} in-flight requests; KB_STATELESS_MAX_INFLIGHT). Retry shortly.",
                            state.limits.max_inflight_process
                        ),
                    }),
                )
                    .into_response();
            }
        }
    } else {
        None
    };

    let upload = match read_stateless_upload(request, &state).await {
        Ok(u) => u,
        Err(err) => return err.into_response(),
    };

    if !is_accepted_extension(&upload.file_name) {
        return ApiError::UnsupportedMedia(format!(
            "\"{}\" has an unsupported type. See GET /v1/formats for the accepted list.",
            upload.file_name
        ))
        .into_response();
    }
    let max_bytes = max_file_bytes();
    let size = upload.bytes.len() as u64;
    if size > max_bytes {
        return ApiError::PayloadTooLarge(payload_too_large_message(
            Some(&upload.file_name),
            Some(size),
        ))
        .into_response();
    }

    let req = match build_process_request(upload) {
        Ok(r) => r,
        Err(err) => return err.into_response(),
    };

    match state.processor.process(req).await {
        Ok(output) => (StatusCode::OK, Json(output)).into_response(),
        Err(err) => process_error_response(err),
    }
}

/// Encode query or passage texts into dense and/or BM25 sparse vectors.
///
/// Uses the exact same FNV-1a sparse tokenizer and dense embedder/prefixes as
/// `POST /v1/process` so callers querying their own Qdrant server produce
/// matching sparse and dense query vectors. `mode: "query"` takes a fast lane
/// and never queues behind bulk document ingestion.
#[utoipa::path(
    post,
    path = "/v1/encode",
    tag = "encode",
    request_body = EncodeRequest,
    responses(
        (status = 200, description = "Encoded dense and/or sparse vectors", body = EncodeResponseSchema),
        (status = 400, description = "Empty texts or dense requested without an embedder", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 503, description = "Configured embedding backend is unavailable", body = ErrorBody),
        (status = 500, description = "Unexpected internal encoding failure", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn encode_texts(
    State(state): State<Arc<StatelessState>>,
    _auth: StatelessAuthed,
    body: axum::body::Bytes,
) -> Response {
    let payload: EncodeRequest = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(err) => {
            return ApiError::BadRequest(format!("Invalid JSON body: {err}")).into_response();
        }
    };
    if payload.texts.is_empty() {
        return ApiError::BadRequest("Body must include a non-empty 'texts' array".to_string())
            .into_response();
    }
    let mode = payload.mode.unwrap_or_default();
    let sparse = payload.sparse.unwrap_or(true);
    match state
        .processor
        .encode(&payload.texts, mode, payload.dense, sparse)
        .await
    {
        Ok(output) => (StatusCode::OK, Json(output)).into_response(),
        Err(err) => process_error_response(err),
    }
}

/// Rerank candidate passages against a search query.
///
/// Proxies through the configured local (TEI / vLLM / llama.cpp) or cloud
/// (Cohere / Voyage / OpenRouter) reranker on the search fast lane without
/// queuing behind bulk document ingestion.
#[utoipa::path(
    post,
    path = "/v1/rerank",
    tag = "rerank",
    request_body = RerankRequest,
    responses(
        (status = 200, description = "Reranked candidate indices and relevance scores", body = RerankResponseSchema),
        (status = 400, description = "Invalid input or no reranker configured", body = ErrorBody),
        (status = 401, description = "Missing or invalid API key", body = ErrorBody),
        (status = 503, description = "Configured reranker backend is unavailable", body = ErrorBody),
        (status = 500, description = "Unexpected internal reranking failure", body = ErrorBody),
    ),
    security(("bearer_auth" = []), ("api_key_header" = [])),
)]
pub async fn rerank_documents(
    State(state): State<Arc<StatelessState>>,
    _auth: StatelessAuthed,
    body: axum::body::Bytes,
) -> Response {
    let payload: RerankRequest = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(err) => {
            return ApiError::BadRequest(format!("Invalid JSON body: {err}")).into_response();
        }
    };
    match state
        .processor
        .rerank(
            &payload.query,
            &payload.documents,
            payload.top_k,
            payload.return_documents,
        )
        .await
    {
        Ok(output) => (StatusCode::OK, Json(output)).into_response(),
        Err(err) => process_error_response(err),
    }
}

fn process_error_response(err: ProcessError) -> Response {
    match err {
        ProcessError::BadRequest(msg) => ApiError::BadRequest(msg).into_response(),
        ProcessError::ExtractionFailed(msg) | ProcessError::NeedsOcr(msg) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorBody { error: msg }),
        )
            .into_response(),
        ProcessError::Unavailable(msg) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody { error: msg }),
        )
            .into_response(),
        ProcessError::Internal(msg) => ApiError::Internal(msg).into_response(),
    }
}

// ─── Multipart Parsing ───────────────────────────────────────────────────────

struct StatelessUpload {
    file_name: String,
    content_type: Option<String>,
    bytes: Vec<u8>,
    fields: Vec<(String, String)>,
}

impl StatelessUpload {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

async fn read_stateless_upload(
    request: Request,
    state: &Arc<StatelessState>,
) -> Result<StatelessUpload, ApiError> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|_| ApiError::BadRequest("No file provided (field: file)".to_string()))?;
    let mut upload = StatelessUpload {
        file_name: String::new(),
        content_type: None,
        bytes: Vec::new(),
        fields: Vec::new(),
    };
    let mut saw_file = false;
    loop {
        let field = multipart.next_field().await.map_err(|err| {
            if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::PayloadTooLarge(payload_too_large_message(None, None))
            } else {
                ApiError::BadRequest(format!("Malformed multipart body: {err}"))
            }
        })?;
        let Some(field) = field else { break };
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" {
            saw_file = true;
            upload.file_name = field.file_name().unwrap_or_default().to_string();
            upload.content_type = field.content_type().map(str::to_string);
            upload.bytes = field
                .bytes()
                .await
                .map_err(|err| {
                    if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
                        ApiError::PayloadTooLarge(payload_too_large_message(None, None))
                    } else {
                        ApiError::BadRequest(format!("Could not read the uploaded file: {err}"))
                    }
                })?
                .to_vec();
        } else {
            let value = field.text().await.map_err(|err| {
                ApiError::BadRequest(format!("Could not read form field {name}: {err}"))
            })?;
            upload.fields.push((name, value));
        }
    }
    if !saw_file || upload.file_name.is_empty() {
        return Err(ApiError::BadRequest(
            "No file provided (field: file)".to_string(),
        ));
    }
    Ok(upload)
}

fn build_process_request(upload: StatelessUpload) -> Result<ProcessRequest, ApiError> {
    let mime_type = upload
        .content_type
        .as_deref()
        .filter(|v| !v.is_empty())
        .unwrap_or("application/octet-stream")
        .to_string();
    let title = upload
        .field("title")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let category = upload
        .field("category")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| {
            upload
                .field("categories")
                .and_then(|v| crate::routes::parse_list(v).into_iter().next())
        });
    let subcategory = upload
        .field("subcategory")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let document_type = upload
        .field("documentType")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);

    let figure_mode = parse_strict_figure_mode(upload.field("figures"))?;
    let embed = parse_optional_bool(upload.field("embed"), "embed")?;
    let sparse = parse_optional_bool(upload.field("sparse"), "sparse")?.unwrap_or(true);
    let tables = parse_optional_bool(upload.field("tables"), "tables")?.unwrap_or(true);
    let entities = parse_optional_bool(upload.field("entities"), "entities")?.unwrap_or(false);
    let include_figure_bytes =
        parse_optional_bool(upload.field("includeFigureBytes"), "includeFigureBytes")?
            .unwrap_or(false);
    let chunk_size = parse_optional_u32(upload.field("chunkSize"), "chunkSize", true)?;
    let chunk_overlap = parse_optional_u32(upload.field("chunkOverlap"), "chunkOverlap", false)?;

    Ok(ProcessRequest {
        filename: upload.file_name,
        mime_type,
        bytes: upload.bytes,
        title,
        category,
        subcategory,
        figure_mode,
        document_type,
        embed,
        sparse,
        tables,
        entities,
        include_figure_bytes,
        chunk_size,
        chunk_overlap,
    })
}

fn parse_strict_figure_mode(value: Option<&str>) -> Result<FigureMode, ApiError> {
    let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(FigureMode::Auto);
    };
    let lower = raw.to_ascii_lowercase();
    if !matches!(lower.as_str(), "auto" | "all" | "none" | "off") {
        return Err(ApiError::BadRequest(format!(
            "Invalid 'figures' value {raw:?}: expected 'auto', 'all', 'none', or 'off'"
        )));
    }
    Ok(parse_figure_mode(Some(&lower), None))
}

fn parse_optional_bool(value: Option<&str>, field: &str) -> Result<Option<bool>, ApiError> {
    let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    match raw.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(Some(true)),
        "false" | "0" | "no" | "off" => Ok(Some(false)),
        _ => Err(ApiError::BadRequest(format!(
            "Invalid '{field}' value {raw:?}: expected a boolean ('true' or 'false')"
        ))),
    }
}

fn parse_optional_u32(
    value: Option<&str>,
    field: &str,
    must_be_positive: bool,
) -> Result<Option<u32>, ApiError> {
    let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let parsed: u32 = raw.parse().map_err(|_| {
        ApiError::BadRequest(format!(
            "Invalid '{field}' value {raw:?}: expected a non-negative integer"
        ))
    })?;
    if must_be_positive && parsed == 0 {
        return Err(ApiError::BadRequest(format!(
            "Invalid '{field}' value 0: expected a positive integer"
        )));
    }
    Ok(Some(parsed))
}

// ─── OpenAPI ─────────────────────────────────────────────────────────────────

/// OpenAPI 3.1 contract for stateless processor-only mode.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "rantai-kb Stateless Processor API",
        version = "0.0.0",
        description = "Stateless document-processing microservice mode (`kb-server serve --stateless` / `KB_MODE=stateless`).\n\n\
                       Runs extraction, OCR, smart chunking, table structuring, figure anchoring, Qdrant-ready BM25 sparse encoding, \
                       optional dense embeddings, optional reranking, and optional entity extraction with zero Postgres, Qdrant, or S3 connections.\n\n\
                       Authenticate with `Authorization: Bearer <key>` or `X-API-Key: <key>` matching `KB_STATELESS_API_KEY` \
                       (unless explicitly running with `KB_STATELESS_ALLOW_UNAUTHENTICATED=true`)."
    ),
    paths(
        stateless_health,
        stateless_health_v1,
        stateless_formats,
        process_document,
        encode_texts,
        rerank_documents,
    ),
    tags(
        (name = "process", description = "Synchronously extract, chunk, parse tables, anchor figures, and encode documents."),
        (name = "encode", description = "Encode query or passage texts into matching dense and Qdrant-compatible BM25 sparse vectors."),
        (name = "rerank", description = "Rerank candidate passages against a query via configured local or cloud rerankers."),
        (name = "health", description = "Liveness, capabilities, limits, backend status, and accepted upload formats. Public."),
    ),
    security(
        ("bearer_auth" = []),
        ("api_key_header" = []),
    ),
    modifiers(&SecurityAddon, &VersionAddon),
)]
pub struct StatelessApiDoc;

/// Pretty-printed OpenAPI 3.1 JSON for stateless mode.
///
/// # Errors
///
/// Fails only if `serde_json` cannot serialise the derived spec.
pub fn stateless_spec_json() -> Result<String, serde_json::Error> {
    static DOCUMENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(document) = DOCUMENT.get() {
        return Ok(document.clone());
    }
    let document = serde_json::to_string_pretty(&StatelessApiDoc::openapi())?;
    let _ = DOCUMENT.set(document.clone());
    Ok(document)
}

fn stateless_openapi_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/openapi.json",
            axum::routing::get(|| async {
                match stateless_spec_json() {
                    Ok(body) => (
                        [(
                            axum::http::header::CONTENT_TYPE,
                            "application/json; charset=utf-8",
                        )],
                        body,
                    )
                        .into_response(),
                    Err(err) => ApiError::Internal(format!(
                        "could not serialise the stateless OpenAPI document: {err}"
                    ))
                    .into_response(),
                }
            }),
        )
        .merge(
            utoipa_swagger_ui::SwaggerUi::new("/docs")
                .config(utoipa_swagger_ui::Config::new(["/openapi.json"])),
        )
}
