//! `AppState`: the whole graph of adapters + the engine, behind `Arc`s.
//!
//! This is the composition root's only shared value: handlers receive it
//! through axum's `State` extractor and never build an adapter themselves.

use std::sync::Arc;

use kb_engine::{Engine, EngineInputs, EngineSettings, NoopExpander, QueryExpander};
use kb_inference::EmbedderSettings;
use kb_inference::ExpansionSettings;
use kb_ports::{BlobStore, DocumentStore, Embedder, EntityStore, GraphStore, JobStore, PortError};

use crate::config::{EmbedderConfig, RerankerConfig};
use crate::events::EventBus;

/// Version string reported by `GET /health` (`KB_VERSION`, defaulting to
/// "dev" exactly as the TypeScript service does).
pub const DEFAULT_VERSION: &str = "dev";

pub struct AppState {
    pub documents: Arc<dyn DocumentStore>,
    pub index: Arc<dyn kb_ports::RetrievalIndex>,
    /// Entity and relation graph in SurrealDB, read by the intelligence route.
    pub entities: Arc<dyn EntityStore>,
    /// The Postgres entity graph the retrieval arm walks, when one is configured
    /// (`KB_GRAPH_STORE`). `None` means there is no graph, and the arm is off.
    pub graph: Option<Arc<dyn GraphStore>>,
    /// The graph retrieval arm, or `None` when `KB_GRAPH_ARM` is off.
    pub graph_arm: Option<Arc<kb_engine::GraphArm>>,
    /// Object storage: the upload route writes, the raw/asset routes read, the
    /// hard-delete and reaper paths remove.
    pub blob: Arc<dyn BlobStore>,
    pub embedder: Arc<dyn Embedder>,
    /// Ingest queue, written by the upload route and read by the job routes.
    pub jobs: Arc<dyn JobStore>,
    /// In-process fan-out between the ingest worker and `GET /v1/events`.
    pub events: Arc<EventBus>,
    pub engine: Engine,
    pub version: String,
}

/// Adapters a caller supplies. Grouped so tests can build a state from fakes
/// without naming every constructor argument.
pub struct StateParts {
    pub documents: Arc<dyn DocumentStore>,
    pub index: Arc<dyn kb_ports::RetrievalIndex>,
    pub entities: Arc<dyn EntityStore>,
    pub graph: Option<Arc<dyn GraphStore>>,
    pub graph_arm: Option<Arc<kb_engine::GraphArm>>,
    pub blob: Arc<dyn BlobStore>,
    pub embedder: Arc<dyn Embedder>,
    pub jobs: Arc<dyn JobStore>,
    pub events: Arc<EventBus>,
    pub version: String,
}

impl AppState {
    /// Build state from a typed config, connecting every adapter once so the
    /// process shares a single Postgres pool.
    pub async fn from_config(config: crate::Config) -> anyhow::Result<Self> {
        let pool = kb_store_pg::connect_pool(&config.pg)
            .await
            .map_err(|e| anyhow::anyhow!("pg: {e}"))?;
        // The graph store shares the pool the rest of the process already holds:
        // three Postgres tables are not worth a second connection pool.
        let graph: Option<Arc<dyn GraphStore>> = match config.graph_store {
            crate::config::GraphStoreChoice::None => None,
            crate::config::GraphStoreChoice::Pg => {
                Some(Arc::new(kb_store_pg::PgGraphStore::from_pool(pool.clone())))
            }
        };
        let graph_arm = match (config.graph_arm.enabled, &graph) {
            (true, Some(store)) => Some(Arc::new(kb_engine::GraphArm::new(
                Arc::clone(store),
                kb_engine::GraphArmConfig {
                    max_seeds: config.graph_arm.max_seeds,
                    tolerance: config.graph_arm.tolerance,
                    damping: config.graph_arm.damping,
                    candidates: config.graph_arm.candidates,
                    cache_entries: config.graph_arm.cache_entries,
                    cache_bytes: config.graph_arm.cache_bytes,
                    ..kb_engine::GraphArmConfig::default()
                },
            ))),
            (true, None) => {
                // Config rejects this combination at boot; a process built by
                // hand should still not start an arm with nothing behind it.
                anyhow::bail!("graph arm is enabled but no graph store is configured");
            }
            (false, _) => None,
        };
        let blob: Arc<dyn BlobStore> = Arc::new(
            kb_store_blob::S3BlobStore::new(config.s3).map_err(|e| anyhow::anyhow!("s3: {e}"))?,
        );
        let page_arm = match config.pages.as_ref() {
            None => None,
            Some(pages) => Some(Arc::new(
                build_page_arm(
                    &config.qdrant,
                    pages,
                    config.cache.as_ref(),
                    Arc::clone(&blob),
                )
                .await?,
            )),
        };
        let documents: Arc<dyn DocumentStore> =
            Arc::new(kb_store_pg::PgDocumentStore::from_pool(pool.clone()));
        let jobs: Arc<dyn JobStore> = Arc::new(kb_store_pg::PgJobStore::from_pool(pool));
        let index: Arc<dyn kb_ports::RetrievalIndex> = match config.chunk_store_choice {
            crate::config::ChunkStoreChoice::Surreal => {
                let store = kb_store_surreal::SurrealChunkStore::new(config.surreal.clone())
                    .map_err(|e| anyhow::anyhow!("surreal: {e}"))?;
                Arc::new(store)
            }
            crate::config::ChunkStoreChoice::Qdrant => {
                let collection = config
                    .qdrant_collection_override
                    .unwrap_or_else(|| config.qdrant.collection_name());
                let client = config
                    .qdrant
                    .client()
                    .map_err(|e| anyhow::anyhow!("qdrant client: {e}"))?;
                kb_store_qdrant::create_collection(&client, &config.qdrant, &collection)
                    .await
                    .map_err(|e| anyhow::anyhow!("qdrant create collection: {e}"))?;
                let q_index =
                    kb_store_qdrant::QdrantIndex::new_with_collection(config.qdrant, collection)
                        .map_err(|e| anyhow::anyhow!("qdrant index: {e}"))?;
                Arc::new(q_index)
            }
        };
        let entities: Arc<dyn EntityStore> = Arc::new(
            kb_store_surreal::SurrealChunkStore::new(config.surreal.clone())
                .map_err(|e| anyhow::anyhow!("surreal entities: {e}"))?,
        );
        let mut embedder: Arc<dyn Embedder> = Arc::new(
            kb_inference::EmbedderImpl::new(embedder_settings_from(config.embedder.clone()))
                .map_err(|e| anyhow::anyhow!("embedder: {e}"))?,
        );
        if let Some(cache) = config.cache.as_ref() {
            let fingerprint = config.embedder.cache_fingerprint(&cache.embedding_tag);
            tracing::info!(dir = %cache.dir.display(), %fingerprint, "embedding disk cache on");
            embedder = Arc::new(
                kb_cache::CachedEmbedder::new(
                    embedder,
                    kb_cache::DiskCache::new(&cache.dir),
                    fingerprint,
                )
                .with_prefixes(
                    &config.embedder.query_prefix,
                    &config.embedder.passage_prefix,
                ),
            );
        }
        let mut reranker: Arc<dyn kb_ports::Reranker> =
            build_reranker(&config.reranker).map_err(|e| anyhow::anyhow!("reranker: {e}"))?;
        if let (Some(cache), Some(fingerprint)) = (
            config.cache.as_ref(),
            reranker_fingerprint(&config.reranker),
        ) {
            let fingerprint = format!("{fingerprint}|tag={}", cache.embedding_tag);
            tracing::info!(%fingerprint, "rerank disk cache on");
            reranker = Arc::new(kb_cache::CachedReranker::new(
                reranker,
                kb_cache::DiskCache::new(&cache.dir),
                fingerprint,
            ));
        }
        let expander: Arc<dyn QueryExpander> = if config.expansion.enabled {
            let bridge = QueryExpanderBridge::new(config.expansion.clone())
                .map_err(|e| anyhow::anyhow!("expander: {e}"))?;
            Arc::new(bridge)
        } else {
            Arc::new(NoopExpander)
        };

        let inputs = EngineInputs {
            embedder: embedder.clone(),
            index: index.clone(),
            reranker,
            document_store: documents.clone(),
            query_expander: expander,
            figure_gate: None,
            graph_arm: graph_arm.clone(),
            page_arm,
        };
        let engine = Engine::new(config.engine, inputs);
        Ok(Self {
            documents,
            index,
            entities,
            graph,
            graph_arm,
            blob,
            embedder,
            jobs,
            events: EventBus::new(),
            engine,
            version: config.version,
        })
    }

    /// Construct a state from explicit adapters. Used in tests so they don't
    /// have to stand up Postgres/Surreal.
    #[must_use]
    pub fn from_parts(
        parts: StateParts,
        reranker: Arc<dyn kb_ports::Reranker>,
        settings: EngineSettings,
    ) -> Self {
        let inputs = EngineInputs {
            embedder: parts.embedder.clone(),
            index: parts.index.clone(),
            reranker,
            document_store: parts.documents.clone(),
            query_expander: Arc::new(NoopExpander),
            figure_gate: None,
            graph_arm: parts.graph_arm.clone(),
            page_arm: None,
        };
        let engine = Engine::new(settings, inputs);
        Self {
            documents: parts.documents,
            index: parts.index,
            entities: parts.entities,
            graph: parts.graph,
            graph_arm: parts.graph_arm,
            blob: parts.blob,
            embedder: parts.embedder,
            jobs: parts.jobs,
            events: parts.events,
            engine,
            version: parts.version,
        }
    }

    /// Assemble a production [`IngestWorker`](kb_engine::IngestWorker) from the
    /// same pool and stores this state already holds, so the HTTP process and
    /// its worker share one database connection and one event bus.
    ///
    /// # Errors
    ///
    /// Fails when the entity extractor or extractor chain cannot be built.
    pub fn build_worker(
        &self,
        config: &crate::Config,
        settings: kb_engine::IngestSettings,
    ) -> anyhow::Result<kb_engine::IngestWorker> {
        let mut extractor = kb_extract::build_production_extractor(&config.extract)
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
        let entity_extractor: Arc<dyn kb_ports::EntityExtractor> = match config.entity_extractor {
            crate::config::EntityExtractorChoice::Heuristic => {
                Arc::new(kb_inference::HeuristicExtractor::default())
            }
            crate::config::EntityExtractorChoice::Llm => Arc::new(
                kb_inference::LlmCombinedExtractor::new(config.entity.clone())
                    .map_err(|e| anyhow::anyhow!("entity extractor: {e}"))?,
            ),
        };
        let mut worker = kb_engine::IngestWorker::new(
            self.jobs.clone(),
            self.documents.clone(),
            self.index.clone(),
            self.blob.clone(),
            extractor,
            self.embedder.clone(),
            settings,
        )
        .with_entity_store(self.entities.clone())
        .with_entity_extractor(entity_extractor)
        .with_progress_sink(Arc::clone(&self.events) as Arc<dyn kb_ports::ProgressSink>);
        if let Some(graph) = self.graph.as_ref() {
            worker = worker.with_graph_store(Arc::clone(graph));
        }
        if let (Some(arm), Some(pages)) =
            (self.engine.inputs.page_arm.as_ref(), config.pages.as_ref())
        {
            // Only the vision rerank reads the stored page images, so only it
            // turns the writes on: otherwise every ingested page is a JPEG in
            // the bucket with no consumer.
            let page_blobs = pages
                .vision
                .as_ref()
                .map(|_| Arc::clone(&self.blob) as Arc<dyn kb_ports::BlobStore>);
            let caption = match pages.caption.as_ref() {
                None => None,
                Some(caption) => Some(build_page_caption(caption, config.cache.as_ref())?),
            };
            worker = worker.with_page_ingest(Arc::new(kb_engine::PageIngest {
                renderer: Arc::new(kb_extract::PdfiumPageRenderer {
                    dpi: pages.dpi,
                    jpeg_quality: 80,
                }),
                embedder: Arc::clone(&arm.embedder),
                index: Arc::clone(&arm.index),
                blobs: page_blobs,
                max_pages: pages.max_pages,
                text_chars: pages.text_chars,
                caption,
            }));
        }
        Ok(worker)
    }
}

/// The opt-in ingest-time page describer, wrapped in its disk cache when
/// `KB_CACHE_DIR` is set. The fingerprint names everything but the image that
/// changes an answer, so a prompt or model change never reuses an old one.
fn build_page_caption(
    caption: &crate::config::PageCaptionSettings,
    cache: Option<&crate::config::CacheConfig>,
) -> anyhow::Result<kb_engine::PageCaption> {
    let describer: Arc<dyn kb_ports::PageDescriber> = Arc::new(
        kb_inference::HttpPageDescriber::new(caption.describer.clone())
            .map_err(|e| anyhow::anyhow!("page describer: {e}"))?,
    );
    let describer: Arc<dyn kb_ports::PageDescriber> = match cache {
        Some(cache) => {
            let fingerprint = format!(
                "caption|{}|prompt={}|reasoning={}",
                caption.describer.model,
                kb_inference::PAGE_DESCRIBE_PROMPT_VERSION,
                if caption.describer.no_reasoning {
                    "off"
                } else {
                    "on"
                },
            );
            tracing::info!(
                %fingerprint,
                dir = %cache.dir.display(),
                "page caption disk cache on"
            );
            Arc::new(kb_cache::CachedPageDescriber::new(
                describer,
                kb_cache::DiskCache::new(&cache.dir),
                fingerprint,
            ))
        }
        None => describer,
    };
    Ok(kb_engine::PageCaption {
        describer,
        config: caption.config,
    })
}

/// The page-image arm: its own Qdrant collection (one per page-embedding
/// model, so vector spaces never mix) and its embedder, disk-cached when
/// `KB_CACHE_DIR` is set.
async fn build_page_arm(
    qdrant: &kb_store_qdrant::QdrantSettings,
    pages: &crate::config::PageArmSettings,
    cache: Option<&crate::config::CacheConfig>,
    blobs: Arc<dyn kb_ports::BlobStore>,
) -> anyhow::Result<kb_engine::PageArm> {
    let settings = kb_store_qdrant::QdrantSettings {
        model_slug: pages.embedder.model.clone(),
        dense_dims: Some(pages.embedder.dimension),
        ..qdrant.clone()
    };
    let collection = format!("pages__{}", settings.collection_name());
    let client = settings
        .client()
        .map_err(|e| anyhow::anyhow!("qdrant client: {e}"))?;
    kb_store_qdrant::create_collection(&client, &settings, &collection)
        .await
        .map_err(|e| anyhow::anyhow!("qdrant create page collection: {e}"))?;
    let index: Arc<dyn kb_ports::RetrievalIndex> = Arc::new(
        kb_store_qdrant::QdrantIndex::new_with_collection(settings, collection.clone())
            .map_err(|e| anyhow::anyhow!("qdrant page index: {e}"))?,
    );
    let mut embedder: Arc<dyn kb_ports::PageEmbedder> = Arc::new(
        kb_inference::HttpPageEmbedder::new(pages.embedder.clone())
            .map_err(|e| anyhow::anyhow!("page embedder: {e}"))?,
    );
    if let Some(cache) = cache {
        let fingerprint = format!(
            "model={}|dim={}|tag={}",
            pages.embedder.model, pages.embedder.dimension, cache.embedding_tag
        );
        embedder = Arc::new(kb_cache::CachedPageEmbedder::new(
            embedder,
            kb_cache::DiskCache::new(&cache.dir),
            fingerprint,
        ));
    }
    tracing::info!(%collection, model = %pages.embedder.model, "page-image arm on");
    let vision = match pages.vision.as_ref() {
        None => None,
        Some(settings) => {
            let scorer: Arc<dyn kb_ports::PageVisionScorer> = Arc::new(
                kb_inference::HttpPageVisionScorer::new(settings.scorer.clone())
                    .map_err(|e| anyhow::anyhow!("vision scorer: {e}"))?,
            );
            let scorer: Arc<dyn kb_ports::PageVisionScorer> = match cache {
                Some(cache) => {
                    let fingerprint = format!(
                        "vision|{}|prompt={}|reasoning={}",
                        settings.scorer.model,
                        kb_inference::VISION_PROMPT_VERSION,
                        if settings.scorer.no_reasoning {
                            "off"
                        } else {
                            "on"
                        },
                    );
                    tracing::info!(
                        %fingerprint,
                        dir = %cache.dir.display(),
                        "vision rerank disk cache on"
                    );
                    Arc::new(kb_cache::CachedPageVisionScorer::new(
                        scorer,
                        kb_cache::DiskCache::new(&cache.dir),
                        fingerprint,
                    ))
                }
                None => scorer,
            };
            tracing::info!(
                model = %settings.scorer.model,
                depth = settings.config.depth,
                "vision rerank on"
            );
            Some(kb_engine::PageVision {
                scorer,
                blobs,
                config: settings.config,
            })
        }
    };
    Ok(kb_engine::PageArm {
        embedder,
        index,
        config: pages.arm,
        vision,
    })
}

/// What a rerank score depends on: provider, model and (for a hosted
/// endpoint) which service answers. Never an API key. `None` for a disabled
/// reranker, which has nothing worth caching.
fn reranker_fingerprint(cfg: &RerankerConfig) -> Option<String> {
    match cfg {
        RerankerConfig::Disabled => None,
        RerankerConfig::Llm { model, .. } => Some(format!("llm|{model}")),
        RerankerConfig::Tei { model, .. } => Some(format!("tei|{model}")),
        RerankerConfig::Vllm { model, .. } => Some(format!("vllm|{model}")),
        RerankerConfig::Cohere {
            model, endpoint, ..
        } => Some(format!(
            "cohere|{model}|{}",
            endpoint.as_deref().unwrap_or("default")
        )),
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
        send_input_type: c.send_input_type,
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
