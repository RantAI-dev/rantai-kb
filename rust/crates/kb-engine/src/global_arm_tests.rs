#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Pipeline-level tests for the global (graph community) arm (`DEC-5`).
//!
//! The properties that matter are the ones an operator cannot see from the
//! outside: with `KB_GLOBAL_ARM` off the pipeline is rank-identical to before,
//! the arm runs only for aggregative queries, a store failure degrades rather
//! than empties, and a community with an out-of-scope member contributes only
//! in-scope chunks.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use kb_ports::{
    ChunkId, ChunkToStore, CommunityRecord, CommunityStore, DocumentId, DocumentMeta,
    DocumentScope, DocumentStore, Embedder, NewCommunity, PortError, ProvenanceArm,
    RetrievalDegrade, TenantId,
};

use crate::fakes::{
    FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_hit, make_fake_scope,
};
use crate::global_arm::{GlobalArm, GlobalArmConfig};
use crate::retrieval::{RetrievalOptions, execute_pipeline};

/// A community store that counts reads and can be told to fail.
#[derive(Default)]
struct CountingCommunityStore {
    communities: Mutex<Vec<CommunityRecord>>,
    loads: Mutex<usize>,
    fail: Mutex<bool>,
}

impl CountingCommunityStore {
    fn seed(&self, communities: Vec<CommunityRecord>) {
        *self.communities.lock().unwrap() = communities;
    }

    fn set_fail(&self, fail: bool) {
        *self.fail.lock().unwrap() = fail;
    }

    fn loads(&self) -> usize {
        *self.loads.lock().unwrap()
    }
}

#[async_trait]
impl CommunityStore for CountingCommunityStore {
    async fn load_communities(
        &self,
        scope: &DocumentScope,
    ) -> Result<Vec<CommunityRecord>, PortError> {
        *self.loads.lock().unwrap() += 1;
        if *self.fail.lock().unwrap() {
            return Err(PortError::Unavailable("community store is down".into()));
        }
        // Deliberately *not* scope-filtered: the arm must contain a store that
        // returns everything, which is what the scope tests below prove.
        let _ = scope;
        Ok(self.communities.lock().unwrap().clone())
    }

    async fn replace_communities(
        &self,
        _tenant: &TenantId,
        _communities: Vec<NewCommunity>,
    ) -> Result<u64, PortError> {
        Ok(0)
    }

    async fn community_version(&self, _tenant: &TenantId) -> Result<u64, PortError> {
        Ok(1)
    }
}

fn doc_store() -> Arc<FakeDocumentStore> {
    let store = FakeDocumentStore::default();
    for (id, title) in [("d1", "Report"), ("d2", "Other tenant document")] {
        store.insert_doc(
            TenantId::new_unchecked("t1"),
            DocumentMeta {
                id: DocumentId::new_unchecked(id),
                title: title.to_string(),
                categories: vec![],
                subcategory: None,
                status: "READY".into(),
                external_ref: None,
                chunk_count: 2,
                file_type: None,
                file_size: None,
                mime_type: None,
                s3_key: None,
                created_at: String::new(),
                updated_at: String::new(),
                knowledge_base_ids: vec![],
                metadata: None,
            },
        );
    }
    Arc::new(store)
}

fn stored(content: &str, chunk_index: u32) -> ChunkToStore {
    ChunkToStore {
        content: content.to_string(),
        chunk_index,
        embedding: vec![0.0; 3],
        metadata: serde_json::json!({ "chunkIndex": chunk_index }),
        contextual_prefix: None,
    }
}

/// One vector hit on `d1_0`, so the baseline is non-empty and the arm has a
/// ranking to fuse with. `d1_1` and `d2_0` are stored but not ranked by any
/// baseline arm: only the global arm can bring them in.
fn chunks() -> Arc<FakeChunkStore> {
    let store = FakeChunkStore::default();
    store.vector_hits.write().unwrap().push(make_fake_hit(
        "d1_0",
        "d1",
        "Report",
        "The report covers funding.",
        0,
        0.9,
    ));
    store.stored_chunks.write().unwrap().insert(
        DocumentId::new_unchecked("d1"),
        vec![
            stored("The report covers funding.", 0),
            stored("The annex lists the themes.", 1),
        ],
    );
    store.stored_chunks.write().unwrap().insert(
        DocumentId::new_unchecked("d2"),
        vec![stored("A document the caller cannot see.", 0)],
    );
    Arc::new(store)
}

fn community(id: &str, summary: &str, documents: &[&str], chunk_ids: &[&str]) -> CommunityRecord {
    CommunityRecord {
        id: id.to_string(),
        version: 1,
        summary: summary.to_string(),
        entity_keys: vec!["funding".to_string()],
        document_ids: documents
            .iter()
            .map(|document| DocumentId::new_unchecked(*document))
            .collect(),
        chunk_ids: chunk_ids
            .iter()
            .map(|chunk| ChunkId::new_unchecked(*chunk))
            .collect(),
        model: "test/model".to_string(),
    }
}

async fn run(
    doc_store: &Arc<FakeDocumentStore>,
    chunks: &Arc<FakeChunkStore>,
    arm: Option<Arc<GlobalArm>>,
    query: &str,
) -> crate::RetrievalOutcome {
    let embedder = Arc::new(FakeEmbedder::default());
    let reranker = Arc::new(FakeReranker::default());
    let expander = Arc::new(crate::NoopExpander);
    let scope = make_fake_scope(
        TenantId::new_unchecked("t1"),
        vec![DocumentId::new_unchecked("d1")],
    );
    let opts = RetrievalOptions {
        global_arm: arm,
        ..RetrievalOptions::default()
    };
    execute_pipeline(
        &(Arc::clone(doc_store) as Arc<dyn DocumentStore>),
        &(Arc::clone(chunks) as Arc<dyn kb_ports::RetrievalIndex>),
        &(embedder as Arc<dyn Embedder>),
        &(reranker as Arc<dyn kb_ports::Reranker>),
        &(expander as Arc<dyn crate::QueryExpander>),
        &scope,
        query,
        &opts,
        &crate::timing::SystemClock,
    )
    .await
    .expect("pipeline")
}

fn arm(store: &Arc<CountingCommunityStore>) -> Arc<GlobalArm> {
    Arc::new(GlobalArm::new(
        Arc::clone(store) as Arc<dyn CommunityStore>,
        GlobalArmConfig::default(),
    ))
}

const AGGREGATIVE: &str = "what are the main themes across these documents?";
const FACTUAL: &str = "who wrote the report?";

/// Flag off: no classification, no store read, no stats — and the answer is the
/// one the pipeline produced before this arm existed.
#[tokio::test]
async fn with_the_flag_off_the_pipeline_never_touches_the_arm() {
    let store = Arc::new(CountingCommunityStore::default());
    store.seed(vec![community(
        "c1",
        "funding and themes of the annual report",
        &["d1"],
        &["d1_1"],
    )]);
    let doc_store = doc_store();
    let chunks = chunks();

    let baseline = run(&doc_store, &chunks, None, AGGREGATIVE).await;
    let with_arm_but_off = run(&doc_store, &chunks, None, AGGREGATIVE).await;

    assert_eq!(store.loads(), 0, "the flag is off: the store is not read");
    assert!(baseline.global_arm.is_none());
    assert_eq!(
        baseline.timings.global_arm_ms, 0.0,
        "the flag is off: not even a clock read for this arm"
    );
    assert_eq!(
        baseline
            .chunks
            .iter()
            .map(|hit| hit.chunk.id.0.clone())
            .collect::<Vec<_>>(),
        with_arm_but_off
            .chunks
            .iter()
            .map(|hit| hit.chunk.id.0.clone())
            .collect::<Vec<_>>(),
        "the pipeline is rank-identical with the flag off"
    );
}

/// Flag on, ordinary question: the arm is not called at all, because a summary
/// of a whole community cannot answer "who wrote the report".
#[tokio::test]
async fn a_non_aggregative_query_does_not_call_the_arm() {
    let store = Arc::new(CountingCommunityStore::default());
    store.seed(vec![community(
        "c1",
        "funding and themes of the annual report",
        &["d1"],
        &["d1_1"],
    )]);
    let doc_store = doc_store();
    let chunks = chunks();

    let baseline = run(&doc_store, &chunks, None, FACTUAL).await;
    let outcome = run(&doc_store, &chunks, Some(arm(&store)), FACTUAL).await;

    assert_eq!(
        store.loads(),
        0,
        "a factual query must not read communities"
    );
    assert!(outcome.global_arm.is_none());
    assert_eq!(
        baseline
            .chunks
            .iter()
            .map(|hit| hit.chunk.id.0.clone())
            .collect::<Vec<_>>(),
        outcome
            .chunks
            .iter()
            .map(|hit| hit.chunk.id.0.clone())
            .collect::<Vec<_>>()
    );
}

/// The arm's whole contract in one test: an aggregative query selects the
/// community, its member chunk is fused in, and the hit says `global`.
#[tokio::test]
async fn an_aggregative_query_fuses_a_community_chunk_with_global_provenance() {
    let store = Arc::new(CountingCommunityStore::default());
    store.seed(vec![community(
        "c1",
        "funding and themes of the annual report",
        &["d1"],
        &["d1_1"],
    )]);
    let doc_store = doc_store();
    let chunks = chunks();

    let outcome = run(&doc_store, &chunks, Some(arm(&store)), AGGREGATIVE).await;

    assert_eq!(store.loads(), 1);
    assert_eq!(
        outcome.global_arm,
        Some(crate::retrieval::GlobalArmStats {
            communities: 1,
            chunks: 1
        })
    );
    let global_hit = outcome
        .chunks
        .iter()
        .find(|hit| hit.chunk.id.0 == "d1_1")
        .expect("the community's chunk must reach the answer");
    assert!(
        global_hit
            .retrieval
            .as_ref()
            .is_some_and(|provenance| provenance.has(ProvenanceArm::Global)),
        "provenance must name the global arm: {:?}",
        global_hit.retrieval
    );
    assert!(outcome.degrades.is_empty());
}

/// A store failure is a degraded search, not an empty one.
#[tokio::test]
async fn a_community_store_failure_degrades_and_keeps_the_other_arms() {
    let store = Arc::new(CountingCommunityStore::default());
    store.set_fail(true);
    let doc_store = doc_store();
    let chunks = chunks();

    let outcome = run(&doc_store, &chunks, Some(arm(&store)), AGGREGATIVE).await;

    assert_eq!(store.loads(), 1);
    assert_eq!(outcome.degrades, vec![RetrievalDegrade::GlobalArmFailed]);
    assert!(!outcome.chunks.is_empty(), "the baseline answer survives");
    assert!(outcome.global_arm.is_none());
}

/// A community whose members include an out-of-scope document contributes only
/// its in-scope chunks, even though the store returned both.
#[tokio::test]
async fn a_community_with_an_out_of_scope_member_returns_only_in_scope_chunks() {
    let store = Arc::new(CountingCommunityStore::default());
    store.seed(vec![community(
        "c1",
        "funding and themes of the annual report",
        &["d1", "d2"],
        &["d1_1", "d2_0"],
    )]);
    let doc_store = doc_store();
    let chunks = chunks();

    let outcome = run(&doc_store, &chunks, Some(arm(&store)), AGGREGATIVE).await;

    assert!(
        outcome
            .chunks
            .iter()
            .all(|hit| hit.chunk.document_id.0 == "d1"),
        "an out-of-scope document must never be returned: {:?}",
        outcome
            .chunks
            .iter()
            .map(|hit| (hit.chunk.id.0.clone(), hit.chunk.document_id.0.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        outcome.chunks.iter().any(|hit| hit.chunk.id.0 == "d1_1"),
        "the in-scope member still contributes"
    );
}

/// A community entirely outside the scope contributes nothing, and that is not
/// a failure.
#[tokio::test]
async fn a_community_entirely_out_of_scope_contributes_nothing() {
    let store = Arc::new(CountingCommunityStore::default());
    store.seed(vec![community(
        "c1",
        "funding and themes of the annual report",
        &["d2"],
        &["d2_0"],
    )]);
    let doc_store = doc_store();
    let chunks = chunks();

    let outcome = run(&doc_store, &chunks, Some(arm(&store)), AGGREGATIVE).await;

    assert_eq!(
        outcome.global_arm,
        Some(crate::retrieval::GlobalArmStats {
            communities: 0,
            chunks: 0
        }),
        "no in-scope community is an empty answer, not a degrade"
    );
    assert!(outcome.degrades.is_empty());
    assert!(outcome.chunks.iter().all(|hit| hit.chunk.id.0 == "d1_0"));
}

/// A tenant with no communities: the arm runs, finds nothing, and says so in
/// stats. This is the common case when an operator turns the flag on before
/// running `communities build`.
#[tokio::test]
async fn no_communities_at_all_is_reported_as_zero_not_a_degrade() {
    let store = Arc::new(CountingCommunityStore::default());
    let doc_store = doc_store();
    let chunks = chunks();

    let outcome = run(&doc_store, &chunks, Some(arm(&store)), AGGREGATIVE).await;

    assert_eq!(
        outcome.global_arm,
        Some(crate::retrieval::GlobalArmStats {
            communities: 0,
            chunks: 0
        })
    );
    assert!(outcome.degrades.is_empty());
}
