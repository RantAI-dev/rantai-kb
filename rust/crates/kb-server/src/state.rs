//! `AppState`: the whole graph of adapters + the engine, behind `Arc`s.

use std::sync::Arc;

use kb_engine::{Engine, EngineInputs, EngineSettings, NoopExpander, QueryExpander};
use kb_inference::EmbedderSettings;
use kb_inference::ExpansionSettings;
use kb_ports::{BlobStore, DocumentStore, Embedder, PortError};

use crate::config::{EmbedderConfig, RerankerConfig};

pub struct AppState {
    pub documents: Arc<dyn DocumentStore>,
    pub chunks: Arc<dyn kb_ports::ChunkStore>,
    /// Object storage. Only the hard-delete path calls it today.
    pub blob: Arc<dyn BlobStore>,
    pub embedder: Arc<dyn Embedder>,
    pub engine: Engine,
}

impl AppState {
    /// Build state from a typed config.
    pub async fn from_config(config: crate::Config) -> anyhow::Result<Self> {
        let documents: Arc<dyn DocumentStore> = Arc::new(
            kb_store_pg::PgDocumentStore::new(config.pg)
                .await
                .map_err(|e| anyhow::anyhow!("pg: {e}"))?,
        );
        let chunks: Arc<dyn kb_ports::ChunkStore> = Arc::new(
            kb_store_surreal::SurrealChunkStore::new(config.surreal)
                .map_err(|e| anyhow::anyhow!("surreal: {e}"))?,
        );
        let blob: Arc<dyn BlobStore> = Arc::new(
            kb_store_blob::S3BlobStore::new(config.s3).map_err(|e| anyhow::anyhow!("s3: {e}"))?,
        );
        let embedder: Arc<dyn Embedder> = Arc::new(
            kb_inference::EmbedderImpl::new(embedder_settings_from(config.embedder.clone()))
                .map_err(|e| anyhow::anyhow!("embedder: {e}"))?,
        );
        let reranker: Arc<dyn kb_ports::Reranker> =
            build_reranker(&config.reranker).map_err(|e| anyhow::anyhow!("reranker: {e}"))?;
        let expander: Arc<dyn QueryExpander> = if config.expansion.enabled {
            let bridge = QueryExpanderBridge::new(config.expansion.clone())
                .map_err(|e| anyhow::anyhow!("expander: {e}"))?;
            Arc::new(bridge)
        } else {
            Arc::new(NoopExpander)
        };

        let inputs = EngineInputs {
            embedder: embedder.clone(),
            chunk_store: chunks.clone(),
            reranker,
            document_store: documents.clone(),
            query_expander: expander,
        };
        let engine = Engine::new(config.engine, inputs);
        Ok(Self {
            documents,
            chunks,
            blob,
            embedder,
            engine,
        })
    }

    /// Construct a state directly from arcs. Used in tests so we don't have to
    /// stand up Postgres/Surreal.
    #[must_use]
    pub fn from_arcs(
        documents: Arc<dyn DocumentStore>,
        chunks: Arc<dyn kb_ports::ChunkStore>,
        blob: Arc<dyn BlobStore>,
        embedder: Arc<dyn Embedder>,
        reranker: Arc<dyn kb_ports::Reranker>,
        engine_settings: EngineSettings,
    ) -> Self {
        let expander: Arc<dyn QueryExpander> = Arc::new(NoopExpander);
        let inputs = EngineInputs {
            embedder: embedder.clone(),
            chunk_store: chunks.clone(),
            reranker,
            document_store: documents.clone(),
            query_expander: expander,
        };
        let engine = Engine::new(engine_settings, inputs);
        Self {
            documents,
            chunks,
            blob,
            embedder,
            engine,
        }
    }

    /// Assemble the production [`IngestWorker`](kb_engine::IngestWorker) using
    /// production adapters: Postgres job queue, Surreal chunk store, S3 blob
    /// store, and the real local extractor (`kb-extract`).
    pub async fn build_worker(
        config: &crate::Config,
        settings: kb_engine::IngestSettings,
    ) -> anyhow::Result<kb_engine::IngestWorker> {
        let documents = Arc::new(
            kb_store_pg::PgDocumentStore::new(config.pg.clone())
                .await
                .map_err(|e| anyhow::anyhow!("pg documents: {e}"))?,
        );
        let jobs = Arc::new(
            kb_store_pg::PgJobStore::connect(&config.pg)
                .await
                .map_err(|e| anyhow::anyhow!("pg jobs: {e}"))?,
        );
        let chunks = Arc::new(
            kb_store_surreal::SurrealChunkStore::new(config.surreal.clone())
                .map_err(|e| anyhow::anyhow!("surreal: {e}"))?,
        );
        let blobs = Arc::new(
            kb_store_blob::S3BlobStore::new(config.s3.clone())
                .map_err(|e| anyhow::anyhow!("s3: {e}"))?,
        );
        let embedder = Arc::new(
            kb_inference::EmbedderImpl::new(embedder_settings_from(config.embedder.clone()))
                .map_err(|e| anyhow::anyhow!("embedder: {e}"))?,
        );
        let extractor = kb_extract::LocalExtractor::shared();
        Ok(kb_engine::IngestWorker::new(
            jobs, documents, chunks, blobs, extractor, embedder, settings,
        ))
    }

    /// Build an [`IngestWorker`](kb_engine::IngestWorker) reusing this state's
    /// already-initialized stores and the real local extractor (`kb-extract`).
    pub async fn build_ingest_worker(
        &self,
        config: &crate::Config,
        settings: kb_engine::IngestSettings,
    ) -> anyhow::Result<kb_engine::IngestWorker> {
        let jobs = Arc::new(
            kb_store_pg::PgJobStore::connect(&config.pg)
                .await
                .map_err(|e| anyhow::anyhow!("pg jobs: {e}"))?,
        );
        let extractor = kb_extract::LocalExtractor::shared();
        Ok(kb_engine::IngestWorker::new(
            jobs,
            self.documents.clone(),
            self.chunks.clone(),
            self.blob.clone(),
            extractor,
            self.embedder.clone(),
            settings,
        ))
    }
}

fn embedder_settings_from(c: EmbedderConfig) -> EmbedderSettings {
    EmbedderSettings {
        base_url: c.base_url,
        model: c.model,
        api_key: c.api_key,
        dimension: c.dimension,
        batch_size: c.batch_size,
        embed_concurrency: c.embed_concurrency,
        query_prefix: c.query_prefix,
        passage_prefix: c.passage_prefix,
        request_timeout: c.request_timeout,
    }
}

fn build_reranker(cfg: &RerankerConfig) -> anyhow::Result<std::sync::Arc<dyn kb_ports::Reranker>> {
    use kb_inference::{CohereReranker, LlmReranker, TeiReranker, VllmReranker};
    match cfg {
        RerankerConfig::Disabled => Ok(Arc::new(kb_inference::NoopReranker)),
        RerankerConfig::Llm { model, api_key } => {
            let r = LlmReranker::new(model, api_key).map_err(|e| anyhow::anyhow!("llm: {e}"))?;
            Ok(Arc::new(r))
        }
        RerankerConfig::Tei { base_url, model } => {
            let r = TeiReranker::new(base_url, model).map_err(|e| anyhow::anyhow!("tei: {e}"))?;
            Ok(Arc::new(r))
        }
        RerankerConfig::Vllm { base_url, model } => {
            let r = VllmReranker::new(base_url, model).map_err(|e| anyhow::anyhow!("vllm: {e}"))?;
            Ok(Arc::new(r))
        }
        RerankerConfig::Cohere {
            model,
            api_key,
            endpoint,
        } => {
            let r = CohereReranker::new(model, api_key, endpoint.as_deref())
                .map_err(|e| anyhow::anyhow!("cohere: {e}"))?;
            Ok(Arc::new(r))
        }
    }
}

// `kb-inference` has its own `QueryExpander`. We bridge it to the engine's
// trait object so the engine stays dep-free of `kb-inference`.

mod expander_bridge {
    use super::*;
    use async_trait::async_trait;

    pub struct QueryExpanderBridge {
        inner: kb_inference::QueryExpander,
    }

    impl QueryExpanderBridge {
        pub fn new(settings: ExpansionSettings) -> anyhow::Result<Self> {
            let inner =
                kb_inference::QueryExpander::new(settings).map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(Self { inner })
        }
    }

    #[async_trait]
    impl QueryExpander for QueryExpanderBridge {
        async fn expand(&self, query: &str) -> Result<Vec<String>, PortError> {
            // `kb-inference` returns `[original]` on every failure and never
            // errors. Its contract is the same as `expandQuery` in
            // src/lib/rag/query-expansion.ts.
            Ok(self.inner.expand(query).await)
        }
    }
}
pub use expander_bridge::QueryExpanderBridge;
