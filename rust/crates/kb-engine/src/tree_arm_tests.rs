//! Tree arm (`DEC-4`) engine tests: gating, fusion, provenance, degradation and
//! scope.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use kb_core::tree::{Outline, OutlineMethod, OutlineNode};
use kb_ports::{
    Candidate, ChunkToStore, DocumentId, DocumentMeta, DocumentScope, DocumentStore, Embedder,
    PortError, RetrievalIndex, TenantId,
};

use crate::fakes::{
    FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_hit, make_fake_scope,
};
use crate::retrieval::{RetrievalOptions, execute_pipeline};
use crate::timing::SystemClock;
use crate::tree_arm::{TreeArm, TreeArmConfig};
use crate::{NoopExpander, QueryExpander, RetrievalOutcome};

fn tenant() -> TenantId {
    TenantId("tenant-1".into())
}

fn doc_meta(id: &str, title: &str) -> DocumentMeta {
    DocumentMeta {
        id: DocumentId(id.into()),
        title: title.into(),
        categories: vec![],
        subcategory: None,
        status: "ready".into(),
        external_ref: None,
        chunk_count: 0,
        file_type: Some("pdf".into()),
        file_size: None,
        mime_type: None,
        s3_key: None,
        created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
        knowledge_base_ids: vec![],
        metadata: None,
    }
}

fn node(title: &str, level: u32, start_page: u32, end_page: u32) -> OutlineNode {
    OutlineNode {
        title: title.into(),
        level,
        start_page,
        end_page,
    }
}

fn outline(title: &str, nodes: Vec<OutlineNode>) -> Outline {
    Outline {
        document_title: Some(title.into()),
        method: OutlineMethod::Bookmarks,
        nodes,
    }
}

fn chunk(_document: &str, page: u32, index: u32, content: &str) -> ChunkToStore {
    ChunkToStore {
        content: content.into(),
        chunk_index: index,
        embedding: Vec::new(),
        metadata: serde_json::json!({ "page": page, "chunkIndex": index }),
        contextual_prefix: None,
    }
}

fn setup(docs: &[DocumentMeta]) -> (Arc<FakeDocumentStore>, Arc<FakeChunkStore>) {
    let documents = Arc::new(FakeDocumentStore::default());
    for doc in docs {
        documents.insert_doc(tenant(), doc.clone());
    }
    (documents, Arc::new(FakeChunkStore::default()))
}

fn options(tree: Option<TreeArm>) -> RetrievalOptions {
    RetrievalOptions {
        max_chunks: 10,
        neighbour_window: 0,
        figure_limit: 0,
        hybrid_bm25_enabled: false,
        tree_arm: tree.map(Arc::new),
        ..RetrievalOptions::default()
    }
}

async fn run(
    documents: &Arc<FakeDocumentStore>,
    chunks: &Arc<FakeChunkStore>,
    scope: &DocumentScope,
    query: &str,
    opts: &RetrievalOptions,
) -> RetrievalOutcome {
    execute_pipeline(
        &(documents.clone() as Arc<dyn DocumentStore>),
        &(chunks.clone() as Arc<dyn RetrievalIndex>),
        &(Arc::new(FakeEmbedder::default()) as Arc<dyn Embedder>),
        &(Arc::new(FakeReranker::default()) as Arc<dyn kb_ports::Reranker>),
        &(Arc::new(NoopExpander) as Arc<dyn QueryExpander>),
        scope,
        query,
        opts,
        &SystemClock,
    )
    .await
    .unwrap()
}

fn ids(outcome: &RetrievalOutcome) -> Vec<String> {
    outcome
        .chunks
        .iter()
        .map(|hit| hit.chunk.id.0.clone())
        .collect()
}

#[tokio::test]
async fn with_the_flag_off_nothing_reads_the_outline() {
    let (documents, chunks) = setup(&[doc_meta("d1", "Quarterly Report")]);
    let scope = make_fake_scope(tenant(), vec![DocumentId("d1".into())]);
    chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "d1_0",
        "d1",
        "Quarterly Report",
        "base",
        0,
        0.9,
    ));
    chunks.stored_chunks.write().unwrap().insert(
        DocumentId("d1".into()),
        vec![chunk("d1", 5, 5, "tree page")],
    );
    documents.insert_outline(
        DocumentId("d1".into()),
        outline(
            "Quarterly Report",
            vec![node("Chapter 2 Leave policy", 1, 5, 9)],
        ),
    );
    // If the arm ran at all this would make the search degrade, so its absence
    // proves the flag really is the off switch.
    documents
        .fail_outlines
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let outcome = run(
        &documents,
        &chunks,
        &scope,
        "what does chapter 2 say about leave?",
        &options(None),
    )
    .await;
    assert_eq!(ids(&outcome), vec!["d1_0"]);
    assert!(outcome.tree.is_none(), "no tree stats when the flag is off");
    assert!(outcome.degrades.is_empty());
}

#[tokio::test]
async fn a_structural_query_fuses_tree_chunks_and_marks_the_provenance() {
    let (documents, chunks) = setup(&[doc_meta("d1", "Quarterly Report")]);
    let scope = make_fake_scope(tenant(), vec![DocumentId("d1".into())]);
    chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "d1_0",
        "d1",
        "Quarterly Report",
        "base",
        0,
        0.9,
    ));
    chunks.stored_chunks.write().unwrap().insert(
        DocumentId("d1".into()),
        vec![
            chunk("d1", 0, 0, "base"),
            chunk("d1", 5, 5, "chapter two text"),
        ],
    );
    documents.insert_outline(
        DocumentId("d1".into()),
        outline(
            "Quarterly Report",
            vec![
                node("Chapter 1 Introduction", 1, 0, 4),
                node("Chapter 2 Leave policy", 1, 5, 9),
            ],
        ),
    );

    let outcome = run(
        &documents,
        &chunks,
        &scope,
        "what does chapter 2 say about leave?",
        &options(Some(TreeArm::new(TreeArmConfig::default()))),
    )
    .await;

    let stats = outcome.tree.expect("stats.treeArm");
    // The chapter-2 node scores explicitly; "chapter" also gives the
    // introduction's title a small BM25 score, which is the arm behaving as
    // designed (every positively scored node competes).
    assert_eq!(stats.nodes, 2);
    assert_eq!(stats.pages, 10, "two five-page nodes, no overlap");
    let tree_hit = outcome
        .chunks
        .iter()
        .find(|hit| hit.chunk.id.0 == "d1_5")
        .expect("the chunk on the chapter's page");
    assert!(
        tree_hit
            .retrieval
            .as_ref()
            .is_some_and(|p| p.has(kb_ports::ProvenanceArm::Tree)),
        "{:?}",
        tree_hit.retrieval
    );
    assert!(outcome.degrades.is_empty());
}

#[tokio::test]
async fn a_non_structural_query_never_calls_the_arm() {
    let (documents, chunks) = setup(&[doc_meta("d1", "Quarterly Report")]);
    let scope = make_fake_scope(tenant(), vec![DocumentId("d1".into())]);
    chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "d1_0",
        "d1",
        "Quarterly Report",
        "a",
        0,
        0.9,
    ));
    documents.insert_outline(
        DocumentId("d1".into()),
        outline("Quarterly Report", vec![node("Chapter 1", 1, 0, 1)]),
    );
    documents
        .fail_outlines
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let outcome = run(
        &documents,
        &chunks,
        &scope,
        "café recommendations near the station",
        &options(Some(TreeArm::new(TreeArmConfig::default()))),
    )
    .await;
    assert_eq!(ids(&outcome), vec!["d1_0"]);
    assert!(outcome.tree.is_none());
    assert!(
        outcome.degrades.is_empty(),
        "the arm was not called: no outline read, no degrade"
    );
}

#[tokio::test]
async fn a_failed_outline_read_degrades_but_returns_the_other_arms() {
    let (documents, chunks) = setup(&[doc_meta("d1", "Quarterly Report")]);
    let scope = make_fake_scope(tenant(), vec![DocumentId("d1".into())]);
    chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "d1_0",
        "d1",
        "Quarterly Report",
        "a",
        0,
        0.9,
    ));
    documents
        .fail_outlines
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let outcome = run(
        &documents,
        &chunks,
        &scope,
        "what does chapter 2 say?",
        &options(Some(TreeArm::new(TreeArmConfig::default()))),
    )
    .await;
    assert_eq!(
        ids(&outcome),
        vec!["d1_0"],
        "the text arm's answer survives"
    );
    assert!(outcome.tree.is_none());
    assert!(
        outcome
            .degrades
            .contains(&kb_ports::RetrievalDegrade::TreeArmFailed),
        "{:?}",
        outcome.degrades
    );
}

#[tokio::test]
async fn an_outline_outside_the_scope_is_never_read_or_returned() {
    let (documents, chunks) = setup(&[
        doc_meta("d1", "Annual Report"),
        doc_meta("d2", "Aquaculture Handbook"),
    ]);
    // The caller may see d1 only. d2 has the matching outline and a chunk on
    // the page the query names.
    let scope = make_fake_scope(tenant(), vec![DocumentId("d1".into())]);
    chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "d1_0",
        "d1",
        "Annual Report",
        "base",
        0,
        0.9,
    ));
    chunks.stored_chunks.write().unwrap().extend([
        (DocumentId("d1".into()), vec![chunk("d1", 0, 0, "base")]),
        (
            DocumentId("d2".into()),
            vec![chunk("d2", 5, 5, "secret chapter two")],
        ),
    ]);
    documents.insert_outline(
        DocumentId("d1".into()),
        outline("Annual Report", vec![node("Chapter 9 Other", 1, 1, 2)]),
    );
    documents.insert_outline(
        DocumentId("d2".into()),
        outline(
            "Aquaculture Handbook",
            vec![node("Chapter 2 Secret leave policy", 1, 5, 9)],
        ),
    );

    let outcome = run(
        &documents,
        &chunks,
        &scope,
        "what does chapter 2 say about leave?",
        &options(Some(TreeArm::new(TreeArmConfig::default()))),
    )
    .await;
    assert!(
        outcome
            .chunks
            .iter()
            .all(|hit| hit.chunk.document_id.0 == "d1"),
        "{:?}",
        ids(&outcome)
    );
    assert!(
        !outcome.chunks.iter().any(|hit| hit.chunk.id.0 == "d2_5"),
        "another document's matching outline reached the answer"
    );
}

#[tokio::test]
async fn the_arm_itself_only_reads_pages_for_documents_in_the_scope() {
    let (documents, chunks) = setup(&[
        doc_meta("d1", "Annual Report"),
        doc_meta("d2", "Aquaculture Handbook"),
    ]);
    let scope = make_fake_scope(tenant(), vec![DocumentId("d1".into())]);
    chunks.stored_chunks.write().unwrap().extend([
        (
            DocumentId("d1".into()),
            vec![chunk("d1", 5, 5, "visible chapter two")],
        ),
        (
            DocumentId("d2".into()),
            vec![chunk("d2", 5, 5, "other tenant chapter two")],
        ),
    ]);
    documents.insert_outline(
        DocumentId("d1".into()),
        outline("Annual Report", vec![node("Chapter 2 Leave", 1, 5, 9)]),
    );
    documents.insert_outline(
        DocumentId("d2".into()),
        outline(
            "Aquaculture Handbook",
            vec![node("Chapter 2 Leave", 1, 5, 9)],
        ),
    );

    let arm = TreeArm::new(TreeArmConfig::default());
    let ranking = arm
        .rank(
            &(documents.clone() as Arc<dyn DocumentStore>),
            &(chunks.clone() as Arc<dyn RetrievalIndex>),
            &scope,
            "what does chapter 2 say about leave?",
        )
        .await
        .unwrap();
    let returned: Vec<&str> = ranking
        .chunks
        .iter()
        .map(|candidate: &Candidate| candidate.document_id.as_str())
        .collect();
    assert_eq!(returned, vec!["d1"]);
}

/// An index that refuses `fetch_pages`, the shape of an adapter that has not
/// implemented the read yet.
struct NoPagesIndex;

#[async_trait]
impl RetrievalIndex for NoPagesIndex {
    async fn upsert(&self, _units: Vec<kb_ports::IndexUnit>) -> Result<(), PortError> {
        Ok(())
    }
    async fn delete_document(
        &self,
        _tenant: &TenantId,
        _document_id: &DocumentId,
    ) -> Result<u64, PortError> {
        Ok(0)
    }
    async fn search(
        &self,
        _query: &kb_ports::HybridQuery,
        _scope: &DocumentScope,
    ) -> Result<Vec<Candidate>, PortError> {
        Ok(Vec::new())
    }
    async fn fetch(
        &self,
        _scope: &DocumentScope,
        _ids: &[kb_ports::ChunkId],
    ) -> Result<Vec<Candidate>, PortError> {
        Ok(Vec::new())
    }
    async fn neighbours(
        &self,
        _scope: &DocumentScope,
        _document_id: &DocumentId,
        _chunk_indices: &[u32],
    ) -> Result<Vec<Candidate>, PortError> {
        Ok(Vec::new())
    }
    async fn figures(
        &self,
        _scope: &DocumentScope,
        _document_ids: &[DocumentId],
    ) -> Result<Vec<Candidate>, PortError> {
        Ok(Vec::new())
    }
    async fn ordered_chunks(
        &self,
        _tenant: &TenantId,
        _document_id: &DocumentId,
    ) -> Result<Vec<Candidate>, PortError> {
        Ok(Vec::new())
    }
    fn capabilities(&self) -> kb_ports::IndexCapabilities {
        kb_ports::IndexCapabilities::dense_only(4)
    }
}

#[tokio::test]
async fn an_index_without_the_page_read_returns_the_error_the_pipeline_degrades_on() {
    let documents = Arc::new(FakeDocumentStore::default());
    documents.insert_doc(tenant(), doc_meta("d1", "Annual Report"));
    documents.insert_outline(
        DocumentId("d1".into()),
        outline("Annual Report", vec![node("Chapter 2 Leave", 1, 5, 9)]),
    );
    let scope = make_fake_scope(tenant(), vec![DocumentId("d1".into())]);
    let index: Arc<dyn RetrievalIndex> = Arc::new(NoPagesIndex);
    let err = TreeArm::new(TreeArmConfig::default())
        .rank(
            &(documents.clone() as Arc<dyn DocumentStore>),
            &index,
            &scope,
            "what does chapter 2 say about leave?",
        )
        .await
        .expect_err("an adapter without the read must say so");
    assert!(matches!(err, PortError::Unavailable(_)), "{err:?}");
}
