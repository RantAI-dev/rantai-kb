//! Typed configuration. Reads from env at boot; no `Arc<Mutex>` because
//! nothing mutates it.

use std::time::Duration;

use kb_engine::EngineSettings;
use kb_inference::ExpansionSettings;
use kb_store_pg::PgSettings;
use kb_store_surreal::SurrealSettings;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    pub pg: PgSettings,
    pub surreal: SurrealSettings,
    pub embedder: EmbedderConfig,
    pub reranker: RerankerConfig,
    pub expansion: ExpansionSettings,
    pub engine: EngineSettings,
    pub request_timeout: Duration,
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
    pub request_timeout: Duration,
}

#[derive(Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RerankerConfig {
    Disabled,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:8080".into(),
            pg: PgSettings {
                database_url: String::new(),
                max_connections: 10,
                connect_timeout: Duration::from_secs(5),
                acquire_timeout: Duration::from_secs(5),
            },
            surreal: SurrealSettings {
                url: String::new(),
                namespace: "kb".into(),
                database: "main".into(),
                username: None,
                password: None,
                request_timeout: Duration::from_secs(10),
            },
            embedder: EmbedderConfig {
                base_url: String::new(),
                model: String::new(),
                api_key: None,
                dimension: 4096,
                batch_size: 32,
                embed_concurrency: 4,
                query_prefix: String::new(),
                passage_prefix: String::new(),
                request_timeout: Duration::from_secs(15),
            },
            reranker: RerankerConfig::Disabled,
            expansion: ExpansionSettings::from_env_or_disabled(),
            engine: EngineSettings::default(),
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl Config {
    /// Read everything from process env. Required vars:
    ///   `KB_DATABASE_URL`
    ///   `KB_SURREAL_URL`
    ///   `KB_EMBEDDING_BASE_URL`, `KB_EMBEDDING_MODEL`
    pub fn from_env() -> anyhow::Result<Self> {
        let mut c = Self::default();
        c.bind_addr = std::env::var("KB_BIND_ADDR").unwrap_or(c.bind_addr);
        c.pg.database_url = std::env::var("KB_DATABASE_URL").unwrap_or(c.pg.database_url);
        c.surreal.url = std::env::var("KB_SURREAL_URL").unwrap_or(c.surreal.url);
        c.surreal.username = std::env::var("KB_SURREAL_USER").ok();
        c.surreal.password = std::env::var("KB_SURREAL_PASS").ok();

        c.embedder.base_url =
            std::env::var("KB_EMBEDDING_BASE_URL").unwrap_or(c.embedder.base_url);
        c.embedder.model = std::env::var("KB_EMBEDDING_MODEL").unwrap_or(c.embedder.model);
        c.embedder.api_key = std::env::var("KB_EMBEDDING_API_KEY")
            .ok()
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
        c.embedder.dimension = std::env::var("KB_EMBEDDING_DIM")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(c.embedder.dimension);

        c.reranker = read_reranker_from_env();

        c.expansion.enabled = std::env::var("KB_QUERY_EXPANSION_ENABLED")
            .ok()
            .map(|v| v == "true")
            .unwrap_or(false);
        c.expansion.api_key = std::env::var("OPENROUTER_API_KEY").ok();

        Ok(c)
    }
}

fn read_reranker_from_env() -> RerankerConfig {
    let kind = std::env::var("KB_RERANK_PROVIDER").unwrap_or_else(|_| "disabled".into());
    match kind.as_str() {
        "tei" => RerankerConfig::Tei {
            base_url: std::env::var("KB_TEI_RERANK_URL").unwrap_or_default(),
            model: std::env::var("KB_TEI_RERANK_MODEL").unwrap_or_default(),
        },
        "vllm" => RerankerConfig::Vllm {
            base_url: std::env::var("KB_VLLM_RERANK_URL").unwrap_or_default(),
            model: std::env::var("KB_VLLM_RERANK_MODEL").unwrap_or_default(),
        },
        "cohere" => RerankerConfig::Cohere {
            api_key: std::env::var("COHERE_API_KEY").unwrap_or_default(),
            model: std::env::var("KB_COHERE_RERANK_MODEL").unwrap_or_else(|_| "rerank-v3.5".into()),
            endpoint: std::env::var("KB_COHERE_RERANK_ENDPOINT").ok(),
        },
        _ => RerankerConfig::Disabled,
    }
}

/// Tiny serialisable stub used in integration tests to share test config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthChecks {
    pub postgres: bool,
    pub vectors: bool,
}