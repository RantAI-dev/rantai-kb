//! The leak matrix, run in process against the real router and the fakes.
//!
//! The instrument is a black-box HTTP client, so the only honest way to test it
//! is to serve the real router on a loopback port and point the instrument at
//! it: real HTTP, real auth, real JSON, real scope resolution, real retrieval
//! arms. Nothing about the retrieval code is stubbed from the instrument's side.
//!
//! Two runs:
//!
//! * the **honest** store, where every arm filters by scope — the matrix must
//!   come back with positive controls that hit and no leaks;
//! * a **deliberately leaking** store, whose vector arm ignores the scope the
//!   way the TypeScript BM25 path did — the instrument must report LEAK, name
//!   the offending request and response, and make the caller fail.
//!
//! The second test is the one that matters: an instrument that cannot fail is
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
use kb_eval::leak::{Arm, LeakMatrix, ScopeKind, SeedDocument, Status, plan};
use kb_eval::leak_http::{LeakConfig, canaries_for, probe};
use kb_ports::{
    Chunk, ChunkId, ChunkMetadata, ChunkStore, ChunkToStore, DocumentId, DocumentMeta,
    DocumentScope, NeighbourAnchor, PortError, SearchHit, StoredChunk, TenantId,
};
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};

fn engine_settings() -> EngineSettings {
    EngineSettings {
        candidate_pool_size: 20,
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

/// Seed the fakes and serve them. `leaking` swaps in a chunk store whose vector
/// arm answers with everything it has, scope or no scope.
async fn serve(leaking: bool) -> (String, BTreeMap<String, String>) {
    let documents = FakeDocumentStore::default();
    let chunks = FakeChunkStore::default();
    let blobs = FakeBlobStore::default();
    let jobs = FakeJobStore::default();
    let entities = FakeEntityStore::default();
    let embedder = FakeEmbedder::default();
    let events = EventBus::new();

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
            }
        }
    }

    let chunk_store: Arc<dyn ChunkStore> = if leaking {
        Arc::new(LeakingChunkStore {
            inner: chunks.clone(),
        })
    } else {
        Arc::new(chunks.clone())
    };

    let state = AppState::from_parts(
        StateParts {
            documents: Arc::new(documents),
            chunks: chunk_store,
            entities: Arc::new(entities),
            blob: Arc::new(blobs),
            embedder: Arc::new(embedder),
            jobs: Arc::new(jobs),
            events: Arc::clone(&events),
            version: "leak-test".to_string(),
        },
        Arc::new(FakeReranker::default()),
        engine_settings(),
    );

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

async fn run_matrix(leaking: bool) -> (LeakMatrix, kb_eval::SeedPlan) {
    let (base_url, keys) = serve(leaking).await;
    let plan = plan(seed_documents());
    let mut config = LeakConfig::new(&base_url, "leak-key-a");
    config.out_dir = std::env::temp_dir().join(format!(
        "kb-leak-{}-{}",
        std::process::id(),
        if leaking { "leaking" } else { "honest" }
    ));
    config.max_chunks = 10;
    let matrix = probe(&client(), &config, &plan, &keys, LeakMatrix::new(&base_url)).await;
    (matrix, plan)
}

#[tokio::test]
async fn the_matrix_passes_against_a_server_that_scopes_every_arm() {
    let (matrix, plan) = run_matrix(false).await;

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
                .is_some_and(|r| r.contains("positive control missed")),
            "{arm:?}: {:?}",
            cell.reason
        );
    }
    // And the report has to name that hole rather than bury it.
    assert!(md.contains("`bm25` × `document_ids`"), "{md}");
    assert!(md.contains("`figure` × `document_ids`"), "{md}");
}

#[tokio::test]
async fn a_deliberately_leaking_store_is_reported_as_a_leak() {
    // The instrument must be able to fail. A store whose vector arm ignores
    // the scope is exactly the Bun BM25 bug this matrix exists to catch.
    let (matrix, plan) = run_matrix(true).await;

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
/// vector arm answers with every chunk it has, whatever the scope says.
struct LeakingChunkStore {
    inner: FakeChunkStore,
}

#[async_trait]
impl ChunkStore for LeakingChunkStore {
    async fn delete_by_document(&self, document_id: &DocumentId) -> Result<u64, PortError> {
        self.inner.delete_by_document(document_id).await
    }

    async fn store_chunks(
        &self,
        document_id: &DocumentId,
        chunks: &[ChunkToStore],
        embedding_model: Option<&str>,
    ) -> Result<(), PortError> {
        self.inner
            .store_chunks(document_id, chunks, embedding_model)
            .await
    }

    async fn chunk_counts(&self, ids: &[DocumentId]) -> Result<Vec<(DocumentId, u32)>, PortError> {
        self.inner.chunk_counts(ids).await
    }

    async fn ordered_chunks(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<StoredChunk>, PortError> {
        self.inner.ordered_chunks(document_id).await
    }

    /// The leak: no scope filter at all.
    async fn vector_search(
        &self,
        _scope: &DocumentScope,
        _embedding: &[f32],
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        let list = self.inner.vector_hits.read().unwrap();
        Ok(list.iter().take(limit as usize).cloned().collect())
    }

    async fn bm25_search(
        &self,
        scope: &DocumentScope,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        self.inner.bm25_search(scope, query, limit).await
    }

    async fn bm25_search_global(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        self.inner.bm25_search_global(query, limit).await
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        anchors: &[NeighbourAnchor],
        window: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        self.inner.neighbours(scope, anchors, window).await
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        query: &str,
        retrieved_text: &str,
        doc_ids: &[DocumentId],
        anchored_chunks: &[(DocumentId, u32)],
        limit: u32,
    ) -> Result<Vec<SearchHit>, PortError> {
        self.inner
            .figures(
                scope,
                query,
                retrieved_text,
                doc_ids,
                anchored_chunks,
                limit,
            )
            .await
    }
}
