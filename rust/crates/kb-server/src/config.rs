//! Typed configuration reading environment variables at startup.

use std::time::Duration;

use anyhow::{Result, bail};
use kb_engine::{EngineSettings, IngestSettings};
use kb_extract::ExtractConfig;
use kb_inference::{ExpansionSettings, ExtractorSettings};
use kb_store_blob::S3Settings;
use kb_store_pg::PgSettings;
use kb_store_surreal::SurrealSettings;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ChunkStoreChoice {
    #[default]
    Surreal,
    Qdrant,
}

/// Which [`kb_ports::EntityExtractor`] ingest uses (`KB_ENTITY_EXTRACTOR`).
///
/// The default is `llm`, which is what production ran before the graph arm
/// existed and is therefore the compatible choice. `heuristic` is the free local
/// one: no key, no network, no spend (AGENTS rule 7), which is what makes the
/// graph arm measurable at all — an arm you can only measure by paying for it
/// gets measured once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EntityExtractorChoice {
    #[default]
    Llm,
    Heuristic,
}

/// Which [`kb_ports::GraphStore`] the entity graph is written to
/// (`KB_GRAPH_STORE`).
///
/// Unset means none, and that is the default: with no graph store there is no
/// graph, and the graph arm has nothing to walk. The Postgres adapter shares the
/// pool the rest of the process already holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GraphStoreChoice {
    #[default]
    None,
    Pg,
}

/// Graph retrieval arm tunables (`KB_GRAPH_ARM`, `KB_GRAPH_*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphArmSettings {
    /// Master switch. Off by default: §11 says graph RAG loses on fact lookup,
    /// and an arm that is on before it has been measured is an arm that has not.
    pub enabled: bool,
    /// Entities seeded into personalised PageRank, from `match_entities`.
    pub max_seeds: usize,
    /// PPR `eps`. The push solver's termination tolerance.
    pub tolerance: f32,
    /// PPR restart probability.
    pub damping: f32,
    /// Cached scoped graphs, and the byte budget across them.
    pub cache_entries: usize,
    pub cache_bytes: usize,
    /// Graph-ranked chunks fetched before fusion.
    pub candidates: usize,
}

impl Default for GraphArmSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_seeds: 8,
            tolerance: 1e-3,
            damping: 0.85,
            cache_entries: 16,
            cache_bytes: 256 * 1024 * 1024,
            candidates: 20,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    pub chunk_store_choice: ChunkStoreChoice,
    pub pg: PgSettings,
    pub surreal: SurrealSettings,
    pub qdrant: kb_store_qdrant::QdrantSettings,
    pub qdrant_collection_override: Option<String>,
    pub s3: S3Settings,
    pub embedder: EmbedderConfig,
    pub reranker: RerankerConfig,
    pub expansion: ExpansionSettings,
    pub engine: EngineSettings,
    pub extract: ExtractConfig,
    /// LLM entity/relation extraction settings (`ENTITY_EXTRACTION_*`).
    pub entity: ExtractorSettings,
    /// Which extractor ingest uses (`KB_ENTITY_EXTRACTOR`).
    pub entity_extractor: EntityExtractorChoice,
    /// Which graph store ingest writes the entity graph to (`KB_GRAPH_STORE`).
    pub graph_store: GraphStoreChoice,
    /// Whether the graph retrieval arm runs (`KB_GRAPH_ARM`).
    pub graph_arm: GraphArmSettings,
    /// In-process ingest worker: whether it runs, and how it is tuned.
    pub worker: WorkerConfig,
    /// Reported by `GET /health` (`KB_VERSION`).
    pub version: String,
    pub request_timeout: Duration,
    /// Content-addressed disk cache for embeddings and extraction
    /// (`KB_CACHE_DIR`). `None` turns caching off.
    pub cache: Option<CacheConfig>,
    /// The page-image arm (`KB_PAGE_EMBEDDING_MODEL`). `None` turns it off.
    pub pages: Option<PageArmSettings>,
    /// The deployment tier preset that shaped this config (`KB_TIER`), with
    /// the variables it filled. `None` when no tier is set.
    pub tier: Option<crate::tier::TierPlan>,
}

/// Page-image arm wiring: which multimodal embedder, how pages are rendered,
/// and how the arm is fused (`docs/eval-ab/vidore3-hr-en-image`).
#[derive(Clone, Debug)]
pub struct PageArmSettings {
    pub embedder: kb_inference::PageEmbedderSettings,
    pub dpi: u32,
    pub max_pages: u32,
    /// Characters of each page's text embedded with its image
    /// (`KB_PAGE_TEXT_CHARS`, default 6000; 0 = image only).
    pub text_chars: usize,
    pub arm: kb_engine::PageArmConfig,
}

/// Where the disk caches live and how their keys are scoped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheConfig {
    pub dir: std::path::PathBuf,
    /// Extra text folded into the embedding fingerprint
    /// (`KB_EMBEDDING_CACHE_TAG`), for when the same model name is served at a
    /// different quantisation and its vectors must not be mixed.
    pub embedding_tag: String,
}

/// Ingest worker wiring and tunables, ported from `src/lib/ingest/worker.ts`.
///
/// The `Math.max` floors there are part of the contract, not cosmetics: they
/// stop a mistyped `0` from turning the worker into a busy loop or from
/// reclaiming jobs that are merely slow.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    /// `KB_WORKER_ENABLED=false` serves the API without running the worker.
    pub enabled: bool,
    /// Jobs in flight at once (`KB_INGEST_CONCURRENCY`, floor 1).
    pub concurrency: usize,
    /// Claim poll interval (`KB_INGEST_POLL_MS`, floor 500).
    pub poll_ms: u64,
    /// "processing" staleness cutoff (`KB_INGEST_STALE_MS`, floor 60_000).
    pub stale_ms: u64,
    /// Attempts before a stranded job fails (`KB_INGEST_MAX_ATTEMPTS`, floor 1).
    pub max_attempts: i32,
    /// Stale-reclaim sweep interval (`KB_INGEST_RECLAIM_MS`, floor 15_000).
    pub reclaim_ms: u64,
    /// Age before a failed job's stored file is reaped (`KB_INGEST_REAP_DAYS`).
    pub reap_days: u32,
    /// Reaper sweep interval (`KB_INGEST_REAP_MS`).
    pub reap_ms: u64,
    /// Strip running headers, footers and page numbers before chunking
    /// (`KB_STRIP_PAGE_FURNITURE`).
    pub strip_page_furniture: bool,
    /// Run entity and relation extraction at all
    /// (`KB_ENTITY_EXTRACTION_ENABLED`, default on).
    pub entity_extraction_enabled: bool,
}

impl WorkerConfig {
    /// Engine settings for the worker these values describe.
    #[must_use]
    pub fn ingest_settings(&self) -> IngestSettings {
        IngestSettings {
            claims_per_tick: self.concurrency,
            poll_interval: Duration::from_millis(self.poll_ms),
            stale_after: Duration::from_millis(self.stale_ms),
            max_attempts: self.max_attempts,
            reclaim_interval: Duration::from_millis(self.reclaim_ms),
            strip_page_furniture: self.strip_page_furniture,
            entity_extraction_enabled: self.entity_extraction_enabled,
            ..IngestSettings::default()
        }
    }
}

fn read_worker<F>(lookup: &F) -> WorkerConfig
where
    F: Fn(&str) -> Option<String>,
{
    WorkerConfig {
        enabled: lookup("KB_WORKER_ENABLED").as_deref() != Some("false"),
        // `Math.max(floor, parseInt(value, 10))` in the TypeScript worker: a
        // missing or unparsable value takes the documented default, and a
        // value below the floor is raised to it.
        concurrency: int_var(lookup, "KB_INGEST_CONCURRENCY", 1)
            .unwrap_or(1)
            .max(1) as usize,
        poll_ms: int_var(lookup, "KB_INGEST_POLL_MS", 3000)
            .unwrap_or(3000)
            .max(500),
        stale_ms: int_var(lookup, "KB_INGEST_STALE_MS", 300_000)
            .unwrap_or(300_000)
            .max(60_000),
        max_attempts: int_var(lookup, "KB_INGEST_MAX_ATTEMPTS", 3)
            .unwrap_or(3)
            .max(1),
        reclaim_ms: int_var(lookup, "KB_INGEST_RECLAIM_MS", 60_000)
            .unwrap_or(60_000)
            .max(15_000),
        reap_days: int_var(lookup, "KB_INGEST_REAP_DAYS", 7)
            .unwrap_or(7)
            .max(0) as u32,
        reap_ms: int_var(lookup, "KB_INGEST_REAP_MS", 3_600_000)
            .unwrap_or(3_600_000)
            .max(1000),
        strip_page_furniture: bool_flag(lookup, "KB_STRIP_PAGE_FURNITURE", false),
        entity_extraction_enabled: bool_flag(lookup, "KB_ENTITY_EXTRACTION_ENABLED", true),
    }
}

#[derive(Clone, Debug)]
pub struct EmbedderConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub dimension: u32,
    pub batch_size: u32,
    pub embed_concurrency: u32,
    pub query_prefix: String,
    pub passage_prefix: String,
    /// `KB_EMBEDDING_INPUT_TYPE=true`: send Voyage's `input_type`
    /// (`query` / `document`) with every embedding request.
    pub send_input_type: bool,
    pub request_timeout: Duration,
}

impl EmbedderConfig {
    /// What a cached vector depends on besides its text and prefix: the model
    /// and its dimension. Not the URL: the same model served from another host
    /// yields the same vectors. A different quantisation behind the same model
    /// name is what `KB_EMBEDDING_CACHE_TAG` is for. The query and passage
    /// prefixes are keyed per side by `kb_cache::CachedEmbedder::with_prefixes`.
    #[must_use]
    pub fn cache_fingerprint(&self, tag: &str) -> String {
        // `input_type` changes the vector, so it is part of the key; the
        // suffix is omitted when off so existing cache entries stay valid.
        let input_type = if self.send_input_type {
            "|input_type=1"
        } else {
            ""
        };
        format!(
            "model={}|dim={}{input_type}|tag={tag}",
            self.model, self.dimension
        )
    }
}

#[derive(Clone, Debug)]
pub enum RerankerConfig {
    Disabled,
    /// LLM-as-reranker over OpenRouter. Also the TS default for unset or
    /// unknown `KB_RERANK_PROVIDER`.
    Llm {
        model: String,
        api_key: String,
    },
    Tei {
        base_url: String,
        model: String,
    },
    Vllm {
        base_url: String,
        model: String,
    },
    Cohere {
        model: String,
        api_key: String,
        endpoint: Option<String>,
    },
}

impl Config {
    /// A required variable, or an error that names the one that is missing.
    pub fn required_var<F>(lookup: &F, name: &str) -> Result<String>
    where
        F: Fn(&str) -> Option<String>,
    {
        match lookup(name) {
            Some(val) if !val.trim().is_empty() => Ok(val),
            _ => bail!("missing required environment variable: {name}"),
        }
    }

    /// Postgres settings from the environment.
    ///
    /// Read separately from [`Config::from_lookup`] because the CLI needs a
    /// database without an inference provider: `create-api-key` and `migrate`
    /// never embed anything, and forcing `OPENROUTER_API_KEY` on an operator's
    /// migration shell would be a barrier with no upside.
    ///
    /// # Errors
    ///
    /// Fails when `DATABASE_URL` is unset or blank.
    pub fn pg_from_lookup<F>(lookup: F) -> Result<PgSettings>
    where
        F: Fn(&str) -> Option<String>,
    {
        Ok(PgSettings {
            database_url: Self::required_var(&lookup, "DATABASE_URL")?,
            max_connections: 10,
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(5),
        })
    }

    /// SurrealDB settings from the environment, for both `serve` and
    /// `apply-schema`.
    ///
    /// # Errors
    ///
    /// Fails when `SURREAL_DB_URL` is unset or blank.
    pub fn surreal_from_lookup<F>(lookup: F) -> Result<SurrealSettings>
    where
        F: Fn(&str) -> Option<String>,
    {
        let url = Self::required_var(&lookup, "SURREAL_DB_URL")?;
        let username = lookup("SURREAL_DB_USER").filter(|s| !s.trim().is_empty());
        let password = lookup("SURREAL_DB_PASS").filter(|s| !s.trim().is_empty());
        let namespace = lookup("SURREAL_DB_NAMESPACE").unwrap_or_else(|| "rantai".into());
        let database = lookup("SURREAL_DB_DATABASE").unwrap_or_else(|| "knowledge".into());
        Ok(SurrealSettings {
            url,
            namespace,
            database,
            username,
            password,
            request_timeout: Duration::from_secs(10),
            // The vector index is defined at this width, and the client cannot
            // read it back, so the RetrievalIndex capabilities need it here.
            embedding_dims: Self::embedding_dim(&lookup),
        })
    }

    /// Embedding width (`KB_EMBEDDING_DIM`, 4096 by default).
    ///
    /// Read on its own by `apply-schema`: the vector index has to be defined
    /// with the width the service will store, and that has to be knowable
    /// before anything connects to an inference provider.
    pub fn embedding_dim<F>(lookup: &F) -> u32
    where
        F: Fn(&str) -> Option<String>,
    {
        // `Number(process.env.KB_EMBEDDING_DIM || 4096)` accepts a padded
        // value, so trim before parsing: a value like "8\n" from an env file
        // must not silently fall back to 4096 and mismatch the vector index.
        lookup("KB_EMBEDDING_DIM")
            .map(|v| v.trim().to_string())
            .and_then(|v| v.parse().ok())
            .unwrap_or(4096)
    }

    /// Read configuration from process environment variables.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Read configuration from a lookup function.
    pub fn from_lookup<F>(lookup: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        // `KB_TIER` (docs/kb-tiers.md) is a preset: it fills role defaults for
        // variables the operator did not set. Resolve what it will fill first,
        // then read everything through the overlay, so a preset-filled value
        // behaves exactly like one written in the environment.
        let tier = crate::tier::Tier::from_lookup(&lookup)?;
        let tier_plan = tier.map(|tier| crate::tier::TierPlan::resolve(tier, &lookup));
        let lookup = crate::tier::with_preset(tier, &lookup);

        let database_url = Self::required_var(&lookup, "DATABASE_URL")?;
        let surreal = Self::surreal_from_lookup(&lookup)?;

        let embedding_api_key = lookup("KB_EMBEDDING_API_KEY")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                // The OpenRouter key may pay for embeddings, unless the tier
                // points the embedder at the operator's own hardware: never
                // send a hosted key to a loopback or LAN endpoint. An unset
                // tier keeps today's fallback.
                tier.is_none_or(crate::tier::Tier::needs_openrouter_key)
                    .then(|| lookup("OPENROUTER_API_KEY"))
                    .flatten()
                    .filter(|s| !s.trim().is_empty())
            });

        if embedding_api_key.is_none() {
            match tier {
                Some(crate::tier::Tier::Cloud) => bail!(
                    "KB_TIER=cloud needs OPENROUTER_API_KEY (or KB_EMBEDDING_API_KEY): \
                     the tier's embedder and reranker run at OpenRouter"
                ),
                // A local tier embeds on the operator's own hardware; no key.
                Some(_) => {}
                None => bail!(
                    "missing required environment variable: OPENROUTER_API_KEY (or KB_EMBEDDING_API_KEY)"
                ),
            }
        }

        let embedding_base_url = lookup("KB_EMBEDDING_BASE_URL")
            .unwrap_or_else(|| "https://openrouter.ai/api/v1/embeddings".into());
        let embedding_model =
            lookup("KB_EMBEDDING_MODEL").unwrap_or_else(|| "qwen/qwen3-embedding-8b".into());
        let embedding_dim = Self::embedding_dim(&lookup);

        let port: u16 = lookup("PORT").and_then(|v| v.parse().ok()).unwrap_or(8080);
        let bind_addr = format!("0.0.0.0:{port}");

        // Retrieval knobs. Names and defaults come from
        // src/lib/rag/config.ts and src/lib/rag/config-surface.ts.
        let hybrid_bm25_enabled =
            lookup("KB_HYBRID_BM25_ENABLED").is_none_or(|v| v.trim() != "false");
        let default_max_chunks = int_var(&lookup, "KB_DEFAULT_MAX_CHUNKS", 8)?;
        let neighbour_window = int_var(&lookup, "KB_NEIGHBOR_WINDOW", 1)?;
        let rerank_initial_k = int_var(&lookup, "KB_RERANK_INITIAL_K", 20)?;
        let rerank_final_k = int_var(&lookup, "KB_RERANK_FINAL_K", 5)?;
        let figure_limit = int_var(&lookup, "KB_FIGURE_MAX_PER_ANSWER", 3)?;
        let query_expansion_enabled = bool_flag(&lookup, "KB_QUERY_EXPANSION_ENABLED", false);
        let query_expansion_model = lookup("KB_QUERY_EXPANSION_MODEL")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "openai/gpt-4.1-nano".into());
        let query_expansion_paraphrases = int_var(&lookup, "KB_QUERY_EXPANSION_PARAPHRASES", 3)?;

        let rerank_enabled = bool_flag(&lookup, "KB_RERANK_ENABLED", false);
        // Defaults keep the pre-L.4 behaviour (every candidate, full text, the
        // reranker client's own 20 s ceiling). A CPU-only box opts into a budget,
        // e.g. 5 candidates of 200 chars (`docs/eval-baseline.md` §8).
        let rerank_candidate_limit = int_var(&lookup, "KB_RERANK_CANDIDATE_LIMIT", 0u32)?.min(50);
        let rerank_char_budget = int_var(&lookup, "KB_RERANK_CHAR_BUDGET", 0)? as usize;
        let rerank_timeout_ms = int_var(&lookup, "KB_RERANK_TIMEOUT_MS", 20_000)? as u64;

        // `KB_MIN_SIMILARITY` is not a documented variable. The minimum is the
        // constant used by the TypeScript retriever (`minSimilarity = 0.30`).
        let min_similarity: f32 = 0.30;

        let engine = EngineSettings {
            candidate_pool_size: rerank_initial_k,
            rerank_candidate_limit,
            rerank_char_budget,
            rerank_timeout_ms,
            rerank_final_k,
            max_chunks: default_max_chunks,
            min_similarity,
            neighbour_window,
            figure_limit,
            expansion_enabled: query_expansion_enabled,
            expansion_paraphrases: query_expansion_paraphrases,
            hybrid_bm25_enabled,
            rerank_enabled,
        };

        let pg = PgSettings {
            database_url,
            max_connections: 10,
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(5),
        };

        // Object storage. Empty credentials stay empty: the adapter then sends
        // anonymous requests, which is what the Bun adapter does too.
        let s3 = S3Settings {
            endpoint: lookup("S3_ENDPOINT").unwrap_or_else(|| "http://localhost:9000".into()),
            region: lookup("S3_REGION").unwrap_or_else(|| "us-east-1".into()),
            bucket: lookup("S3_BUCKET").unwrap_or_else(|| "rantai-kb".into()),
            access_key_id: lookup("S3_ACCESS_KEY_ID").unwrap_or_default(),
            secret_access_key: lookup("S3_SECRET_ACCESS_KEY").unwrap_or_default(),
            force_path_style: lookup("S3_ENABLE_PATH_STYLE").as_deref() != Some("0"),
            request_timeout: Duration::from_secs(30),
        };

        let embedder = EmbedderConfig {
            base_url: embedding_base_url,
            model: embedding_model.clone(),
            api_key: embedding_api_key.clone(),
            dimension: embedding_dim,
            batch_size: int_var(&lookup, "KB_EMBEDDING_BATCH_SIZE", 32)?.max(1),
            // Both tunable for slow local backends: a consumer GPU embedding
            // a long PDF needs more than 15 s per batch once several batches
            // queue on it (docs/local-models.md).
            // Bun reads `KB_EMBED_CONCURRENCY`; honour it when the Rust name is unset,
            // so one .env drives both runtimes.
            embed_concurrency: if lookup("KB_EMBEDDING_CONCURRENCY").is_some() {
                int_var(&lookup, "KB_EMBEDDING_CONCURRENCY", 4u32)?
            } else {
                int_var(&lookup, "KB_EMBED_CONCURRENCY", 4u32)?
            }
            .clamp(1, 32),
            query_prefix: lookup("KB_EMBEDDING_QUERY_PREFIX").unwrap_or_default(),
            passage_prefix: lookup("KB_EMBEDDING_PASSAGE_PREFIX").unwrap_or_default(),
            send_input_type: bool_flag(&lookup, "KB_EMBEDDING_INPUT_TYPE", false),
            request_timeout: Duration::from_millis(
                int_var(&lookup, "KB_EMBEDDING_TIMEOUT_MS", 15_000u64)?.max(1_000),
            ),
        };

        let reranker = read_reranker(&lookup)?;

        let expansion = ExpansionSettings {
            enabled: query_expansion_enabled,
            api_key: embedding_api_key,
            endpoint: "https://openrouter.ai/api/v1/chat/completions".into(),
            model: query_expansion_model,
            paraphrase_count: query_expansion_paraphrases,
            timeout: Duration::from_secs(8),
        };

        let extract = ExtractConfig::from_lookup(&lookup);

        let chunk_store_choice = match lookup("KB_CHUNK_STORE").as_deref() {
            Some("qdrant") => ChunkStoreChoice::Qdrant,
            _ => ChunkStoreChoice::Surreal,
        };

        let qdrant_url = lookup("QDRANT_GRPC_URL")
            .or_else(|| lookup("QDRANT_URL"))
            .unwrap_or_else(|| "http://127.0.0.1:16334".to_string());
        let qdrant_api_key = lookup("QDRANT_API_KEY").filter(|s| !s.trim().is_empty());
        let qdrant_collection_override =
            lookup("QDRANT_COLLECTION").filter(|s| !s.trim().is_empty());
        let qdrant = kb_store_qdrant::QdrantSettings {
            url: qdrant_url,
            api_key: qdrant_api_key,
            model_slug: embedding_model.clone(),
            dense_dims: Some(embedding_dim),
            hnsw_m: 16,
            hnsw_ef_construct: 100,
            request_timeout: Duration::from_secs(30),
            upsert_batch: 256,
        };

        let entity = ExtractorSettings::from_lookup(&lookup);
        let entity_extractor = match lookup("KB_ENTITY_EXTRACTOR")
            .map(|v| v.trim().to_lowercase())
            .as_deref()
        {
            None | Some("") | Some("llm") | Some("openrouter") => EntityExtractorChoice::Llm,
            Some("heuristic") | Some("local") | Some("free") => EntityExtractorChoice::Heuristic,
            Some(other) => {
                bail!("KB_ENTITY_EXTRACTOR must be \"llm\" or \"heuristic\", got {other:?}")
            }
        };
        let graph_store = match lookup("KB_GRAPH_STORE")
            .map(|v| v.trim().to_lowercase())
            .as_deref()
        {
            None | Some("") | Some("none") => GraphStoreChoice::None,
            Some("pg") | Some("postgres") => GraphStoreChoice::Pg,
            Some(other) => bail!("KB_GRAPH_STORE must be \"none\" or \"pg\", got {other:?}"),
        };
        let graph_arm = GraphArmSettings {
            enabled: bool_flag(&lookup, "KB_GRAPH_ARM", false),
            max_seeds: int_var(&lookup, "KB_GRAPH_MAX_SEEDS", 8)?.max(1) as usize,
            tolerance: float_var(&lookup, "KB_GRAPH_PPR_EPS", 1e-3)?,
            damping: float_var(&lookup, "KB_GRAPH_PPR_DAMPING", 0.85)?,
            cache_entries: int_var(&lookup, "KB_GRAPH_CACHE_ENTRIES", 16)?.max(1) as usize,
            cache_bytes: int_var(&lookup, "KB_GRAPH_CACHE_BYTES", 256 * 1024 * 1024)?.max(0)
                as usize,
            candidates: int_var(&lookup, "KB_GRAPH_CANDIDATES", 20)?.max(1) as usize,
        };
        // An arm with no store behind it is not a degraded arm, it is a
        // misconfiguration that would silently return nothing. Say so at boot.
        if graph_arm.enabled && graph_store == GraphStoreChoice::None {
            bail!("KB_GRAPH_ARM=1 needs a graph store: set KB_GRAPH_STORE=pg");
        }
        let worker = read_worker(&lookup);
        let version = lookup("KB_VERSION")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| crate::state::DEFAULT_VERSION.to_string());

        let cache = lookup("KB_CACHE_DIR")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(|dir| CacheConfig {
                dir: dir.into(),
                embedding_tag: lookup("KB_EMBEDDING_CACHE_TAG").unwrap_or_default(),
            });

        let pages = match lookup("KB_PAGE_EMBEDDING_MODEL")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            None => None,
            Some(model) => {
                if chunk_store_choice != ChunkStoreChoice::Qdrant {
                    bail!("KB_PAGE_EMBEDDING_MODEL needs KB_CHUNK_STORE=qdrant");
                }
                let api_key = lookup("KB_PAGE_EMBEDDING_API_KEY")
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| lookup("OPENROUTER_API_KEY").filter(|s| !s.trim().is_empty()));
                let format = match lookup("KB_PAGE_EMBEDDING_FORMAT")
                    .map(|v| v.trim().to_lowercase())
                    .as_deref()
                {
                    None | Some("") | Some("openrouter") => {
                        kb_inference::PageEmbedFormat::OpenRouter
                    }
                    Some("vllm") => kb_inference::PageEmbedFormat::VllmChat,
                    Some(other) => bail!(
                        "KB_PAGE_EMBEDDING_FORMAT must be \"openrouter\" or \"vllm\", got {other:?}"
                    ),
                };
                Some(PageArmSettings {
                    embedder: kb_inference::PageEmbedderSettings {
                        base_url: lookup("KB_PAGE_EMBEDDING_BASE_URL")
                            .unwrap_or_else(|| "https://openrouter.ai/api/v1/embeddings".into()),
                        model,
                        api_key,
                        dimension: int_var(&lookup, "KB_PAGE_EMBEDDING_DIM", 1024u32)?.max(1),
                        format,
                        batch_size: int_var(&lookup, "KB_PAGE_EMBEDDING_BATCH_SIZE", 4u32)?
                            .clamp(1, 64),
                        concurrency: int_var(&lookup, "KB_PAGE_EMBEDDING_CONCURRENCY", 4u32)?
                            .clamp(1, 32),
                        request_timeout: Duration::from_millis(
                            int_var(&lookup, "KB_PAGE_EMBEDDING_TIMEOUT_MS", 60_000u64)?.max(1_000),
                        ),
                    },
                    dpi: int_var(&lookup, "KB_PAGE_IMAGE_DPI", 96u32)?.clamp(36, 300),
                    max_pages: int_var(&lookup, "KB_PAGE_IMAGE_MAX_PAGES", 500u32)?,
                    text_chars: int_var(&lookup, "KB_PAGE_TEXT_CHARS", 6000usize)?,
                    arm: kb_engine::PageArmConfig {
                        image_weight: float_var(&lookup, "KB_PAGE_ARM_WEIGHT", 0.5)?
                            .clamp(0.0, 1.0),
                        rrf_k: int_var(&lookup, "KB_PAGE_ARM_RRF_K", 5u32)?.max(1),
                        candidates: int_var(&lookup, "KB_PAGE_ARM_CANDIDATES", 100usize)?
                            .clamp(1, 500),
                        timeout: Duration::from_millis(
                            int_var(&lookup, "KB_PAGE_ARM_TIMEOUT_MS", 10_000u64)?.max(100),
                        ),
                        rerank: if bool_flag(&lookup, "KB_PAGE_RERANK", true) {
                            Some(kb_engine::PageRerankConfig {
                                depth: int_var(&lookup, "KB_PAGE_RERANK_DEPTH", 30usize)?
                                    .clamp(2, 100),
                                weight: float_var(&lookup, "KB_PAGE_RERANK_WEIGHT", 0.7)?
                                    .clamp(0.0, 1.0),
                                rrf_k: int_var(&lookup, "KB_PAGE_RERANK_RRF_K", 1u32)?.max(1),
                                chars: int_var(&lookup, "KB_PAGE_RERANK_CHARS", 3000usize)?
                                    .max(100),
                                timeout: Duration::from_millis(
                                    int_var(&lookup, "KB_PAGE_RERANK_TIMEOUT_MS", 20_000u64)?
                                        .max(100),
                                ),
                            })
                        } else {
                            None
                        },
                    },
                })
            }
        };

        Ok(Config {
            pages,
            cache,
            chunk_store_choice,
            qdrant,
            qdrant_collection_override,
            bind_addr,
            pg,
            surreal,
            s3,
            embedder,
            reranker,
            expansion,
            engine,
            extract,
            entity,
            entity_extractor,
            graph_store,
            graph_arm,
            worker,
            version,
            request_timeout: Duration::from_secs(30),
            tier: tier_plan,
        })
    }

    /// Print one INFO line describing what `KB_TIER` filled. A no-op without a
    /// tier, so an unset `KB_TIER` logs exactly what it logged before.
    pub fn log_tier(&self) {
        if let Some(plan) = self.tier.as_ref() {
            plan.log();
        }
    }
}

fn bool_flag<F>(lookup: &F, name: &str, default: bool) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    match lookup(name) {
        None => default,
        Some(v) if v.trim().is_empty() => default,
        Some(v) => matches!(
            v.trim().to_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
    }
}

fn float_var<F>(lookup: &F, name: &str, default: f32) -> Result<f32>
where
    F: Fn(&str) -> Option<String>,
{
    match lookup(name) {
        None => Ok(default),
        Some(v) if v.trim().is_empty() => Ok(default),
        Some(v) => v
            .trim()
            .parse::<f32>()
            .map_err(|_| anyhow::anyhow!("{name} must be a number, got {v:?}")),
    }
}

fn int_var<F, T>(lookup: &F, name: &str, default: T) -> Result<T>
where
    F: Fn(&str) -> Option<String>,
    T: std::str::FromStr + Copy,
{
    match lookup(name) {
        None => Ok(default),
        Some(v) if v.trim().is_empty() => Ok(default),
        Some(v) => v
            .trim()
            .parse::<T>()
            .map_err(|_| anyhow::anyhow!("{name} must be an integer, got {v:?}")),
    }
}

/// Reads `KB_RERANK_*` as `src/lib/rag/rerankers/index.ts` does.
/// `tei`, `vllm` and `cohere` select their adapters. `openrouter`, unset and
/// unknown providers all use the LLM reranker, as in TypeScript.
fn read_reranker<F>(lookup: &F) -> Result<RerankerConfig>
where
    F: Fn(&str) -> Option<String>,
{
    if !bool_flag(lookup, "KB_RERANK_ENABLED", false) {
        return Ok(RerankerConfig::Disabled);
    }

    let kind = lookup("KB_RERANK_PROVIDER")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let model = lookup("KB_RERANK_MODEL").filter(|s| !s.trim().is_empty());
    let base_url = lookup("KB_RERANK_BASE_URL").filter(|s| !s.trim().is_empty());
    let api_key = lookup("KB_RERANK_API_KEY")
        .filter(|s| !s.trim().is_empty())
        .or_else(|| lookup("COHERE_API_KEY").filter(|s| !s.trim().is_empty()));

    match kind.as_str() {
        "tei" => Ok(RerankerConfig::Tei {
            base_url: base_url.unwrap_or_else(|| "http://tei-rerank:80".into()),
            model: model.unwrap_or_else(|| "BAAI/bge-reranker-v2-m3".into()),
        }),
        "vllm" => Ok(RerankerConfig::Vllm {
            base_url: base_url.unwrap_or_else(|| "http://localhost:8200".into()),
            model: model.unwrap_or_else(|| "nvidia/llama-nemotron-rerank-1b-v2".into()),
        }),
        "cohere" => {
            let endpoint = base_url;
            // OpenRouter's rerank router takes the Cohere request shape; the
            // cloud tier points this adapter at it with the same key that pays
            // for embeddings. Only that host receives that key.
            let openrouter = endpoint
                .as_deref()
                .is_some_and(|url| url.to_ascii_lowercase().contains("openrouter.ai"));
            let api_key = api_key
                .or_else(|| {
                    if openrouter {
                        lookup("OPENROUTER_API_KEY").filter(|s| !s.trim().is_empty())
                    } else {
                        None
                    }
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "KB_RERANK_API_KEY or COHERE_API_KEY is required for cohere reranker \
                         (OPENROUTER_API_KEY is used when KB_RERANK_BASE_URL is openrouter.ai)"
                    )
                })?;
            Ok(RerankerConfig::Cohere {
                model: model.unwrap_or_else(|| "rerank-v3.5".into()),
                api_key,
                endpoint,
            })
        }
        _ => {
            let api_key = api_key
                .or_else(|| lookup("OPENROUTER_API_KEY").filter(|s| !s.trim().is_empty()))
                .unwrap_or_default();
            Ok(RerankerConfig::Llm {
                model: model.unwrap_or_else(|| "openai/gpt-4.1-nano".into()),
                api_key,
            })
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthChecks {
    pub postgres: bool,
    pub vectors: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_missing_required_database_url_fails() {
        let mut map = HashMap::new();
        map.insert("SURREAL_DB_URL", "http://localhost:8000");
        map.insert("OPENROUTER_API_KEY", "test-key");

        let err = Config::from_lookup(|k| map.get(k).copied().map(str::to_string)).unwrap_err();
        assert!(err.to_string().contains("DATABASE_URL"));
    }

    #[test]
    fn test_missing_required_surreal_url_fails() {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://user:pass@localhost/db");
        map.insert("OPENROUTER_API_KEY", "test-key");

        let err = Config::from_lookup(|k| map.get(k).copied().map(str::to_string)).unwrap_err();
        assert!(err.to_string().contains("SURREAL_DB_URL"));
    }

    #[test]
    fn test_missing_api_key_fails() {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://user:pass@localhost/db");
        map.insert("SURREAL_DB_URL", "http://localhost:8000");

        let err = Config::from_lookup(|k| map.get(k).copied().map(str::to_string)).unwrap_err();
        assert!(err.to_string().contains("OPENROUTER_API_KEY"));
    }

    #[test]
    fn test_config_reads_every_documented_variable_by_name() {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://user:pass@localhost/custom_pg");
        map.insert("SURREAL_DB_URL", "http://custom-surreal:8000");
        map.insert("SURREAL_DB_USER", "custom_surreal_user");
        map.insert("SURREAL_DB_PASS", "custom_surreal_pass");
        map.insert("SURREAL_DB_NAMESPACE", "custom_ns");
        map.insert("SURREAL_DB_DATABASE", "custom_db");
        map.insert(
            "KB_EMBEDDING_BASE_URL",
            "https://api.custom.ai/v1/embeddings",
        );
        map.insert("KB_EMBEDDING_API_KEY", "custom-embed-key");
        map.insert("KB_EMBEDDING_MODEL", "custom-model-v2");
        map.insert("KB_EMBEDDING_DIM", "1536");
        map.insert("PORT", "9099");
        map.insert("KB_DEFAULT_MAX_CHUNKS", "12");
        map.insert("KB_NEIGHBOR_WINDOW", "3");
        map.insert("KB_RERANK_INITIAL_K", "30");
        map.insert("KB_RERANK_FINAL_K", "7");
        map.insert("KB_FIGURE_MAX_PER_ANSWER", "6");
        map.insert("KB_HYBRID_BM25_ENABLED", "false");
        map.insert("KB_QUERY_EXPANSION_ENABLED", "true");
        map.insert("KB_QUERY_EXPANSION_MODEL", "custom/expander");
        map.insert("KB_QUERY_EXPANSION_PARAPHRASES", "2");
        map.insert("KB_VECTOR_KNN", "true");
        map.insert("KB_RERANK_ENABLED", "true");
        map.insert("KB_RERANK_PROVIDER", "tei");
        map.insert("KB_RERANK_BASE_URL", "http://tei.local:80");
        map.insert("KB_RERANK_MODEL", "BAAI/custom");
        map.insert("KB_RERANK_API_KEY", "unused-for-tei");
        map.insert("KB_EMBEDDING_QUERY_PREFIX", "query: ");
        map.insert("KB_EMBEDDING_PASSAGE_PREFIX", "passage: ");
        map.insert("S3_ENDPOINT", "http://s3.local:9000");
        map.insert("S3_REGION", "ap-southeast-1");
        map.insert("S3_BUCKET", "custom-bucket");
        map.insert("S3_ACCESS_KEY_ID", "custom-key");
        map.insert("S3_SECRET_ACCESS_KEY", "custom-secret");
        map.insert("S3_ENABLE_PATH_STYLE", "1");
        map.insert("KB_EXTRACT_PRIMARY", "mineru");
        map.insert("KB_EXTRACT_FALLBACK", "unpdf");
        map.insert("KB_EXTRACT_SMART_FALLBACK", "openai/gpt-4.1-nano");
        map.insert("KB_EXTRACT_MINERU_BASE_URL", "http://mineru.local:8100");
        map.insert("KB_MINERU_API_KEY", "mineru-key");
        map.insert("KB_MISTRAL_OCR_KEY", "mistral-key");
        map.insert("KB_LAYOUT_EXTRACTOR_ORDER", "mistral,sidecar");
        map.insert("OCR_FALLBACK_MODEL", "custom-ocr-model");
        map.insert("KB_WORKER_ENABLED", "false");
        map.insert("KB_INGEST_CONCURRENCY", "4");
        map.insert("KB_INGEST_POLL_MS", "1500");
        map.insert("KB_INGEST_STALE_MS", "900000");
        map.insert("KB_INGEST_MAX_ATTEMPTS", "5");
        map.insert("KB_INGEST_RECLAIM_MS", "30000");
        map.insert("KB_INGEST_REAP_DAYS", "3");
        map.insert("KB_INGEST_REAP_MS", "600000");
        map.insert("KB_VERSION", "9.9.9");
        map.insert("ENTITY_EXTRACTION_LLM_MODEL", "custom/entity-model");
        map.insert("ENTITY_EXTRACTION_LLM_BASE_URL", "http://llm.local/v1");
        map.insert("ENTITY_EXTRACTION_CONCURRENCY", "3");

        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert_eq!(
            cfg.pg.database_url,
            "postgres://user:pass@localhost/custom_pg"
        );
        assert_eq!(cfg.surreal.url, "http://custom-surreal:8000");
        assert_eq!(cfg.surreal.username.as_deref(), Some("custom_surreal_user"));
        assert_eq!(cfg.surreal.password.as_deref(), Some("custom_surreal_pass"));
        assert_eq!(cfg.surreal.namespace, "custom_ns");
        assert_eq!(cfg.surreal.database, "custom_db");
        assert_eq!(cfg.embedder.base_url, "https://api.custom.ai/v1/embeddings");
        assert_eq!(cfg.embedder.api_key.as_deref(), Some("custom-embed-key"));
        assert_eq!(cfg.embedder.model, "custom-model-v2");
        assert_eq!(cfg.embedder.dimension, 1536);
        assert_eq!(cfg.bind_addr, "0.0.0.0:9099");
        assert_eq!(cfg.engine.max_chunks, 12);
        assert_eq!(cfg.engine.neighbour_window, 3);
        assert_eq!(cfg.engine.candidate_pool_size, 30);
        assert_eq!(cfg.engine.rerank_final_k, 7);
        assert!((cfg.engine.min_similarity - 0.30).abs() < 1e-4);
        assert_eq!(cfg.engine.figure_limit, 6);
        assert!(!cfg.engine.hybrid_bm25_enabled);
        assert!(cfg.engine.expansion_enabled);
        assert_eq!(cfg.engine.expansion_paraphrases, 2);
        assert_eq!(cfg.expansion.model, "custom/expander");
        assert_eq!(cfg.embedder.query_prefix, "query: ");
        assert_eq!(cfg.embedder.passage_prefix, "passage: ");
        assert_eq!(cfg.s3.endpoint, "http://s3.local:9000");
        assert_eq!(cfg.s3.region, "ap-southeast-1");
        assert_eq!(cfg.s3.bucket, "custom-bucket");
        assert_eq!(cfg.s3.access_key_id, "custom-key");
        assert!(cfg.s3.force_path_style);
        assert_eq!(cfg.extract.primary, "mineru");
        assert_eq!(cfg.extract.fallback, "unpdf");
        assert_eq!(cfg.extract.smart_fallback, "openai/gpt-4.1-nano");
        assert_eq!(cfg.extract.mineru_base_url, "http://mineru.local:8100");
        assert_eq!(cfg.extract.mineru_api_key, "mineru-key");
        assert_eq!(cfg.extract.mistral_ocr_key, "mistral-key");
        assert_eq!(
            cfg.extract.layout_extractor_order,
            vec!["mistral", "sidecar"]
        );
        assert_eq!(cfg.extract.ocr_fallback_model, "custom-ocr-model");
        assert_eq!(cfg.version, "9.9.9");
        assert!(!cfg.worker.enabled);
        assert_eq!(cfg.worker.concurrency, 4);
        assert_eq!(cfg.worker.poll_ms, 1500);
        assert_eq!(cfg.worker.stale_ms, 900_000);
        assert_eq!(cfg.worker.max_attempts, 5);
        assert_eq!(cfg.worker.reclaim_ms, 30_000);
        assert_eq!(cfg.worker.reap_days, 3);
        assert_eq!(cfg.worker.reap_ms, 600_000);
        assert_eq!(cfg.entity.model, "custom/entity-model");
        assert_eq!(cfg.entity.base_url, "http://llm.local/v1");
        assert_eq!(cfg.entity.concurrency, 3);
        let worker_settings = cfg.worker.ingest_settings();
        assert_eq!(worker_settings.poll_interval, Duration::from_millis(1500));
        assert_eq!(worker_settings.stale_after, Duration::from_millis(900_000));
        assert_eq!(worker_settings.max_attempts, 5);
        assert_eq!(
            worker_settings.reclaim_interval,
            Duration::from_millis(30_000)
        );

        match &cfg.reranker {
            RerankerConfig::Tei { base_url, model } => {
                assert_eq!(base_url, "http://tei.local:80");
                assert_eq!(model, "BAAI/custom");
            }
            other => panic!("expected TEI reranker, got {other:?}"),
        }
    }

    #[test]
    fn test_defaults_match_typescript_config() {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://u:p@localhost/db");
        map.insert("SURREAL_DB_URL", "http://localhost:8000");
        map.insert("OPENROUTER_API_KEY", "test-key");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert_eq!(cfg.engine.max_chunks, 8);
        assert_eq!(cfg.engine.neighbour_window, 1);
        assert_eq!(cfg.engine.candidate_pool_size, 20);
        assert_eq!(cfg.engine.rerank_final_k, 5);
        assert_eq!(cfg.engine.figure_limit, 3);
        assert!(cfg.engine.hybrid_bm25_enabled);
        assert!(!cfg.engine.expansion_enabled);
        assert_eq!(cfg.engine.expansion_paraphrases, 3);
        assert!(matches!(cfg.reranker, RerankerConfig::Disabled));
        // Worker defaults match `src/lib/ingest/worker.ts`.
        assert!(cfg.worker.enabled);
        assert_eq!(cfg.worker.concurrency, 1);
        assert_eq!(cfg.worker.poll_ms, 3000);
        assert_eq!(cfg.worker.stale_ms, 300_000);
        assert_eq!(cfg.worker.max_attempts, 3);
        assert_eq!(cfg.worker.reclaim_ms, 60_000);
        assert_eq!(cfg.worker.reap_days, 7);
        assert_eq!(cfg.worker.reap_ms, 3_600_000);
        assert_eq!(cfg.version, "dev");
        // Object storage defaults match the Bun adapter.
        assert_eq!(cfg.s3.endpoint, "http://localhost:9000");
        assert_eq!(cfg.s3.region, "us-east-1");
        assert_eq!(cfg.s3.bucket, "rantai-kb");
        assert_eq!(cfg.s3.access_key_id, "");
        assert!(cfg.s3.force_path_style);
    }

    #[test]
    fn test_worker_knobs_keep_the_typescript_floors() {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://u:p@localhost/db");
        map.insert("SURREAL_DB_URL", "http://localhost:8000");
        map.insert("OPENROUTER_API_KEY", "test-key");
        // Values below the floor are raised, exactly like `Math.max`.
        map.insert("KB_INGEST_CONCURRENCY", "0");
        map.insert("KB_INGEST_POLL_MS", "10");
        map.insert("KB_INGEST_STALE_MS", "1000");
        map.insert("KB_INGEST_MAX_ATTEMPTS", "0");
        map.insert("KB_INGEST_RECLAIM_MS", "1");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert_eq!(cfg.worker.concurrency, 1);
        assert_eq!(cfg.worker.poll_ms, 500);
        assert_eq!(cfg.worker.stale_ms, 60_000);
        assert_eq!(cfg.worker.max_attempts, 1);
        assert_eq!(cfg.worker.reclaim_ms, 15_000);
        // Only the exact string "false" disables the worker.
        map.insert("KB_WORKER_ENABLED", "false");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert!(!cfg.worker.enabled);
        map.insert("KB_WORKER_ENABLED", "0");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert!(cfg.worker.enabled);
    }

    #[test]
    fn test_cache_dir_is_off_by_default_and_scopes_the_embedding_fingerprint() {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://u:p@localhost/db");
        map.insert("SURREAL_DB_URL", "http://localhost:8000");
        map.insert("OPENROUTER_API_KEY", "test-key");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert_eq!(cfg.cache, None);

        map.insert("KB_CACHE_DIR", "/var/cache/kb");
        map.insert("KB_EMBEDDING_CACHE_TAG", "q8");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        let cache = cfg.cache.expect("cache configured");
        assert_eq!(cache.dir, std::path::PathBuf::from("/var/cache/kb"));
        let a = cfg.embedder.cache_fingerprint(&cache.embedding_tag);
        let b = cfg.embedder.cache_fingerprint("");
        assert_ne!(a, b, "the tag must change the fingerprint");
        assert!(a.contains("dim=4096"));
        assert!(!cfg.embedder.send_input_type, "input_type is opt-in");

        // Voyage's input_type changes the vector, so it changes the key.
        map.insert("KB_EMBEDDING_INPUT_TYPE", "true");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert!(cfg.embedder.send_input_type);
        assert_ne!(cfg.embedder.cache_fingerprint(&cache.embedding_tag), a);
        map.remove("KB_EMBEDDING_INPUT_TYPE");

        // An extraction fingerprint never carries a secret.
        let mut extract = cfg.extract.clone();
        extract.mistral_ocr_key = "sk-secret-value".into();
        let fp = extract.cache_fingerprint();
        assert!(!fp.contains("sk-secret-value"));
        assert_ne!(fp, cfg.extract.cache_fingerprint(), "key presence matters");
    }

    #[test]
    fn test_embedder_timeout_and_concurrency_are_tunable() {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://u:p@localhost/db");
        map.insert("SURREAL_DB_URL", "http://localhost:8000");
        map.insert("OPENROUTER_API_KEY", "test-key");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert_eq!(cfg.embedder.request_timeout, Duration::from_secs(15));
        assert_eq!(cfg.embedder.embed_concurrency, 4);
        map.insert("KB_EMBED_CONCURRENCY", "6");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert_eq!(cfg.embedder.embed_concurrency, 6, "Bun's name is honoured");
        map.insert("KB_EMBEDDING_TIMEOUT_MS", "120000");
        map.insert("KB_EMBEDDING_CONCURRENCY", "1");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert_eq!(cfg.embedder.request_timeout, Duration::from_secs(120));
        assert_eq!(cfg.embedder.embed_concurrency, 1);
        // A timeout too short to ever succeed is raised to 1 s.
        map.insert("KB_EMBEDDING_TIMEOUT_MS", "5");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert_eq!(cfg.embedder.request_timeout, Duration::from_secs(1));
    }

    #[test]
    fn test_path_style_can_be_disabled() {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://u:p@localhost/db");
        map.insert("SURREAL_DB_URL", "http://localhost:8000");
        map.insert("OPENROUTER_API_KEY", "test-key");
        map.insert("S3_ENABLE_PATH_STYLE", "0");
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        assert!(!cfg.s3.force_path_style);
    }

    #[test]
    fn test_non_integer_setting_fails_with_name() {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://u:p@localhost/db");
        map.insert("SURREAL_DB_URL", "http://localhost:8000");
        map.insert("OPENROUTER_API_KEY", "test-key");
        map.insert("KB_DEFAULT_MAX_CHUNKS", "many");
        let err = Config::from_lookup(|k| map.get(k).copied().map(str::to_string)).unwrap_err();
        assert!(err.to_string().contains("KB_DEFAULT_MAX_CHUNKS"));
    }

    fn page_arm_map() -> HashMap<&'static str, &'static str> {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://u:p@localhost/db");
        map.insert("SURREAL_DB_URL", "http://localhost:8000");
        map.insert("OPENROUTER_API_KEY", "test-key");
        map.insert("KB_CHUNK_STORE", "qdrant");
        map.insert("KB_PAGE_EMBEDDING_MODEL", "Qwen/Qwen3-VL-Embedding-2B");
        map
    }

    fn page_format(map: &HashMap<&'static str, &'static str>) -> kb_inference::PageEmbedFormat {
        let cfg = Config::from_lookup(|k| map.get(k).copied().map(str::to_string))
            .expect("config should load");
        cfg.pages.expect("page arm configured").embedder.format
    }

    #[test]
    fn test_page_embedding_format_defaults_to_openrouter_and_reads_vllm() {
        let mut map = page_arm_map();
        assert_eq!(page_format(&map), kb_inference::PageEmbedFormat::OpenRouter);
        map.insert("KB_PAGE_EMBEDDING_FORMAT", "openrouter");
        assert_eq!(page_format(&map), kb_inference::PageEmbedFormat::OpenRouter);
        map.insert("KB_PAGE_EMBEDDING_FORMAT", "vllm");
        assert_eq!(
            page_format(&map),
            kb_inference::PageEmbedFormat::VllmChat,
            "the local vLLM shape is selected by name"
        );
    }

    #[test]
    fn test_unknown_page_embedding_format_names_the_variable() {
        let mut map = page_arm_map();
        map.insert("KB_PAGE_EMBEDDING_FORMAT", "chat");
        let err = Config::from_lookup(|k| map.get(k).copied().map(str::to_string)).unwrap_err();
        assert!(
            err.to_string().contains("KB_PAGE_EMBEDDING_FORMAT"),
            "{err}"
        );
    }

    // --- KB_TIER presets (`docs/kb-tiers.md`) -------------------------------

    fn tier_map() -> HashMap<&'static str, &'static str> {
        let mut map = HashMap::new();
        map.insert("DATABASE_URL", "postgres://u:p@localhost/db");
        map.insert("SURREAL_DB_URL", "http://localhost:8000");
        map
    }

    fn resolved(map: &HashMap<&'static str, &'static str>) -> Config {
        Config::from_lookup(|k| map.get(k).copied().map(str::to_string)).expect("config loads")
    }

    /// The Qwen3 instruction the repo already uses (`docs/local-models.md`).
    const QWEN3_INSTRUCTION: &str =
        "Instruct: Given a question, retrieve passages that answer it\nQuery: ";

    #[test]
    fn test_unset_tier_resolves_exactly_like_todays_config() {
        let mut map = tier_map();
        map.insert("OPENROUTER_API_KEY", "test-key");
        let cfg = resolved(&map);
        assert!(cfg.tier.is_none(), "no preset without KB_TIER");

        // Today's defaults, one per role the preset touches. A preset that
        // leaked in without `KB_TIER` would move at least one of these.
        assert_eq!(cfg.embedder.model, "qwen/qwen3-embedding-8b");
        assert_eq!(cfg.embedder.dimension, 4096);
        assert_eq!(
            cfg.embedder.base_url,
            "https://openrouter.ai/api/v1/embeddings"
        );
        assert!(!cfg.embedder.send_input_type);
        assert_eq!(cfg.embedder.query_prefix, "");
        assert_eq!(cfg.embedder.batch_size, 32);
        assert_eq!(cfg.embedder.embed_concurrency, 4);
        assert_eq!(cfg.embedder.request_timeout, Duration::from_secs(15));
        assert!(matches!(cfg.reranker, RerankerConfig::Disabled));
        assert_eq!(cfg.engine.rerank_candidate_limit, 0);
        assert_eq!(cfg.worker.concurrency, 1);
        assert!(cfg.worker.entity_extraction_enabled);
        assert!(cfg.pages.is_none());
        assert_eq!(cfg.chunk_store_choice, ChunkStoreChoice::Surreal);

        // The whole struct, against the same environment with every one of
        // those values written out explicitly: still identical.
        let mut explicit = map.clone();
        explicit.insert("KB_EMBEDDING_MODEL", "qwen/qwen3-embedding-8b");
        explicit.insert("KB_EMBEDDING_DIM", "4096");
        explicit.insert(
            "KB_EMBEDDING_BASE_URL",
            "https://openrouter.ai/api/v1/embeddings",
        );
        explicit.insert("KB_EMBEDDING_INPUT_TYPE", "false");
        explicit.insert("KB_EMBEDDING_QUERY_PREFIX", "");
        explicit.insert("KB_EMBEDDING_BATCH_SIZE", "32");
        explicit.insert("KB_EMBEDDING_CONCURRENCY", "4");
        explicit.insert("KB_EMBEDDING_TIMEOUT_MS", "15000");
        explicit.insert("KB_RERANK_ENABLED", "false");
        explicit.insert("KB_RERANK_CANDIDATE_LIMIT", "0");
        explicit.insert("KB_INGEST_CONCURRENCY", "1");
        explicit.insert("KB_ENTITY_EXTRACTION_ENABLED", "true");
        explicit.insert("KB_CHUNK_STORE", "surreal");
        assert_eq!(
            format!("{cfg:#?}"),
            format!("{:#?}", resolved(&explicit)),
            "unset tier must resolve exactly like today's explicit settings"
        );
    }

    #[test]
    fn test_each_tier_fills_its_role_defaults() {
        // (tier, embedding concurrency, chunk-rerank candidates, page arm on)
        let rows = [
            ("lite", 1u32, None, false),
            ("desk", 2, Some(10u32), false),
            ("studio", 4, Some(20), false),
            ("station", 8, Some(30), false),
            ("cloud", 4, Some(20), true),
        ];
        for (tier, concurrency, candidates, page_arm) in rows {
            let mut map = tier_map();
            if tier == "cloud" {
                map.insert("OPENROUTER_API_KEY", "test-key");
            }
            map.insert("KB_TIER", tier);
            let cfg = resolved(&map);
            let plan = cfg.tier.as_ref().expect("tier plan recorded");
            assert_eq!(plan.tier.as_str(), tier);
            assert!(!plan.filled.is_empty(), "{tier} filled nothing");

            assert_eq!(cfg.embedder.dimension, 1024, "{tier}");
            assert_eq!(cfg.embedder.embed_concurrency, concurrency, "{tier}");
            assert_eq!(
                cfg.engine.rerank_candidate_limit,
                candidates.unwrap_or(0),
                "{tier}"
            );
            assert_eq!(cfg.pages.is_some(), page_arm, "{tier}");

            if tier == "cloud" {
                assert_eq!(cfg.embedder.model, "voyageai/voyage-multimodal-3.5");
                assert!(cfg.embedder.send_input_type, "voyage wants input_type");
                assert_eq!(cfg.chunk_store_choice, ChunkStoreChoice::Qdrant);
                let pages = cfg.pages.as_ref().expect("cloud page arm");
                assert_eq!(pages.embedder.model, "voyageai/voyage-multimodal-3.5");
                assert_eq!(pages.embedder.dimension, 1024);
                assert!(pages.arm.rerank.is_some(), "cloud page rerank on");
                match &cfg.reranker {
                    RerankerConfig::Cohere {
                        model,
                        api_key,
                        endpoint,
                    } => {
                        assert_eq!(model, "voyageai/rerank-2.5");
                        assert_eq!(api_key, "test-key", "one OpenRouter key");
                        assert_eq!(
                            endpoint.as_deref(),
                            Some("https://openrouter.ai/api/v1/rerank")
                        );
                    }
                    other => panic!("cloud reranker should be cohere-shaped, got {other:?}"),
                }
            } else {
                assert_eq!(cfg.embedder.model, "Qwen/Qwen3-Embedding-0.6B");
                assert_eq!(cfg.embedder.base_url, "http://127.0.0.1:8081/v1/embeddings");
                assert_eq!(cfg.embedder.query_prefix, QWEN3_INSTRUCTION);
                assert_eq!(cfg.embedder.request_timeout, Duration::from_secs(120));
                assert_eq!(cfg.chunk_store_choice, ChunkStoreChoice::Surreal);
                assert!(
                    cfg.embedder.api_key.is_none(),
                    "{tier} must not need a paid key"
                );
                if tier == "lite" {
                    assert!(matches!(cfg.reranker, RerankerConfig::Disabled));
                    assert_eq!(cfg.embedder.batch_size, 16);
                    assert_eq!(cfg.worker.concurrency, 1);
                    assert!(!cfg.worker.entity_extraction_enabled);
                } else {
                    assert_eq!(cfg.embedder.batch_size, 32);
                    match &cfg.reranker {
                        RerankerConfig::Tei { base_url, model } => {
                            assert_eq!(base_url, "http://127.0.0.1:8082");
                            assert_eq!(model, "BAAI/bge-reranker-v2-m3");
                        }
                        other => panic!("{tier} reranker should be TEI bge, got {other:?}"),
                    }
                    assert!(cfg.worker.entity_extraction_enabled);
                }
            }

            // Every variable the plan reports was really unset by the operator.
            assert!(
                plan.filled.iter().all(|(name, _)| !map.contains_key(name)),
                "{tier} claims to have filled an explicit variable"
            );
        }
    }

    #[test]
    fn test_an_explicit_variable_overrides_the_tier_preset() {
        let mut map = tier_map();
        map.insert("KB_TIER", "lite");
        map.insert("KB_EMBEDDING_MODEL", "operator/model");
        map.insert("KB_EMBED_CONCURRENCY", "6");
        map.insert("KB_INGEST_CONCURRENCY", "3");
        // Present but empty: explicit, so the preset does not fill it and the
        // setting's own default applies.
        map.insert("KB_ENTITY_EXTRACTION_ENABLED", "");
        let cfg = resolved(&map);

        assert_eq!(cfg.embedder.model, "operator/model");
        assert_eq!(cfg.embedder.embed_concurrency, 6);
        assert_eq!(cfg.worker.concurrency, 3);
        assert!(
            cfg.worker.entity_extraction_enabled,
            "an empty explicit value suppresses the preset"
        );
        let plan = cfg.tier.as_ref().expect("tier plan");
        for overridden in [
            "KB_EMBEDDING_MODEL",
            "KB_EMBED_CONCURRENCY",
            "KB_INGEST_CONCURRENCY",
            "KB_ENTITY_EXTRACTION_ENABLED",
        ] {
            assert!(
                !plan.filled.iter().any(|(name, _)| *name == overridden),
                "{overridden} was set explicitly and must not be reported as filled"
            );
        }
        // Values the operator did not touch still come from the tier.
        assert_eq!(cfg.embedder.dimension, 1024);
        assert!(
            plan.filled
                .iter()
                .any(|(name, _)| *name == "KB_EMBEDDING_DIM")
        );
    }

    #[test]
    fn test_cloud_without_an_openrouter_key_names_the_tier_and_the_variable() {
        let mut map = tier_map();
        map.insert("KB_TIER", "cloud");
        let err = Config::from_lookup(|k| map.get(k).copied().map(str::to_string)).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("cloud"), "{message}");
        assert!(message.contains("OPENROUTER_API_KEY"), "{message}");
    }

    #[test]
    fn test_unknown_tier_is_an_error_listing_the_valid_values() {
        let mut map = tier_map();
        map.insert("OPENROUTER_API_KEY", "test-key");
        map.insert("KB_TIER", "workstation");
        let err = Config::from_lookup(|k| map.get(k).copied().map(str::to_string)).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("KB_TIER"), "{message}");
        assert!(message.contains("workstation"), "{message}");
        for valid in ["lite", "desk", "studio", "station", "cloud"] {
            assert!(message.contains(valid), "{message} omits {valid}");
        }
    }

    #[test]
    fn test_the_tier_log_line_keeps_secrets_out() {
        let mut map = tier_map();
        map.insert("KB_TIER", "cloud");
        map.insert("OPENROUTER_API_KEY", "sk-or-secret-value");
        let cfg = resolved(&map);
        let plan = cfg.tier.as_ref().expect("tier plan");
        let line = plan
            .filled
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(", ");
        assert!(!line.contains("sk-or-secret-value"), "{line}");
    }
}
