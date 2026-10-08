//! Typed configuration reading environment variables at startup.

use std::time::Duration;

use anyhow::{Result, bail};
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

impl Config {
    /// Read configuration from process environment variables.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Read configuration from a lookup function.
    pub fn from_lookup<F>(lookup: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let database_url = match lookup("DATABASE_URL") {
            Some(val) if !val.trim().is_empty() => val,
            _ => bail!("missing required environment variable: DATABASE_URL"),
        };

        let surreal_url = match lookup("SURREAL_DB_URL") {
            Some(val) if !val.trim().is_empty() => val,
            _ => bail!("missing required environment variable: SURREAL_DB_URL"),
        };

        let surreal_user = lookup("SURREAL_DB_USER").filter(|s| !s.trim().is_empty());
        let surreal_pass = lookup("SURREAL_DB_PASS").filter(|s| !s.trim().is_empty());
        let surreal_ns = lookup("SURREAL_DB_NAMESPACE").unwrap_or_else(|| "kb".into());
        let surreal_db = lookup("SURREAL_DB_DATABASE").unwrap_or_else(|| "main".into());

        let embedding_api_key = lookup("KB_EMBEDDING_API_KEY")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| lookup("OPENROUTER_API_KEY").filter(|s| !s.trim().is_empty()));

        if embedding_api_key.is_none() {
            bail!(
                "missing required environment variable: OPENROUTER_API_KEY (or KB_EMBEDDING_API_KEY)"
            );
        }

        let embedding_base_url = lookup("KB_EMBEDDING_BASE_URL")
            .unwrap_or_else(|| "https://openrouter.ai/api/v1/embeddings".into());
        let embedding_model =
            lookup("KB_EMBEDDING_MODEL").unwrap_or_else(|| "qwen/qwen3-embedding-8b".into());
        let embedding_dim: u32 = lookup("KB_EMBEDDING_DIM")
            .and_then(|v| v.parse().ok())
            .unwrap_or(4096);

        let port: u16 = lookup("PORT").and_then(|v| v.parse().ok()).unwrap_or(3001);
        let bind_addr = format!("0.0.0.0:{port}");

        // Retrieval knobs. Names and defaults come from
        // src/lib/rag/config.ts and src/lib/rag/config-surface.ts.
        let hybrid_bm25_enabled = bool_flag(&lookup, "KB_HYBRID_BM25_ENABLED", true);
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

        // `KB_MIN_SIMILARITY` is not a documented variable. The minimum is the
        // constant used by the TypeScript retriever (`minSimilarity = 0.30`).
        let min_similarity: f32 = 0.30;

        let engine = EngineSettings {
            candidate_pool_size: rerank_initial_k,
            rerank_final_k,
            max_chunks: default_max_chunks,
            min_similarity,
            neighbour_window,
            figure_limit,
            expansion_enabled: query_expansion_enabled,
            expansion_paraphrases: query_expansion_paraphrases,
            hybrid_bm25_enabled,
        };

        let pg = PgSettings {
            database_url,
            max_connections: 10,
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(5),
        };

        let surreal = SurrealSettings {
            url: surreal_url,
            namespace: surreal_ns,
            database: surreal_db,
            username: surreal_user,
            password: surreal_pass,
            request_timeout: Duration::from_secs(10),
        };

        let embedder = EmbedderConfig {
            base_url: embedding_base_url,
            model: embedding_model,
            api_key: embedding_api_key.clone(),
            dimension: embedding_dim,
            batch_size: 32,
            embed_concurrency: 4,
            query_prefix: lookup("KB_EMBEDDING_QUERY_PREFIX").unwrap_or_default(),
            passage_prefix: lookup("KB_EMBEDDING_PASSAGE_PREFIX").unwrap_or_default(),
            request_timeout: Duration::from_secs(15),
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

        Ok(Config {
            bind_addr,
            pg,
            surreal,
            embedder,
            reranker,
            expansion,
            engine,
            request_timeout: Duration::from_secs(30),
        })
    }
}

fn bool_flag<F>(lookup: &F, name: &str, default: bool) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    match lookup(name) {
        None => default,
        Some(v) if v.trim().is_empty() => default,
        Some(v) => v.trim() == "true",
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

/// Reads `KB_RERANK_*` as `src/lib/rag/rerankers/index.ts` does. The provider
/// names are `tei`, `vllm`, `cohere` and `openrouter` (the LLM reranker).
/// The Rust port has no LLM reranker, so `openrouter` and any unset or
/// unknown provider are reported as a configuration error.
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
        "cohere" => Ok(RerankerConfig::Cohere {
            model: model.unwrap_or_else(|| "rerank-v4.0-pro".into()),
            api_key: api_key.unwrap_or_default(),
            endpoint: base_url,
        }),
        other => bail!(
            "KB_RERANK_PROVIDER={other:?} is not supported by the Rust service (use tei, vllm or cohere)"
        ),
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
}
