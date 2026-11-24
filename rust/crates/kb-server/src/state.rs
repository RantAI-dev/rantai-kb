//! `AppState`: the whole graph of adapters + the engine, behind `Arc`s.
//!
//! This is the composition root's only shared value: handlers receive it
//! through axum's `State` extractor and never build an adapter themselves.

use std::sync::Arc;

use kb_engine::{Engine, EngineInputs, EngineSettings, NoopExpander, QueryExpander};
use kb_inference::EmbedderSettings;
use kb_inference::ExpansionSettings;
use kb_ports::{
    BlobStore, DocumentStore, Embedder, EntityStore, GraphStore, JobStore, PortError, TableStore,
};

use crate::config::{EmbedderConfig, RerankerConfig};
use crate::events::EventBus;

/// Version string reported by `GET /health` (`KB_VERSION`, defaulting to
/// "dev" exactly as the TypeScript service does).
pub const DEFAULT_VERSION: &str = "dev";

pub struct AppState {
    pub documents: Arc<dyn DocumentStore>,
    pub index: Arc<dyn kb_ports::RetrievalIndex>,
    /// The document-intelligence read path, backed by the Postgres entity
    /// graph. `None` when `KB_GRAPH_STORE` is unset: there is no graph, so the
    /// route returns an empty view rather than inventing one.
    pub entities: Option<Arc<dyn EntityStore>>,
    /// The Postgres entity graph the retrieval arm walks, when one is configured
    /// (`KB_GRAPH_STORE`). `None` means there is no graph, and the arm is off.
    pub graph: Option<Arc<dyn GraphStore>>,
    /// The graph retrieval arm, or `None` when `KB_GRAPH_ARM` is off.
    pub graph_arm: Option<Arc<kb_engine::GraphArm>>,
    /// Object storage: the upload route writes, the raw/asset routes read, the
    /// hard-delete and reaper paths remove.
    pub blob: Arc<dyn BlobStore>,
    /// Stored tables (`TBL-1`), or `None` when `KB_TABLES=false`. With no store
    /// the `/v1/tables` routes 404, ingest stores nothing, and search is
    /// exactly what it was.
    pub tables: Option<Arc<dyn TableStore>>,
    pub embedder: Arc<dyn Embedder>,
    /// Ingest queue, written by the upload route and read by the job routes.
    pub jobs: Arc<dyn JobStore>,
    /// In-process fan-out between the ingest worker and `GET /v1/events`.
    pub events: Arc<EventBus>,
    pub engine: Engine,
    pub version: String,
    /// `KB_TIER` name (`lite`, `cloud`, …), or `None` when no tier is set.
    /// Reported by `GET /health`.
    pub tier: Option<String>,
    /// Hosts of configured model endpoints that are not local, sorted and
    /// de-duplicated. Reported by `GET /health` so a caller can refuse to send
    /// content that would leave the platform.
    pub egress: Vec<String>,
    /// The model backends, probed by `GET /health` so a client can see
    /// whether the embedder, the reranker and the OCR backend can serve
    /// (`#36`).
    pub backends: crate::backends::BackendProbe,
    /// What the upload route does with a file the tenant already has
    /// (`KB_UPLOAD_DEDUPE`). `reject` by default: the same bytes twice cost a
    /// whole ingest and produce a second copy of the same answer.
    pub upload_dedupe: kb_core::policy::UploadDedupe,
    /// Whether the thin MCP endpoint is mounted (`KB_MCP`, default off).
    pub mcp_enabled: bool,
}

/// Adapters a caller supplies. Grouped so tests can build a state from fakes
/// without naming every constructor argument.
pub struct StateParts {
    pub documents: Arc<dyn DocumentStore>,
    pub index: Arc<dyn kb_ports::RetrievalIndex>,
    pub entities: Option<Arc<dyn EntityStore>>,
    pub graph: Option<Arc<dyn GraphStore>>,
    pub graph_arm: Option<Arc<kb_engine::GraphArm>>,
    pub blob: Arc<dyn BlobStore>,
    pub embedder: Arc<dyn Embedder>,
    pub jobs: Arc<dyn JobStore>,
    pub events: Arc<EventBus>,
    /// The table store, when the caller wants one. `None` in a test state that
    /// does not exercise tables.
    pub tables: Option<Arc<dyn TableStore>>,
    pub version: String,
    /// What the upload route does with a file the tenant already has
    /// (`KB_UPLOAD_DEDUPE`).
    pub upload_dedupe: kb_core::policy::UploadDedupe,
}

impl AppState {
    /// Build state from a typed config, connecting every adapter once so the
    /// process shares a single Postgres pool.
    pub async fn from_config(config: crate::Config) -> anyhow::Result<Self> {
        // Both are pure reads of the resolved config; take them before any
        // field is moved into an adapter.
        let tier = config
            .tier
            .as_ref()
            .map(|plan| plan.tier.as_str().to_string());
        let egress = config.egress_hosts();
        // The model backends the health route probes (`#36`). Read before any
        // field is moved out of the config.
        let backends = crate::backends::BackendTargets::from_config(&config);
        let pool = kb_store_pg::connect_pool(&config.pg)
            .await
            .map_err(|e| anyhow::anyhow!("pg: {e}"))?;
        // The graph store shares the pool the rest of the process already holds:
        // three Postgres tables are not worth a second connection pool. It backs
        // both the graph arm and the intelligence route.
        let pg_graph: Option<Arc<kb_store_pg::PgGraphStore>> = match config.graph_store {
            crate::config::GraphStoreChoice::None => None,
            crate::config::GraphStoreChoice::Pg => {
                Some(Arc::new(kb_store_pg::PgGraphStore::from_pool(pool.clone())))
            }
        };
        let graph: Option<Arc<dyn GraphStore>> = pg_graph
            .as_ref()
            .map(|store| Arc::clone(store) as Arc<dyn GraphStore>);
        let entities: Option<Arc<dyn EntityStore>> =
            pg_graph.map(|store| store as Arc<dyn EntityStore>);
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
        // The global (graph community) arm, `DEC-5`. Off unless the operator
        // turned `KB_GLOBAL_ARM` on; the store reads the `KbCommunity` table the
        // operator command `communities build` writes.
        let global_arm = if config.global_arm.enabled {
            let store: Arc<dyn kb_ports::CommunityStore> =
                Arc::new(kb_store_pg::PgCommunityStore::from_pool(pool.clone()));
            Some(Arc::new(kb_engine::GlobalArm::new(
                store,
                kb_engine::GlobalArmConfig {
                    communities: config.global_arm.communities,
                    weight: config.global_arm.weight,
                    chunks: config.global_arm.chunks,
                },
            )))
        } else {
            None
        };
        let blob: Arc<dyn BlobStore> = Arc::new(
            kb_store_blob::S3BlobStore::new(config.s3).map_err(|e| anyhow::anyhow!("s3: {e}"))?,
        );
        // The table store shares the pool and the bucket: it is Postgres rows,
        // Parquet blobs and an in-memory DuckDB over one cached file.
        let tables: Option<Arc<dyn TableStore>> = if config.tables.enabled {
            Some(Arc::new(kb_store_tables::TablesStore::from_pool(
                pool.clone(),
                Arc::clone(&blob),
                kb_store_tables::TableStoreSettings {
                    cache_dir: config.tables.cache_dir.clone(),
                    query_timeout_ms: config.tables.query_timeout_ms,
                },
            )))
        } else {
            None
        };
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
        let index: Arc<dyn kb_ports::RetrievalIndex> = {
            let collection = config
                .qdrant_collection_override
                .clone()
                .unwrap_or_else(|| config.qdrant.collection_name());
            let client = config
                .qdrant
                .client()
                .map_err(|e| anyhow::anyhow!("qdrant client: {e}"))?;
            kb_store_qdrant::create_collection(&client, &config.qdrant, &collection)
                .await
                .map_err(|e| anyhow::anyhow!("qdrant create collection: {e}"))?;
            let q_index = kb_store_qdrant::QdrantIndex::new_with_collection(
                config.qdrant.clone(),
                collection,
            )
            .map_err(|e| anyhow::anyhow!("qdrant index: {e}"))?;
            Arc::new(q_index)
        };
        let mut embedder: Arc<dyn Embedder> = Arc::new(
            kb_inference::EmbedderImpl::new(embedder_settings_from(config.embedder.clone()))
                .map_err(|e| anyhow::anyhow!("embedder: {e}"))?,
        );
        if let Some(cache) = config.cache.as_ref() {
            let fingerprint = config.embedder.cache_fingerprint(&cache.embedding_tag);
            let (query_prefix, passage_prefix) = config.embedder.effective_prefixes();
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

        let tree_arm = config
            .tree_arm
            .map(|tree| Arc::new(kb_engine::TreeArm::new(tree)));
        let inputs = EngineInputs {
            embedder: embedder.clone(),
            index: index.clone(),
            reranker,
            document_store: documents.clone(),
            query_expander: expander,
            figure_gate: None,
            graph_arm: graph_arm.clone(),
            global_arm: global_arm.clone(),
            page_arm,
            tree_arm,
        };
        let engine = Engine::new(config.engine, inputs);
        Ok(Self {
            documents,
            index,
            entities,
            graph,
            graph_arm,
            blob,
            tables,
            embedder,
            jobs,
            events: EventBus::new(),
            engine,
            version: config.version,
            tier,
            egress,
            // The backends the health route probes (`#36`).
            backends: crate::backends::BackendProbe::new(backends),
            upload_dedupe: config.upload_dedupe,
            mcp_enabled: config.mcp_enabled,
        })
    }

    /// Construct a state from explicit adapters. Used in tests so they don't
    /// have to stand up Postgres/Qdrant.
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
            // A hand-built state carries no `KbCommunity` store; a test that
            // wants the arm installs it on `state.engine.inputs` directly.
            global_arm: None,
            page_arm: None,
            tree_arm: None,
        };
        let engine = Engine::new(settings, inputs);
        Self {
            documents: parts.documents,
            index: parts.index,
            entities: parts.entities,
            graph: parts.graph,
            graph_arm: parts.graph_arm,
            blob: parts.blob,
            tables: parts.tables,
            embedder: parts.embedder,
            jobs: parts.jobs,
            events: parts.events,
            engine,
            version: parts.version,
            // Hand-built test states carry no config, so nothing can leave the
            // process through a model endpoint they did not configure.
            tier: None,
            egress: Vec::new(),
            // No configured backends: a hand-built state has no model
            // endpoints to probe.
            backends: crate::backends::BackendProbe::default(),
            upload_dedupe: parts.upload_dedupe,
            // A hand-built state has no config: MCP is off unless a test turns
            // it on explicitly with `with_mcp`.
            mcp_enabled: false,
        }
    }

    /// Turn the MCP endpoint on or off on a hand-built state (tests).
    #[must_use]
    pub fn with_mcp(mut self, enabled: bool) -> Self {
        self.mcp_enabled = enabled;
        self
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
        .with_progress_sink(Arc::clone(&self.events) as Arc<dyn kb_ports::ProgressSink>);
        if let Some(graph) = self.graph.as_ref() {
            // Entity extraction has exactly one destination now: the Postgres
            // graph. With no graph there is nowhere to put the result, so the
            // extractor is not built and no paid call is made.
            worker = worker
                .with_graph_store(Arc::clone(graph))
                .with_entity_extractor(entity_extractor);
        }
        if config.visuals.enabled {
            worker = worker.with_visuals(Arc::new(build_visuals(
                &config.visuals,
                config.cache.as_ref(),
            )?));
        }
        if let Some(tables) = self.tables.as_ref() {
            worker = worker.with_table_store(Arc::clone(tables));
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

/// The visual stage (`ING-5`): the local collector, and the image describer
/// wrapped in its disk cache when `KB_CACHE_DIR` is set. The fingerprint names
/// everything but the image that changes an answer.
fn build_visuals(
    visuals: &crate::config::VisualConfig,
    cache: Option<&crate::config::CacheConfig>,
) -> anyhow::Result<kb_engine::visuals::VisualPipeline> {
    let describer = match &visuals.describer {
        None => None,
        Some(settings) => {
            let inner: Arc<dyn kb_ports::ImageDescriber> = Arc::new(
                kb_inference::HttpImageDescriber::new(settings.clone())
                    .map_err(|e| anyhow::anyhow!("image describer: {e}"))?,
            );
            Some(match cache {
                Some(cache) => {
                    let fingerprint = format!(
                        "image-read|{}|prompt={}|lang={}|tokens={}|reasoning={}",
                        settings.model,
                        kb_inference::IMAGE_READ_PROMPT_VERSION,
                        settings.language,
                        settings.max_tokens,
                        if settings.no_reasoning { "off" } else { "on" },
                    );
                    tracing::info!(%fingerprint, "image reading disk cache on");
                    Arc::new(kb_cache::CachedImageDescriber::new(
                        inner,
                        kb_cache::DiskCache::new(&cache.dir),
                        fingerprint,
                    )) as Arc<dyn kb_ports::ImageDescriber>
                }
                None => inner,
            })
        }
    };
    Ok(kb_engine::visuals::VisualPipeline::new(
        Arc::new(kb_extract::LocalVisualCollector),
        describer,
        visuals.settings.clone(),
    ))
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
pub(crate) fn reranker_fingerprint(cfg: &RerankerConfig) -> Option<String> {
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

pub(crate) fn embedder_settings_from(c: EmbedderConfig) -> EmbedderSettings {
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
        format: c.format,
        request_timeout: c.request_timeout,
    }
}

pub(crate) fn build_reranker(
    cfg: &RerankerConfig,
) -> anyhow::Result<std::sync::Arc<dyn kb_ports::Reranker>> {
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
