//! The graph arm, through the whole pipeline.
//!
//! Two things are being tested and they are not the same thing.
//!
//! **Does it help.** A chunk the dense and keyword arms missed reaches the answer,
//! carrying `graph` provenance and a rank inside the graph arm's own list. The
//! fusion has to be able to promote a third list, not just annotate the first two.
//!
//! **Does it stay in scope.** AGENTS rule 5: every retrieval arm is filtered to the
//! caller's visible documents, and the graph arm has two ways to leak that the
//! others do not. It can *return* a chunk from a document outside the scope, and it
//! can *walk through* one — two visible entities connected only by a relation
//! extracted from an invisible document. The second is the expensive one, and it is
//! why `GraphStore::load_tenant_graph` filters edges rather than results. Both are
//! tested here against the reference `InMemoryGraphStore`, which passes the port
//! contract's `no_path_through_an_out_of_scope_document` property.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use kb_ports::{
    ChunkId, ChunkType, DocumentId, DocumentMeta, DocumentScope, DocumentStore, Embedder,
    EntityUpsert, GraphStore, IndexUnit, ProvenanceArm, RelationUpsert, Reranker, RetrievalDegrade,
    RetrievalIndex, TenantId,
};

use crate::fakes::{
    FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_scope,
};
use crate::graph_arm::{GraphArm, GraphArmConfig};
use crate::in_memory_graph_store::InMemoryGraphStore;
use crate::retrieval::{RetrievalOptions, execute_pipeline};
use crate::timing::SystemClock;

const TENANT: &str = "tenant-a";

fn doc(id: &str) -> DocumentMeta {
    DocumentMeta {
        id: DocumentId::new_unchecked(id),
        title: format!("Title {id}"),
        categories: vec![],
        subcategory: None,
        status: "ready".to_string(),
        external_ref: None,
        chunk_count: 1,
        file_type: None,
        file_size: None,
        mime_type: None,
        s3_key: None,
        created_at: String::new(),
        updated_at: String::new(),
        knowledge_base_ids: vec![],
        metadata: None,
    }
}

fn tenant() -> TenantId {
    TenantId::new_unchecked(TENANT)
}

fn noop_expander() -> Arc<dyn crate::QueryExpander> {
    Arc::new(crate::NoopExpander)
}

async fn seed_chunks(store: &FakeChunkStore, document: &str, chunks: &[&str]) {
    let id = DocumentId::new_unchecked(document);
    let units: Vec<IndexUnit> = chunks
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let content = (*text).to_string();
            IndexUnit {
                id: ChunkId(format!("{document}_{i}")),
                document_id: id.clone(),
                tenant: tenant(),
                knowledge_base_id: None,
                category: None,
                chunk_index: i as u32,
                page: None,
                chunk_type: ChunkType::Text,
                sparse: kb_core::sparse::encode_document(&content),
                content,
                contextual_prefix: None,
                metadata: serde_json::json!({}),
                dense: vec![0.0; 4],
            }
        })
        .collect();
    store
        .upsert(units)
        .await
        .expect("seeding a fake cannot fail");
}

/// A graph holding `entities` and the relations between them, attributed to
/// `documents` — the document each relation was extracted from, which is what
/// makes it scopable.
/// The document a chunk id belongs to, which is the document that *owns* a
/// mention — the scope rule is applied to that, not to the id's text.
fn document_of(chunk_id: &str) -> &str {
    chunk_id
        .rsplit_once('_')
        .map_or(chunk_id, |(document, _)| document)
}

async fn seed_graph(
    store: &InMemoryGraphStore,
    entities: &[(&str, &str, &[&str])],
    relations: &[(&str, &str, &str, &str)],
) {
    for (key, name, chunk_ids) in entities {
        // One `upsert_entities` call per document, because a mention belongs to
        // the document that owns it and a scope is applied to ownership.
        let mut by_document: std::collections::BTreeMap<&str, Vec<ChunkId>> = Default::default();
        for chunk_id in *chunk_ids {
            by_document
                .entry(document_of(chunk_id))
                .or_default()
                .push(ChunkId((*chunk_id).to_string()));
        }
        for (document, mentions) in by_document {
            store
                .upsert_entities(
                    &tenant(),
                    &DocumentId::new_unchecked(document),
                    vec![EntityUpsert {
                        key: (*key).to_string(),
                        name: (*name).to_string(),
                        entity_type: "ProperNoun".to_string(),
                        aliases: Vec::new(),
                        mentions,
                    }],
                )
                .await
                .expect("seed entities");
        }
    }
    for (src, dst, document, chunk) in relations {
        store
            .relate(
                &tenant(),
                &DocumentId::new_unchecked(*document),
                vec![RelationUpsert {
                    src_key: (*src).to_string(),
                    dst_key: (*dst).to_string(),
                    relation_type: "CO_OCCURS".to_string(),
                    weight: 1.0,
                    chunk_id: Some(ChunkId((*chunk).to_string())),
                }],
            )
            .await
            .expect("seed relations");
    }
}

struct Harness {
    documents: Arc<FakeDocumentStore>,
    chunks: Arc<FakeChunkStore>,
    embedder: Arc<FakeEmbedder>,
    reranker: Arc<FakeReranker>,
    graph: Arc<InMemoryGraphStore>,
}

fn harness(documents: &[&str]) -> Harness {
    let doc_store = Arc::new(FakeDocumentStore::default());
    for id in documents {
        doc_store.insert_doc(tenant(), doc(id));
    }
    Harness {
        documents: doc_store,
        chunks: Arc::new(FakeChunkStore::default()),
        embedder: Arc::new(FakeEmbedder::default()),
        reranker: Arc::new(FakeReranker::default()),
        graph: Arc::new(InMemoryGraphStore::new()),
    }
}

async fn run(
    h: &Harness,
    scope: &DocumentScope,
    query: &str,
    graph_arm: Option<Arc<GraphArm>>,
) -> crate::RetrievalOutcome {
    let opts = RetrievalOptions {
        max_chunks: 10,
        candidate_pool_size: 10,
        figure_limit: 0,
        neighbour_window: 0,
        graph_arm,
        ..RetrievalOptions::default()
    };
    execute_pipeline(
        &(h.documents.clone() as Arc<dyn DocumentStore>),
        &(h.chunks.clone() as Arc<dyn RetrievalIndex>),
        &(h.embedder.clone() as Arc<dyn Embedder>),
        &(h.reranker.clone() as Arc<dyn Reranker>),
        &noop_expander(),
        scope,
        query,
        &opts,
        &SystemClock,
    )
    .await
    .expect("the pipeline runs")
}

fn arm(h: &Harness) -> Arc<GraphArm> {
    let store: Arc<dyn GraphStore> = h.graph.clone();
    Arc::new(GraphArm::new(store, GraphArmConfig::default()))
}

/// The arm contributes a chunk neither other arm found, and says so.
#[tokio::test]
async fn a_chunk_only_the_graph_found_reaches_the_answer_with_graph_provenance() {
    let h = harness(&["doc-visible"]);
    seed_chunks(
        &h.chunks,
        "doc-visible",
        &["Alpha Corporation published the report."],
    )
    .await;
    // The entity is mentioned in a document the caller cannot see as well, so the
    // walk has to choose between two mentions and the only defensible one is the
    // visible one. Neither dense nor keyword finds anything here: this chunk
    // arrives purely through the graph.
    seed_graph(
        &h.graph,
        &[
            (
                "alpha corporation",
                "Alpha Corporation",
                &["doc-visible_0", "doc-hidden_0"],
            ),
            ("beta foundation", "Beta Foundation", &["doc-visible_0"]),
        ],
        &[(
            "alpha corporation",
            "beta foundation",
            "doc-visible",
            "doc-visible_0",
        )],
    )
    .await;

    let scope = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-visible")]);
    let outcome = run(&h, &scope, "Alpha Corporation", Some(arm(&h))).await;

    assert_eq!(outcome.chunks.len(), 1, "{:?}", outcome.chunks);
    let hit = &outcome.chunks[0];
    assert_eq!(hit.chunk.id.0, "doc-visible_0");
    let provenance = hit
        .retrieval
        .as_ref()
        .expect("provenance is always reported");
    assert!(
        provenance.has(ProvenanceArm::Graph),
        "the graph arm must be named: {provenance:?}"
    );
    assert_eq!(
        provenance.graph_rank,
        Some(0),
        "the chunk the walk ranked first is graph rank 0"
    );
}

/// The rule that is easy to get wrong: filtering results is not enough.
#[tokio::test]
async fn a_chunk_from_an_out_of_scope_document_never_appears_through_the_graph_arm() {
    let h = harness(&["doc-visible", "doc-hidden"]);
    seed_chunks(
        &h.chunks,
        "doc-visible",
        &["Alpha Corporation published the report."],
    )
    .await;
    seed_chunks(
        &h.chunks,
        "doc-hidden",
        &["Alpha Corporation and Beta Foundation signed the pact."],
    )
    .await;
    // The relation is extracted from `doc-hidden`, and its *most* mentioned chunk
    // is the hidden one — so a graph that filtered results but not edges would put
    // `doc-hidden_0` at the top of the walk.
    seed_graph(
        &h.graph,
        &[
            (
                "alpha corporation",
                "Alpha Corporation",
                &["doc-visible_0", "doc-hidden_0"],
            ),
            // Mentioned only in the hidden document. If the scope filter were on
            // results rather than edges, the walk would still reach it through the
            // relation below and put its chunk first.
            ("beta foundation", "Beta Foundation", &["doc-hidden_0"]),
        ],
        &[(
            "alpha corporation",
            "beta foundation",
            "doc-hidden",
            "doc-hidden_0",
        )],
    )
    .await;

    let scope = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-visible")]);
    let outcome = run(&h, &scope, "Alpha Corporation", Some(arm(&h))).await;

    let ids: Vec<&str> = outcome
        .chunks
        .iter()
        .map(|hit| hit.chunk.id.0.as_str())
        .collect();
    assert!(
        ids.iter().all(|id| id.starts_with("doc-visible_")),
        "an out-of-scope chunk reached the answer: {ids:?}"
    );
    let documents: Vec<&str> = outcome
        .chunks
        .iter()
        .map(|hit| hit.chunk.document_id.0.as_str())
        .collect();
    assert!(
        !documents.contains(&"doc-hidden"),
        "an out-of-scope document reached the answer: {documents:?}"
    );
    // And the walk genuinely had something to say: this is not a test that passes
    // because the arm returned nothing at all.
    assert!(
        outcome.chunks.iter().any(|hit| hit
            .retrieval
            .as_ref()
            .is_some_and(|p| p.has(ProvenanceArm::Graph))),
        "the graph arm should have contributed: {:?}",
        outcome.chunks
    );
}

/// The expensive one: both endpoints are visible, and the only thing connecting
/// them is a relation from a document the caller cannot see. If the scope filter
/// were on results rather than edges, the walk would route through that document
/// and the caller would learn the shape of a document they were never shown.
#[tokio::test]
async fn a_path_through_an_out_of_scope_document_does_not_become_a_path() {
    let h = harness(&["doc-visible", "doc-secret"]);
    seed_chunks(
        &h.chunks,
        "doc-visible",
        &[
            "Alpha Corporation is described here.",
            "Beta Foundation is described here.",
        ],
    )
    .await;
    seed_chunks(
        &h.chunks,
        "doc-secret",
        &["This document is not visible to the caller."],
    )
    .await;
    // Two visible entities whose every connecting relation came from
    // `doc-secret`. `Gamma Holdings` exists only in the secret document.
    seed_graph(
        &h.graph,
        &[
            ("alpha corporation", "Alpha Corporation", &["doc-visible_0"]),
            ("beta foundation", "Beta Foundation", &["doc-visible_1"]),
            // Visible *and* secret: the query names it, so it seeds the walk, and
            // the only routes from it to the other two run through `doc-secret`.
            (
                "gamma holdings",
                "Gamma Holdings",
                &["doc-visible_0", "doc-secret_0"],
            ),
        ],
        &[
            (
                "alpha corporation",
                "gamma holdings",
                "doc-secret",
                "doc-secret_0",
            ),
            (
                "gamma holdings",
                "beta foundation",
                "doc-secret",
                "doc-secret_0",
            ),
        ],
    )
    .await;

    let scope = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-visible")]);
    let outcome = run(&h, &scope, "Gamma Holdings", Some(arm(&h))).await;

    let ids: Vec<&str> = outcome
        .chunks
        .iter()
        .map(|hit| hit.chunk.id.0.as_str())
        .collect();
    assert!(
        !ids.iter().any(|id| id.starts_with("doc-secret")),
        "the walk routed through an invisible document: {ids:?}"
    );
}

/// A store that is down must cost the caller its graph arm, not its search.
#[tokio::test]
async fn a_failing_graph_store_degrades_visibly_and_still_returns_the_other_arms() {
    struct BrokenGraph;
    #[async_trait::async_trait]
    impl GraphStore for BrokenGraph {
        async fn upsert_entities(
            &self,
            _: &TenantId,
            _: &DocumentId,
            _: Vec<EntityUpsert>,
        ) -> Result<(), kb_ports::PortError> {
            Err(kb_ports::PortError::Unavailable("down".into()))
        }
        async fn relate(
            &self,
            _: &TenantId,
            _: &DocumentId,
            _: Vec<RelationUpsert>,
        ) -> Result<(), kb_ports::PortError> {
            Err(kb_ports::PortError::Unavailable("down".into()))
        }
        async fn delete_document(
            &self,
            _: &TenantId,
            _: &DocumentId,
        ) -> Result<(), kb_ports::PortError> {
            Err(kb_ports::PortError::Unavailable("down".into()))
        }
        async fn load_tenant_graph(
            &self,
            _: &DocumentScope,
        ) -> Result<kb_ports::TenantGraph, kb_ports::PortError> {
            Err(kb_ports::PortError::Unavailable("down".into()))
        }
        async fn graph_version(&self, _: &TenantId) -> Result<u64, kb_ports::PortError> {
            Err(kb_ports::PortError::Unavailable("down".into()))
        }
        async fn match_entities(
            &self,
            _: &DocumentScope,
            _: &[String],
        ) -> Result<Vec<(String, f32)>, kb_ports::PortError> {
            Err(kb_ports::PortError::Unavailable("down".into()))
        }
        async fn entity_mentions(
            &self,
            _: &DocumentScope,
            _: &str,
        ) -> Result<Vec<ChunkId>, kb_ports::PortError> {
            Err(kb_ports::PortError::Unavailable("down".into()))
        }
    }

    let h = harness(&["doc-visible"]);
    seed_chunks(&h.chunks, "doc-visible", &["Alpha Corporation published."]).await;
    h.chunks
        .vector_hits
        .write()
        .unwrap()
        .push(crate::fakes::make_fake_hit(
            "doc-visible_0",
            "doc-visible",
            "Title",
            "Content",
            1,
            0.9,
        ));

    let scope = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-visible")]);
    let broken: Arc<dyn GraphStore> = Arc::new(BrokenGraph);
    let arm = Arc::new(GraphArm::new(broken, GraphArmConfig::default()));
    let outcome = run(&h, &scope, "Alpha Corporation", Some(arm)).await;

    assert!(
        outcome
            .degrades
            .contains(&RetrievalDegrade::GraphArmUnavailable),
        "a failed arm must say so: {:?}",
        outcome.degrades
    );
    assert!(
        !outcome.chunks.is_empty(),
        "the other arms' results must survive a broken graph"
    );
    assert!(
        outcome.timings.graph_arm_ms > 0.0,
        "the arm ran and cost time"
    );
}

/// With the arm off, the pipeline must be the pipeline it was.
#[tokio::test]
async fn the_arm_off_pipeline_is_unchanged_and_reports_zero_graph_time() {
    let h = harness(&["doc-visible"]);
    seed_chunks(
        &h.chunks,
        "doc-visible",
        &["Alpha Corporation published the report."],
    )
    .await;
    h.chunks
        .vector_hits
        .write()
        .unwrap()
        .push(crate::fakes::make_fake_hit(
            "doc-visible_0",
            "doc-visible",
            "Title",
            "Content",
            1,
            0.9,
        ));
    seed_graph(
        &h.graph,
        &[("alpha corporation", "Alpha Corporation", &["doc-visible_0"])],
        &[],
    )
    .await;

    let scope = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-visible")]);
    let outcome = run(&h, &scope, "Alpha Corporation", None).await;

    assert_eq!(outcome.timings.graph_arm_ms, 0.0);
    assert!(outcome.degrades.is_empty(), "{:?}", outcome.degrades);
    for hit in &outcome.chunks {
        let provenance = hit.retrieval.as_ref().expect("provenance");
        assert!(!provenance.has(ProvenanceArm::Graph));
        assert_eq!(provenance.graph_rank, None);
    }
}

/// An arm with nothing to say is not a degraded arm.
#[tokio::test]
async fn a_query_no_entity_matches_is_not_a_degrade() {
    let h = harness(&["doc-visible"]);
    seed_chunks(&h.chunks, "doc-visible", &["Alpha Corporation published."]).await;
    h.chunks
        .vector_hits
        .write()
        .unwrap()
        .push(crate::fakes::make_fake_hit(
            "doc-visible_0",
            "doc-visible",
            "Title",
            "Content",
            1,
            0.9,
        ));
    seed_graph(
        &h.graph,
        &[("alpha corporation", "Alpha Corporation", &["doc-visible_0"])],
        &[],
    )
    .await;

    let scope = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-visible")]);
    let outcome = run(&h, &scope, "photosynthesis in C4 grasses", Some(arm(&h))).await;

    assert!(
        !outcome
            .degrades
            .contains(&RetrievalDegrade::GraphArmUnavailable),
        "no match is not a failure: {:?}",
        outcome.degrades
    );
    assert!(!outcome.chunks.is_empty(), "the dense arm still answered");
}

/// The cache is per-process state, so it must survive across requests without
/// becoming the reason two callers see different graphs for the same scope.
#[tokio::test]
async fn a_warm_cache_gives_the_same_answer_as_a_cold_one() {
    let h = harness(&["doc-visible"]);
    seed_chunks(
        &h.chunks,
        "doc-visible",
        &["Alpha Corporation published the report."],
    )
    .await;
    seed_graph(
        &h.graph,
        &[
            ("alpha corporation", "Alpha Corporation", &["doc-visible_0"]),
            ("beta foundation", "Beta Foundation", &["doc-visible_0"]),
        ],
        &[(
            "alpha corporation",
            "beta foundation",
            "doc-visible",
            "doc-visible_0",
        )],
    )
    .await;

    let scope = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-visible")]);
    let arm = arm(&h);
    let cold = run(&h, &scope, "Alpha Corporation", Some(Arc::clone(&arm))).await;
    let warm = run(&h, &scope, "Alpha Corporation", Some(arm)).await;

    let ids = |o: &crate::RetrievalOutcome| -> Vec<String> {
        o.chunks.iter().map(|h| h.chunk.id.0.clone()).collect()
    };
    assert_eq!(
        ids(&cold),
        ids(&warm),
        "the cache must not change the answer"
    );
}

/// Scope, again, from the other end: two callers, one tenant, one graph.
#[tokio::test]
async fn two_scopes_of_one_tenant_get_different_answers_from_the_same_graph() {
    let h = harness(&["doc-a", "doc-b"]);
    seed_chunks(&h.chunks, "doc-a", &["Alpha Corporation published."]).await;
    seed_chunks(&h.chunks, "doc-b", &["Beta Foundation published."]).await;
    seed_graph(
        &h.graph,
        &[
            ("alpha corporation", "Alpha Corporation", &["doc-a_0"]),
            ("beta foundation", "Beta Foundation", &["doc-b_0"]),
        ],
        &[("alpha corporation", "beta foundation", "doc-a", "doc-a_0")],
    )
    .await;

    let arm = arm(&h);
    let scope_a = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-a")]);
    let scope_b = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-b")]);

    let a = run(&h, &scope_a, "Alpha Corporation", Some(Arc::clone(&arm))).await;
    let b = run(&h, &scope_b, "Beta Foundation", Some(Arc::clone(&arm))).await;

    let ids_a: Vec<&str> = a.chunks.iter().map(|h| h.chunk.id.0.as_str()).collect();
    let ids_b: Vec<&str> = b.chunks.iter().map(|h| h.chunk.id.0.as_str()).collect();
    assert!(!ids_a.is_empty(), "scope A must get an answer: {ids_a:?}");
    assert!(!ids_b.is_empty(), "scope B must get an answer: {ids_b:?}");
    assert!(
        ids_a.iter().all(|id| id.starts_with("doc-a_")),
        "scope A saw {ids_a:?}"
    );
    assert!(
        ids_b.iter().all(|id| id.starts_with("doc-b_")),
        "scope B saw {ids_b:?}"
    );
}

/// The index the arm fetches through must be the caller's scoped one.
#[tokio::test]
async fn the_index_fetch_honours_the_scope_it_is_given() {
    let h = harness(&["doc-a", "doc-b"]);
    seed_chunks(&h.chunks, "doc-a", &["Alpha Corporation published."]).await;
    seed_chunks(
        &h.chunks,
        "doc-b",
        &["Alpha Corporation and Beta Foundation."],
    )
    .await;
    let scope = make_fake_scope(tenant(), vec![DocumentId::new_unchecked("doc-a")]);
    let ids = vec![
        ChunkId("doc-a_0".to_string()),
        ChunkId("doc-b_0".to_string()),
    ];
    let fetched = h.chunks.fetch(&scope, &ids).await.expect("fetch");
    assert_eq!(fetched.len(), 1, "{fetched:?}");
    assert_eq!(fetched[0].unit_id.0, "doc-a_0");
    assert_eq!(fetched[0].chunk_type, ChunkType::Text);
}
