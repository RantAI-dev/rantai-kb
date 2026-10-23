//! The leak matrix, run in process against the real router and the fakes.
//!
//! The instrument is a black-box HTTP client, so the only honest way to test it
//! is to serve the real router on a loopback port and point the instrument at
//! it: real HTTP, real auth, real JSON, real scope resolution, real retrieval
//! arms. Nothing about the retrieval code is stubbed from the instrument's side.
//!
//! Runs:
//!
//! * the **honest** store, where every arm filters by scope — the matrix must
//!   come back with positive controls that hit and no leaks. The page-image arm
//!   is on, backed by the reference `InMemoryIndex`;
//! * a **deliberately leaking** store, whose vector arm ignores the scope the
//!   way the TypeScript BM25 path did — the instrument must report LEAK, name
//!   the offending request and response, and make the caller fail;
//! * a **leaking page path**: a page index that ignores the scope *and* a chunk
//!   fetch that ignores it too — the page row must report LEAK;
//! * a page index that ignores the scope while the chunk fetch still honours
//!   it — contained, and the test pins that down, because it is what the black
//!   box can and cannot see.
//!
//! The leaking runs are the ones that matter: an instrument that cannot fail is
//! not an instrument.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use kb_engine::EngineSettings;
use kb_engine::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeEntityStore, FakeJobStore,
    FakeReranker,
};
use kb_engine::{InMemoryIndex, PageArm, PageArmConfig};
use kb_eval::leak::{Arm, LeakMatrix, ScopeKind, SeedDocument, Status, plan};
use kb_eval::leak_http::{LeakConfig, canaries_for, probe};
use kb_ports::{
    ArmRanks, ArmScores, Candidate, Chunk, ChunkId, ChunkMetadata, ChunkToStore, ChunkType,
    DocumentId, DocumentMeta, DocumentScope, HybridQuery, IndexCapabilities, IndexUnit,
    PageEmbedder, PageImage, PortError, RetrievalIndex, SearchHit, TenantId,
};
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};

fn engine_settings() -> EngineSettings {
    EngineSettings {
        candidate_pool_size: 20,
        rerank_candidate_limit: 10,
        rerank_char_budget: 350,
        rerank_timeout_ms: 3000,
        rerank_final_k: 10,
        max_chunks: 10,
        min_similarity: 0.30,
        // Both weave and figures on, so those arms have something to return.
        neighbour_window: 1,
        figure_limit: 3,
        expansion_enabled: false,
        expansion_paraphrases: 3,
        hybrid_bm25_enabled: true,
        rerank_enabled: false,
    }
}

/// A document row the matrix can scope by knowledge base and category.
fn doc(id: &str, title: &str, kb: &str, category: &str) -> DocumentMeta {
    DocumentMeta {
        id: DocumentId(id.to_string()),
        title: title.to_string(),
        categories: vec![category.to_string()],
        subcategory: None,
        status: "ready".to_string(),
        chunk_count: 2,
        file_type: Some("text/markdown".to_string()),
        file_size: Some(1024),
        mime_type: Some("text/markdown".to_string()),
        s3_key: None,
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        knowledge_base_ids: vec![kb.to_string()],
        metadata: None,
    }
}

fn hit(id: &str, doc_id: &str, content: &str, chunk_index: u32, similarity: f32) -> SearchHit {
    SearchHit {
        chunk: Chunk {
            id: ChunkId(id.to_string()),
            document_id: DocumentId(doc_id.to_string()),
            document_title: doc_id.to_string(),
            content: content.to_string(),
            chunk_type: kb_ports::ChunkType::Text,
            metadata: ChunkMetadata {
                page: Some(0),
                chunk_index: Some(chunk_index),
                ..Default::default()
            },
            categories: vec![],
            subcategory: None,
        },
        similarity,
        rank: 0,
        is_bm25: false,
        retrieval: None,
    }
}

fn figure_hit(id: &str, doc_id: &str, caption: &str) -> SearchHit {
    let mut hit = hit(id, doc_id, caption, 2, 0.5);
    hit.chunk.metadata.asset_key = Some(format!("documents/{doc_id}/assets/1.png"));
    hit
}

/// The corpus both runs share: 2 tenants × 2 knowledge bases × 2 categories.
fn seed_documents() -> Vec<SeedDocument> {
    let mut out = Vec::new();
    let mut index = 0;
    for tenant in ["tenant-a", "tenant-b"] {
        for kb in 0..2 {
            for category in 0..2 {
                let id = format!("{tenant}-kb{kb}-cat{category}");
                out.push(SeedDocument {
                    id: id.clone(),
                    tenant: tenant.to_string(),
                    knowledge_base_id: format!("{tenant}-kb{kb}"),
                    category_id: format!("{tenant}-cat{category}"),
                    canaries: canaries_for(0xC0FFEE, index),
                    n_chunks: 2,
                    has_figures: true,
                    figure_asset_key: Some(format!("documents/{tenant}/{id}/assets/1.png")),
                });
                index += 1;
            }
        }
    }
    out
}

/// Which scope hole, if any, the served stores have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Every arm filters by scope.
    Honest,
    /// The text index's vector arm answers with everything it has.
    LeakingVector,
    /// The page index ignores the scope, and so does the chunk fetch that turns
    /// a page into the chunk on it: the page arm's whole path is open.
    LeakingPagePath,
    /// Only the page index ignores the scope; the chunk fetch is still scoped.
    LeakingPageIndexOnly,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Honest => "honest",
            Self::LeakingVector => "leaking-vector",
            Self::LeakingPagePath => "leaking-page-path",
            Self::LeakingPageIndexOnly => "leaking-page-index",
        }
    }
}

/// The page embedder the stack's stub stands in for, in process: every page
/// and every query get the same vector, so every page in the index is a
/// perfect match for every query and only the scope keeps a foreign one out.
struct SameVectorPageEmbedder;

#[async_trait]
impl PageEmbedder for SameVectorPageEmbedder {
    async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        Ok(pages.iter().map(|_| vec![1.0, 1.0, 1.0]).collect())
    }
    async fn embed_query(&self, _query: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![1.0, 1.0, 1.0])
    }
    fn dimension(&self) -> u32 {
        3
    }
}

/// Two pages per document. Page 0 carries the text hits the fake text index
/// already returns, so the page arm only *tags* them; page 1 carries no text
/// hit at all, so a page-1 match has to be turned into a chunk by
/// `RetrievalIndex::fetch` — the path a page-arm leak would take.
fn page_units(tenant: &str, id: &str) -> Vec<IndexUnit> {
    (0..2u32)
        .map(|page| IndexUnit {
            id: kb_engine::page_arm::page_unit_id(&DocumentId(id.to_string()), page),
            document_id: DocumentId(id.to_string()),
            tenant: TenantId(tenant.to_string()),
            knowledge_base_id: None,
            category: None,
            chunk_index: page,
            page: Some(page),
            chunk_type: ChunkType::Page,
            content: String::new(),
            contextual_prefix: None,
            metadata: serde_json::json!({ "chunkIds": [format!("{id}_{page}")], "page": page }),
            dense: vec![1.0, 1.0, 1.0],
            sparse: kb_core::sparse::SparseVector::default(),
        })
        .collect()
}

/// Seed the fakes and serve them, with the scope hole `mode` names.
async fn serve(mode: Mode) -> (String, BTreeMap<String, String>) {
    let documents = FakeDocumentStore::default();
    let chunks = FakeChunkStore::default();
    let blobs = FakeBlobStore::default();
    let jobs = FakeJobStore::default();
    let entities = FakeEntityStore::default();
    let embedder = FakeEmbedder::default();
    let events = EventBus::new();

    let pages = InMemoryIndex::new(3);
    let mut keys = BTreeMap::new();
    for (tenant, key) in [("tenant-a", "leak-key-a"), ("tenant-b", "leak-key-b")] {
        documents.insert_api_key(key, TenantId(tenant.to_string()));
        keys.insert(tenant.to_string(), key.to_string());
        for kb in 0..2 {
            for category in 0..2 {
                let id = format!("{tenant}-kb{kb}-cat{category}");
                let canaries = canaries_for(
                    0xC0FFEE,
                    seed_documents()
                        .iter()
                        .position(|d| d.id == id)
                        .unwrap_or(0),
                );
                documents.insert_doc(
                    TenantId(tenant.to_string()),
                    doc(
                        &id,
                        &id,
                        &format!("{tenant}-kb{kb}"),
                        &format!("{tenant}-cat{category}"),
                    ),
                );
                // One vector hit, one BM25 hit, one neighbour and one figure
                // per document: every arm has something it *could* return.
                chunks.vector_hits.write().unwrap().push(hit(
                    &format!("{id}-v"),
                    &id,
                    &canaries.vector,
                    0,
                    0.9,
                ));
                chunks.vector_hits.write().unwrap().push(hit(
                    &format!("{id}-v2"),
                    &id,
                    &canaries.neighbour,
                    1,
                    0.6,
                ));
                chunks.bm25_hits.write().unwrap().push(hit(
                    &format!("{id}-b"),
                    &id,
                    &canaries.bm25,
                    0,
                    0.8,
                ));
                chunks.neighbour_hits.write().unwrap().push(hit(
                    &format!("{id}-n"),
                    &id,
                    &canaries.neighbour,
                    1,
                    0.4,
                ));
                chunks.figure_hits.write().unwrap().push(figure_hit(
                    &format!("{id}-f"),
                    &id,
                    &canaries.figure,
                ));
                // The chunks the page arm's fetch reads back, one per page.
                chunks.stored_chunks.write().unwrap().insert(
                    DocumentId(id.clone()),
                    [&canaries.vector, &canaries.neighbour]
                        .iter()
                        .enumerate()
                        .map(|(i, content)| ChunkToStore {
                            content: (*content).clone(),
                            chunk_index: u32::try_from(i).unwrap(),
                            embedding: vec![0.0; 3],
                            metadata: serde_json::json!({ "page": i }),
                            contextual_prefix: None,
                        })
                        .collect(),
                );
                pages.upsert(page_units(tenant, &id)).await.unwrap();
            }
        }
    }

    let index: Arc<dyn RetrievalIndex> = match mode {
        Mode::Honest | Mode::LeakingPageIndexOnly => Arc::new(chunks.clone()),
        Mode::LeakingVector => Arc::new(LeakingChunkStore {
            inner: chunks.clone(),
            leak_search: true,
            leak_fetch: false,
        }),
        Mode::LeakingPagePath => Arc::new(LeakingChunkStore {
            inner: chunks.clone(),
            leak_search: false,
            leak_fetch: true,
        }),
    };
    // The vector-leak run keeps the page arm off, so it stays the run it always
    // was: with the arm on, in-scope pages outrank the leaked text hits and push
    // them below `maxChunks`, which would blunt the very leak it exists to show.
    let page_index: Option<Arc<dyn RetrievalIndex>> = match mode {
        Mode::Honest => Some(Arc::new(pages)),
        Mode::LeakingVector => None,
        Mode::LeakingPagePath | Mode::LeakingPageIndexOnly => {
            Some(Arc::new(UnscopedPageIndex { inner: pages }))
        }
    };

    let mut state = AppState::from_parts(
        StateParts {
            documents: Arc::new(documents),
            index,
            entities: Arc::new(entities),
            graph: None,
            graph_arm: None,
            blob: Arc::new(blobs),
            embedder: Arc::new(embedder),
            jobs: Arc::new(jobs),
            events: Arc::clone(&events),
            version: "leak-test".to_string(),
        },
        Arc::new(FakeReranker::default()),
        engine_settings(),
    );
    // `StateParts` has no page arm (it predates it); the engine reads it from
    // its inputs per request, so it is installed there.
    state.engine.inputs.page_arm = page_index.map(|index| {
        Arc::new(PageArm {
            embedder: Arc::new(SameVectorPageEmbedder),
            index,
            config: PageArmConfig::default(),
        })
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, kb_server::build_router(state)).await;
    });
    (format!("http://{addr}"), keys)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap()
}

async fn run_matrix(mode: Mode) -> (LeakMatrix, kb_eval::SeedPlan) {
    let (base_url, keys) = serve(mode).await;
    let plan = plan(seed_documents());
    let mut config = LeakConfig::new(&base_url, "leak-key-a");
    config.out_dir =
        std::env::temp_dir().join(format!("kb-leak-{}-{}", std::process::id(), mode.label()));
    config.max_chunks = 10;
    let matrix = probe(&client(), &config, &plan, &keys, LeakMatrix::new(&base_url)).await;
    (matrix, plan)
}

#[tokio::test]
async fn the_matrix_passes_against_a_server_that_scopes_every_arm() {
    let (matrix, plan) = run_matrix(Mode::Honest).await;

    assert!(
        !matrix.any_leak(),
        "an honest store must not leak: {matrix:?}"
    );
    assert_eq!(matrix.total_leaks(), 0);
    assert!(matrix.total_probes() > 0, "nothing was probed");

    // The positive control is what makes the above mean anything: every arm
    // must be able to find its own target from a scope that contains it.
    for arm in Arm::ALL {
        let report = matrix
            .arms
            .iter()
            .find(|a| a.arm == arm)
            .unwrap_or_else(|| panic!("{arm:?} was never probed"));
        // The graph arm is off in this server (`KB_GRAPH_ARM` unset), so it has
        // nothing to find. Every other arm must still prove it can find its target.
        if arm == Arm::Graph {
            assert_eq!(
                report.status,
                Status::Untested,
                "the graph arm is off here, so it must report UNTESTED, not {:?}",
                report.status
            );
            continue;
        }
        assert!(
            report.positive_hits > 0,
            "{arm:?} could not find its own target anywhere: {:?}",
            report.leak_examples
        );
        assert_ne!(
            report.status,
            Status::Leak,
            "{arm:?} leaked against an honest store: {:?}",
            report.leak_examples
        );
    }

    // Every scope kind was exercised, and none of them leaked.
    for kind in ScopeKind::ALL {
        let probes: usize = matrix
            .arms
            .iter()
            .flat_map(|a| a.cells.iter())
            .filter(|c| c.scope == kind)
            .map(|c| c.probes)
            .sum();
        assert!(probes > 0, "no probe for scope kind {kind:?}");
    }

    let md = kb_eval::leak::render_markdown(&matrix, &plan);
    assert!(md.contains("## Matrix (arm × scope type)"), "{md}");
    assert!(!md.contains("#### LEAK"), "{md}");
    assert!(md.contains("PASS"), "{md}");

    // A finding, not a failure: `POST /v1/search` with `documentIds` takes the
    // documentIds branch, which answers from the vector arm alone. The bm25 and
    // figure arms therefore cannot be exercised inside a documentIds scope, and
    // the instrument says so instead of passing the cell on a positive control
    // it never got. If hybrid retrieval is ever routed through that branch,
    // this assertion is what will change.
    for arm in [Arm::Bm25, Arm::Figure] {
        let report = matrix.arms.iter().find(|a| a.arm == arm).unwrap();
        let cell = report
            .cells
            .iter()
            .find(|c| c.scope == ScopeKind::DocumentIds)
            .unwrap();
        assert_eq!(cell.status, Status::Untested, "{arm:?} in documentIds");
        assert_eq!(cell.leaks, 0);
        assert!(
            cell.reason
                .as_deref()
                .is_some_and(|r| r.contains(if arm == Arm::Bm25 {
                    "runs no keyword arm"
                } else {
                    "positive control missed"
                })),
            "{arm:?}: {:?}",
            cell.reason
        );
    }
    // And the report has to name that hole rather than bury it.
    assert!(md.contains("`bm25` × `document_ids`"), "{md}");
    assert!(md.contains("`figure` × `document_ids`"), "{md}");

    // The page row: on, positive control hitting in every scope the page arm
    // can be reached from, and zero leaks. `documentIds` is answered by one
    // dense search that never runs the page arm, and the cell says so.
    let page = matrix.arms.iter().find(|a| a.arm == Arm::Page).unwrap();
    assert_eq!(page.leaks, 0, "{:?}", page.leak_examples);
    for kind in [
        ScopeKind::Tenant,
        ScopeKind::KnowledgeBase,
        ScopeKind::Category,
    ] {
        let cell = page.cells.iter().find(|c| c.scope == kind).unwrap();
        assert_eq!(cell.status, Status::Pass, "page × {kind:?}: {cell:?}");
        assert_eq!(cell.positive_hits, cell.positive_probes, "page × {kind:?}");
        assert!(
            cell.probes > cell.positive_probes,
            "page × {kind:?} had no leak probe"
        );
    }
    let by_ids = page
        .cells
        .iter()
        .find(|c| c.scope == ScopeKind::DocumentIds)
        .unwrap();
    assert_eq!(by_ids.status, Status::Untested);
    assert!(
        by_ids
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("never reaches the page-image arm")),
        "{:?}",
        by_ids.reason
    );
    assert!(md.contains("`page` × `document_ids`"), "{md}");
}

/// The page row must be able to fail. A page index that ignores the scope, with
/// a chunk fetch that ignores it too, is the page arm's whole path open: every
/// page of the other tenant matches the query perfectly and its chunk comes back.
#[tokio::test]
async fn a_page_arm_that_ignores_scope_is_reported_as_a_leak_in_the_page_row() {
    let (matrix, plan) = run_matrix(Mode::LeakingPagePath).await;
    assert!(matrix.any_leak(), "{matrix:?}");
    let page = matrix.arms.iter().find(|a| a.arm == Arm::Page).unwrap();
    assert_eq!(page.status, Status::Leak, "{page:?}");
    let example = page.leak_examples.first().expect("evidence");
    assert_eq!(example.arm, Arm::Page);
    let scope = plan
        .scopes
        .iter()
        .find(|s| s.label == example.scope)
        .unwrap();
    for leaked in &example.foreign_document_ids {
        assert!(!scope.allowed.contains(leaked), "{leaked} is in scope");
    }
    // The vector arm is honest in this run, so nothing but the page path leaked
    // through a ranked arm: the row that went red is the row that should.
    let vector = matrix.arms.iter().find(|a| a.arm == Arm::Vector).unwrap();
    assert_eq!(vector.leaks, 0, "{:?}", vector.leak_examples);
    let md = kb_eval::leak::render_markdown(&matrix, &plan);
    assert!(md.contains("#### LEAK: page"), "{md}");
}

/// What the black box can and cannot see. A page index that ignores the scope
/// on its own is *contained*: a page names chunk ids, and the chunk behind a
/// page is read back through the scoped `fetch`, so a foreign page brings
/// nothing into the answer. The matrix therefore cannot see a hole in the page
/// index alone — the engine-level test
/// (`kb-engine/src/page_arm_tests.rs`) is what watches that layer. If this
/// ever reports a leak, the fetch stopped being a second fence.
#[tokio::test]
async fn a_page_index_hole_alone_is_contained_by_the_scoped_chunk_fetch() {
    let (matrix, _plan) = run_matrix(Mode::LeakingPageIndexOnly).await;
    let page = matrix.arms.iter().find(|a| a.arm == Arm::Page).unwrap();
    assert_eq!(page.leaks, 0, "{:?}", page.leak_examples);
    assert!(!matrix.any_leak(), "{matrix:?}");
}

#[tokio::test]
async fn a_deliberately_leaking_store_is_reported_as_a_leak() {
    // The instrument must be able to fail. A store whose vector arm ignores
    // the scope is exactly the Bun BM25 bug this matrix exists to catch.
    let (matrix, plan) = run_matrix(Mode::LeakingVector).await;

    assert!(
        matrix.any_leak(),
        "a leaking store must be reported as a leak: {matrix:?}"
    );
    assert!(matrix.total_leaks() > 0);
    let vector = matrix
        .arms
        .iter()
        .find(|a| a.arm == Arm::Vector)
        .expect("the vector arm was probed");
    assert_eq!(vector.status, Status::Leak);
    assert!(vector.leaks > 0);

    // The evidence has to survive: which scope, which query, which foreign
    // document, and the request/response that produced it.
    let example = vector
        .leak_examples
        .first()
        .expect("a leak with no evidence is not a report");
    assert_eq!(example.arm, Arm::Vector);
    assert!(!example.scope.is_empty());
    assert!(!example.query.is_empty());
    assert!(!example.foreign_document_ids.is_empty());
    // The foreign document really is outside the scope it was found in.
    let leaked = &example.foreign_document_ids[0];
    let owner = plan
        .documents
        .iter()
        .find(|d| &d.id == leaked)
        .expect("the leaked document was seeded by this test");
    let scope = plan
        .scopes
        .iter()
        .find(|s| s.label == example.scope)
        .expect("the leaking scope");
    assert!(
        !scope.allowed.contains(leaked),
        "the example is not actually a cross-scope hit"
    );
    assert_ne!(scope.tenant, owner.tenant, "expected a cross-tenant hit");

    // The rendered report names the leak, with its request and response.
    let md = kb_eval::leak::render_markdown(&matrix, &plan);
    assert!(md.contains("#### LEAK: vector"), "{md}");
    assert!(md.contains(leaked.as_str()), "{md}");
    assert!(md.contains("foreign documents returned"), "{md}");
}

/// A chunk store that reproduces the bug the matrix is built to catch: the
/// vector arm (`leak_search`) answers with every chunk it has, or the id fetch
/// (`leak_fetch`) reads any document's chunk, whatever the scope says.
struct LeakingChunkStore {
    inner: FakeChunkStore,
    leak_search: bool,
    leak_fetch: bool,
}

#[async_trait]
impl RetrievalIndex for LeakingChunkStore {
    async fn upsert(&self, units: Vec<IndexUnit>) -> Result<(), PortError> {
        self.inner.upsert(units).await
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<u64, PortError> {
        self.inner.delete_document(tenant, document_id).await
    }

    async fn search(
        &self,
        query: &HybridQuery,
        scope: &DocumentScope,
    ) -> Result<Vec<Candidate>, PortError> {
        if !self.leak_search {
            return self.inner.search(query, scope).await;
        }
        // The leak: ignores scope and returns all vector hits!
        let list = self.inner.vector_hits.read().unwrap();
        let mut out = Vec::new();
        for h in list.iter() {
            out.push(Candidate {
                unit_id: h.chunk.id.clone(),
                document_id: h.chunk.document_id.clone(),
                tenant: TenantId("fake".into()),
                chunk_index: h.chunk.metadata.chunk_index.unwrap_or(0),
                page: h.chunk.metadata.page,
                chunk_type: h.chunk.chunk_type,
                content: h.chunk.content.clone(),
                contextual_prefix: h.chunk.metadata.contextual_prefix.clone(),
                metadata: serde_json::json!({
                    "section": h.chunk.metadata.section,
                    "assetKey": h.chunk.metadata.asset_key,
                    "anchorChunkIndex": h.chunk.metadata.anchor_chunk_index,
                    "chunkIndex": h.chunk.metadata.chunk_index,
                    "page": h.chunk.metadata.page,
                }),
                scores: ArmScores {
                    dense: Some(h.similarity),
                    sparse: None,
                    fused: h.similarity,
                },
                ranks: ArmRanks {
                    dense: Some(h.rank),
                    sparse: None,
                    fused: h.rank,
                },
            });
        }
        out.truncate(query.k);
        Ok(out)
    }

    async fn fetch(
        &self,
        scope: &DocumentScope,
        ids: &[ChunkId],
    ) -> Result<Vec<Candidate>, PortError> {
        if !self.leak_fetch {
            return self.inner.fetch(scope, ids).await;
        }
        // The leak: any document's chunk, by id, scope or no scope.
        let stored = self.inner.stored_chunks.read().unwrap();
        let mut out = Vec::new();
        for (document_id, chunks) in stored.iter() {
            for chunk in chunks {
                let unit_id = format!("{}_{}", document_id.0, chunk.chunk_index);
                if !ids.iter().any(|id| id.0 == unit_id) {
                    continue;
                }
                out.push(Candidate {
                    unit_id: ChunkId(unit_id),
                    document_id: document_id.clone(),
                    tenant: scope.tenant().clone(),
                    chunk_index: chunk.chunk_index,
                    page: None,
                    chunk_type: ChunkType::Text,
                    content: chunk.content.clone(),
                    contextual_prefix: None,
                    metadata: chunk.metadata.clone(),
                    scores: ArmScores::default(),
                    ranks: ArmRanks::default(),
                });
            }
        }
        Ok(out)
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        document_id: &DocumentId,
        chunk_indices: &[u32],
    ) -> Result<Vec<Candidate>, PortError> {
        self.inner
            .neighbours(scope, document_id, chunk_indices)
            .await
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        document_ids: &[DocumentId],
    ) -> Result<Vec<Candidate>, PortError> {
        self.inner.figures(scope, document_ids).await
    }

    async fn ordered_chunks(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<Vec<Candidate>, PortError> {
        self.inner.ordered_chunks(tenant, document_id).await
    }

    async fn health_check(&self) -> Result<(), PortError> {
        self.inner.health_check().await
    }

    fn capabilities(&self) -> IndexCapabilities {
        self.inner.capabilities()
    }
}

/// A page index that answers with every page it holds, whoever owns it.
struct UnscopedPageIndex {
    inner: InMemoryIndex,
}

#[async_trait]
impl RetrievalIndex for UnscopedPageIndex {
    async fn upsert(&self, units: Vec<IndexUnit>) -> Result<(), PortError> {
        self.inner.upsert(units).await
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<u64, PortError> {
        self.inner.delete_document(tenant, document_id).await
    }

    async fn search(
        &self,
        query: &HybridQuery,
        _scope: &DocumentScope,
    ) -> Result<Vec<Candidate>, PortError> {
        // The leak: every tenant's pages, read back through each owner's own
        // full ordering, so nothing about the caller's scope is consulted.
        let mut out = Vec::new();
        for tenant in ["tenant-a", "tenant-b"] {
            for kb in 0..2 {
                for category in 0..2 {
                    let id = DocumentId(format!("{tenant}-kb{kb}-cat{category}"));
                    out.extend(
                        self.inner
                            .ordered_chunks(&TenantId(tenant.to_string()), &id)
                            .await?
                            .into_iter()
                            .map(|mut c| {
                                c.scores = ArmScores {
                                    dense: Some(1.0),
                                    sparse: None,
                                    fused: 1.0,
                                };
                                c
                            }),
                    );
                }
            }
        }
        out.truncate(query.k);
        Ok(out)
    }

    async fn fetch(
        &self,
        scope: &DocumentScope,
        ids: &[ChunkId],
    ) -> Result<Vec<Candidate>, PortError> {
        self.inner.fetch(scope, ids).await
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        document_id: &DocumentId,
        chunk_indices: &[u32],
    ) -> Result<Vec<Candidate>, PortError> {
        self.inner
            .neighbours(scope, document_id, chunk_indices)
            .await
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        document_ids: &[DocumentId],
    ) -> Result<Vec<Candidate>, PortError> {
        self.inner.figures(scope, document_ids).await
    }

    async fn ordered_chunks(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<Vec<Candidate>, PortError> {
        self.inner.ordered_chunks(tenant, document_id).await
    }

    async fn health_check(&self) -> Result<(), PortError> {
        self.inner.health_check().await
    }

    fn capabilities(&self) -> IndexCapabilities {
        self.inner.capabilities()
    }
}

/// The `--pdf` corpus is only worth seeding if pdfium reads it back the way the
/// ingest will: two pages, each canary in the text layer of its page, and both
/// pages renderable for the page arm. Skipped (and says so) without libpdfium.
#[tokio::test]
async fn the_pdf_corpus_reads_back_with_every_canary_and_renders_two_pages() {
    use kb_ports::PageRenderer;
    let Ok(pdfium) = kb_extract::pdfium::get_pdfium() else {
        eprintln!("skipping: libpdfium not loaded (rust/scripts/fetch-pdfium.sh)");
        return;
    };
    let canaries = canaries_for(0xC0FFEE, 3);
    let pdf = kb_eval::leak_http::document_pdf(3, &canaries, 0xC0FFEE);
    let text = kb_extract::pdfium::extract_pdf_text_layer(pdfium, &pdf).unwrap();
    let pages = text.page_map.expect("a page map");
    assert_eq!(pages.len(), 2, "{pages:?}");
    // Line wrapping turns some spaces into newlines; compare on words.
    let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = words(&pages[0].text);
    let second = words(&pages[1].text);
    assert!(first.contains(&words(&canaries.vector)), "{first}");
    assert!(first.contains(&words(&canaries.figure)), "{first}");
    assert!(first.contains(&canaries.bm25), "{first}");
    assert!(second.contains(&words(&canaries.neighbour)), "{second}");
    assert!(second.contains(&words(&canaries.entity)), "{second}");

    let rendered = kb_extract::PdfiumPageRenderer {
        dpi: 36,
        jpeg_quality: 60,
    }
    .render("application/pdf", &pdf, 10)
    .await
    .unwrap();
    assert_eq!(rendered.len(), 2);
    assert!(rendered.iter().all(|p| !p.bytes.is_empty()));
}
