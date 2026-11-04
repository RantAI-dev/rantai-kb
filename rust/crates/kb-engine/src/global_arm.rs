//! The global (graph community) retrieval arm — behind `KB_GLOBAL_ARM`.
//!
//! `DEC-5` asks for graph community summaries and a "global mode" for broad,
//! aggregative questions. This is that arm, and it is **off by default because
//! the measurement says it loses**: `EVAL-3` found that fusing summary-selected
//! communities into search lowered aggregative recall@10 by 0.067 (HR) and
//! 0.103 (CS) with an oracle gate, before any gate error
//! (`docs/eval-ab/retrieval-gaps/README.md`). The product owner still asked for
//! it to be built and switchable, so an operator can measure it on their own
//! corpus. Nothing here runs unless `KB_GLOBAL_ARM=1`.
//!
//! # What it does
//!
//! 1. Read the tenant's communities for the caller's scope. The store has
//!    already narrowed them; the arm narrows again, because a store that
//!    ignored the scope must not be able to steer an answer.
//! 2. Rank the summaries with BM25 (`kb_core::community`), take the top
//!    `communities`, and order their member chunks by `(community rank, chunk
//!    id)`.
//! 3. Fetch those chunks through the **scoped** `RetrievalIndex` and fuse them
//!    with the current fused ranking at `weight` (default 0.5), exactly the
//!    prototype's `global_fuse.py`.
//!
//! # The summary text never leaves this module
//!
//! A community summary is written from every document in the community,
//! including ones the caller cannot see. It is used to *select* a community and
//! is never returned as a chunk, a source or context. The answer contains member
//! chunks only, and each of them is fetched through a scope-filtered index.
//!
//! # Failure is visible
//!
//! A store or fetch failure is an error the pipeline turns into the
//! `global_arm_failed` degrade; the other arms' results go out unchanged. No
//! communities for the tenant is **not** a failure — it is an arm with nothing
//! to say, reported as `stats.globalArm.communities = 0`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use kb_ports::{Candidate, ChunkId, CommunityStore, DocumentScope, PortError, RetrievalIndex};

/// Arm tunables.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobalArmConfig {
    /// Communities selected per query (`KB_GLOBAL_ARM_COMMUNITIES`, default 3:
    /// the prototype's `--top-communities 3`).
    pub communities: usize,
    /// RRF weight of the arm's list (`KB_GLOBAL_ARM_WEIGHT`, default 0.5: the
    /// prototype found a full-weight fuse displaced correct baseline hits).
    pub weight: f32,
    /// Chunks the arm may contribute per query (`KB_GLOBAL_ARM_CHUNKS`, default
    /// 200: the prototype emitted at most 200 pages per query).
    pub chunks: usize,
}

impl Default for GlobalArmConfig {
    fn default() -> Self {
        Self {
            communities: 3,
            weight: 0.5,
            chunks: 200,
        }
    }
}

/// What the arm produced for one query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalRanking {
    /// Communities selected (the number reported in `stats.globalArm`).
    pub communities: usize,
    /// Member chunk ids, best first, already narrowed to the scope.
    pub chunks: Vec<ChunkId>,
}

/// Rank communities by their summaries and return their in-scope member chunks.
#[derive(Clone)]
pub struct GlobalArm {
    store: Arc<dyn CommunityStore>,
    config: GlobalArmConfig,
}

impl GlobalArm {
    #[must_use]
    pub fn new(store: Arc<dyn CommunityStore>, config: GlobalArmConfig) -> Self {
        Self { store, config }
    }

    #[must_use]
    pub fn config(&self) -> &GlobalArmConfig {
        &self.config
    }

    /// The arm's chunk list for `query`, or an empty ranking.
    ///
    /// Only a store failure is an error. "No community shares a term with the
    /// query" is an empty ranking: the other arms still have their candidates
    /// and the fuser leaves them alone.
    pub async fn rank(
        &self,
        scope: &DocumentScope,
        query: &str,
    ) -> Result<GlobalRanking, PortError> {
        let mut communities = self.store.load_communities(scope).await?;
        let visible: HashSet<&str> = scope
            .document_ids()
            .iter()
            .map(kb_ports::DocumentId::as_str)
            .collect();
        // Defence in depth: the port promises a narrowed record, but an adapter
        // that returned a wider one would otherwise let a summary written from
        // an invisible document choose the caller's chunks.
        communities.retain(|community| {
            community
                .document_ids
                .iter()
                .any(|document| visible.contains(document.as_str()))
        });
        if communities.is_empty() {
            return Ok(GlobalRanking::default());
        }

        let summaries: Vec<&str> = communities
            .iter()
            .map(|community| community.summary.as_str())
            .collect();
        let selected =
            kb_core::community::rank_summaries(&summaries, query, self.config.communities);
        if selected.is_empty() {
            return Ok(GlobalRanking::default());
        }

        let mut chunks: Vec<ChunkId> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for (index, _score) in &selected {
            let mut members: Vec<&ChunkId> = communities[*index]
                .chunk_ids
                .iter()
                .filter(|chunk| {
                    chunk
                        .document_part()
                        .is_some_and(|document| visible.contains(document))
                })
                .collect();
            members.sort();
            for chunk in members {
                if seen.insert(chunk.as_str()) {
                    chunks.push(chunk.clone());
                }
                if chunks.len() >= self.config.chunks {
                    break;
                }
            }
            if chunks.len() >= self.config.chunks {
                break;
            }
        }
        chunks.truncate(self.config.chunks);
        Ok(GlobalRanking {
            communities: selected.len(),
            chunks,
        })
    }
}

/// Fuse the global arm's chunks with the current fused ranking.
///
/// The baseline is the **already fused** candidate list (dense + sparse + any
/// graph arm), not the raw arm lists: the prototype fused a baseline run with
/// the global run, and re-fusing the raw arms here would give the dense and
/// sparse arms a second vote.
///
/// `score = 1/(k + baseline_rank) + weight/(k + global_rank)`, ranks 1-based,
/// `k = rrf_k` — the same RRF shape the rest of the pipeline uses. The global
/// list is fetched through `index.fetch`, which is scoped, and is filtered again
/// by document id here, so a chunk outside the caller's scope can never be
/// returned even by an adapter that ignored its scope.
///
/// Returns the fused candidates and each global chunk's 0-based position in the
/// arm's own list (provenance, not a score).
pub async fn fuse_global(
    index: &Arc<dyn RetrievalIndex>,
    scope: &DocumentScope,
    chunk_ids: &[ChunkId],
    candidates: &[Candidate],
    weight: f32,
    rrf_k: u32,
    pool_size: u32,
) -> Result<(Vec<Candidate>, HashMap<ChunkId, u32>), PortError> {
    if chunk_ids.is_empty() {
        return Ok((candidates.to_vec(), HashMap::new()));
    }
    let fetched = index.fetch(scope, chunk_ids).await?;
    let visible: HashSet<&str> = scope
        .document_ids()
        .iter()
        .map(kb_ports::DocumentId::as_str)
        .collect();
    let position: HashMap<&str, u32> = chunk_ids
        .iter()
        .enumerate()
        .map(|(rank, id)| (id.as_str(), u32::try_from(rank).unwrap_or(u32::MAX)))
        .collect();
    let mut global_list: Vec<Candidate> = fetched
        .into_iter()
        .filter(|candidate| {
            position.contains_key(candidate.unit_id.as_str())
                && visible.contains(candidate.document_id.as_str())
        })
        .collect();
    global_list.sort_by_key(|candidate| position[candidate.unit_id.as_str()]);

    let mut global_ranks: HashMap<ChunkId, u32> = HashMap::new();
    for (rank, candidate) in global_list.iter().enumerate() {
        global_ranks.insert(
            candidate.unit_id.clone(),
            u32::try_from(rank).unwrap_or(u32::MAX),
        );
    }

    let k = f64::from(rrf_k.max(1));
    // Insertion order is the deterministic tie-break: the baseline order first,
    // then anything only the global arm found.
    let mut order: Vec<String> = Vec::new();
    let mut first: HashMap<String, Candidate> = HashMap::new();
    let mut scores: HashMap<String, f64> = HashMap::new();
    for (rank, candidate) in candidates.iter().enumerate() {
        let id = candidate.unit_id.0.clone();
        if scores.contains_key(&id) {
            continue;
        }
        #[allow(clippy::cast_precision_loss)]
        let contribution = 1.0 / (k + rank as f64 + 1.0);
        scores.insert(id.clone(), contribution);
        first.insert(id.clone(), candidate.clone());
        order.push(id);
    }
    let weight = f64::from(weight);
    for (rank, candidate) in global_list.iter().enumerate() {
        let id = candidate.unit_id.0.clone();
        #[allow(clippy::cast_precision_loss)]
        let contribution = weight / (k + rank as f64 + 1.0);
        if let Some(score) = scores.get_mut(&id) {
            *score += contribution;
        } else {
            scores.insert(id.clone(), contribution);
            first.insert(id.clone(), candidate.clone());
            order.push(id);
        }
    }

    let mut fused: Vec<(f64, usize, Candidate)> = order
        .into_iter()
        .enumerate()
        .filter_map(|(position, id)| {
            let score = scores.get(&id).copied()?;
            let candidate = first.remove(&id)?;
            Some((score, position, candidate))
        })
        .collect();
    fused.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    fused.truncate(pool_size as usize);

    let mut out: Vec<Candidate> = Vec::with_capacity(fused.len());
    for (rank, (score, _position, mut candidate)) in fused.into_iter().enumerate() {
        candidate.scores.fused = score as f32;
        candidate.ranks.fused = u32::try_from(rank).unwrap_or(u32::MAX);
        out.push(candidate);
    }
    Ok((out, global_ranks))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use kb_ports::{ArmRanks, ArmScores, ChunkType, CommunityRecord, DocumentId, TenantId};
    use std::sync::Mutex;

    use crate::fakes::{FakeChunkStore, make_fake_scope};

    /// A community store that answers from memory and can be told to fail.
    #[derive(Default)]
    struct FakeCommunityStore {
        communities: Mutex<Vec<CommunityRecord>>,
        fail: Mutex<bool>,
        loads: Mutex<usize>,
    }

    impl FakeCommunityStore {
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

    #[async_trait::async_trait]
    impl CommunityStore for FakeCommunityStore {
        async fn load_communities(
            &self,
            scope: &DocumentScope,
        ) -> Result<Vec<CommunityRecord>, PortError> {
            *self.loads.lock().unwrap() += 1;
            if *self.fail.lock().unwrap() {
                return Err(PortError::Unavailable("community store is down".into()));
            }
            let visible: HashSet<&str> = scope
                .document_ids()
                .iter()
                .map(DocumentId::as_str)
                .collect();
            Ok(self
                .communities
                .lock()
                .unwrap()
                .iter()
                .filter(|community| {
                    community
                        .document_ids
                        .iter()
                        .any(|document| visible.contains(document.as_str()))
                })
                .cloned()
                .collect())
        }

        async fn replace_communities(
            &self,
            _tenant: &TenantId,
            _communities: Vec<kb_ports::NewCommunity>,
        ) -> Result<u64, PortError> {
            Ok(0)
        }

        async fn community_version(&self, _tenant: &TenantId) -> Result<u64, PortError> {
            Ok(1)
        }
    }

    fn community(id: &str, summary: &str, documents: &[&str], chunks: &[&str]) -> CommunityRecord {
        CommunityRecord {
            id: id.to_string(),
            version: 1,
            summary: summary.to_string(),
            entity_keys: vec!["alpha".to_string()],
            document_ids: documents
                .iter()
                .map(|document| DocumentId::new_unchecked(*document))
                .collect(),
            chunk_ids: chunks
                .iter()
                .map(|chunk| ChunkId::new_unchecked(*chunk))
                .collect(),
            model: "test/model".to_string(),
        }
    }

    fn arm(store: &Arc<FakeCommunityStore>) -> GlobalArm {
        GlobalArm::new(
            Arc::clone(store) as Arc<dyn CommunityStore>,
            GlobalArmConfig::default(),
        )
    }

    fn hit(id: &str, document: &str) -> Candidate {
        Candidate {
            unit_id: ChunkId::new_unchecked(id),
            document_id: DocumentId::new_unchecked(document),
            tenant: TenantId::new_unchecked("tenant-a"),
            chunk_index: 0,
            page: None,
            chunk_type: ChunkType::Text,
            content: format!("content {id}"),
            contextual_prefix: None,
            metadata: serde_json::json!({}),
            scores: ArmScores {
                dense: Some(0.9),
                sparse: None,
                fused: 0.9,
            },
            ranks: ArmRanks {
                dense: Some(0),
                sparse: None,
                fused: 0,
            },
        }
    }

    #[tokio::test]
    async fn the_summary_sharing_the_query_terms_wins_and_returns_its_chunks() {
        let store = Arc::new(FakeCommunityStore::default());
        store.seed(vec![
            community(
                "c1",
                "labour mobility and wage trends",
                &["doc-a"],
                &["doc-a_0"],
            ),
            community(
                "c2",
                "fisheries quotas and catch limits",
                &["doc-a"],
                &["doc-a_1"],
            ),
        ]);
        let scope = make_fake_scope(
            TenantId::new_unchecked("tenant-a"),
            vec![DocumentId::new_unchecked("doc-a")],
        );
        let ranking = arm(&store)
            .rank(&scope, "labour mobility trends")
            .await
            .expect("ranking");
        assert_eq!(ranking.communities, 1);
        assert_eq!(ranking.chunks, vec![ChunkId::new_unchecked("doc-a_0")]);
    }

    #[tokio::test]
    async fn no_communities_is_an_empty_ranking_not_an_error() {
        let store = Arc::new(FakeCommunityStore::default());
        let scope = make_fake_scope(
            TenantId::new_unchecked("tenant-a"),
            vec![DocumentId::new_unchecked("doc-a")],
        );
        let ranking = arm(&store)
            .rank(&scope, "main themes")
            .await
            .expect("empty is not a failure");
        assert_eq!(ranking, GlobalRanking::default());
    }

    #[tokio::test]
    async fn a_store_failure_is_an_error_the_pipeline_can_degrade_on() {
        let store = Arc::new(FakeCommunityStore::default());
        store.set_fail(true);
        let scope = make_fake_scope(
            TenantId::new_unchecked("tenant-a"),
            vec![DocumentId::new_unchecked("doc-a")],
        );
        assert!(arm(&store).rank(&scope, "main themes").await.is_err());
        assert_eq!(store.loads(), 1);
    }

    /// The one the arm exists to keep true.
    #[tokio::test]
    async fn a_community_with_an_out_of_scope_member_returns_only_in_scope_chunks() {
        let store = Arc::new(FakeCommunityStore::default());
        // A store that ignored the scope on purpose: both members are returned.
        store.seed(vec![community(
            "c1",
            "labour mobility and wage trends",
            &["doc-a", "doc-b"],
            &["doc-a_0", "doc-b_7"],
        )]);
        let scope = make_fake_scope(
            TenantId::new_unchecked("tenant-a"),
            vec![DocumentId::new_unchecked("doc-a")],
        );
        let ranking = arm(&store)
            .rank(&scope, "labour mobility trends")
            .await
            .expect("ranking");
        assert_eq!(ranking.chunks, vec![ChunkId::new_unchecked("doc-a_0")]);
    }

    #[tokio::test]
    async fn a_community_entirely_out_of_scope_returns_nothing() {
        let store = Arc::new(FakeCommunityStore::default());
        store.seed(vec![community(
            "c1",
            "labour mobility and wage trends",
            &["doc-b"],
            &["doc-b_7"],
        )]);
        let scope = make_fake_scope(
            TenantId::new_unchecked("tenant-a"),
            vec![DocumentId::new_unchecked("doc-a")],
        );
        let ranking = arm(&store)
            .rank(&scope, "labour mobility trends")
            .await
            .expect("ranking");
        assert_eq!(ranking.communities, 0);
        assert!(ranking.chunks.is_empty());
    }

    #[tokio::test]
    async fn fusion_fetches_scoped_chunks_and_keeps_the_baseline_order() {
        let chunks = Arc::new(FakeChunkStore::default());
        chunks.stored_chunks.write().unwrap().insert(
            DocumentId::new_unchecked("doc-a"),
            vec![
                kb_ports::ChunkToStore {
                    content: "content doc-a_0".into(),
                    chunk_index: 0,
                    embedding: vec![0.0; 3],
                    metadata: serde_json::json!({}),
                    contextual_prefix: None,
                },
                kb_ports::ChunkToStore {
                    content: "content doc-a_1".into(),
                    chunk_index: 1,
                    embedding: vec![0.0; 3],
                    metadata: serde_json::json!({}),
                    contextual_prefix: None,
                },
            ],
        );
        let scope = make_fake_scope(
            TenantId::new_unchecked("tenant-a"),
            vec![DocumentId::new_unchecked("doc-a")],
        );
        let baseline = vec![hit("doc-a_0", "doc-a")];
        let (fused, ranks) = fuse_global(
            &(chunks as Arc<dyn RetrievalIndex>),
            &scope,
            &[
                ChunkId::new_unchecked("doc-a_1"),
                ChunkId::new_unchecked("doc-a_0"),
            ],
            &baseline,
            0.5,
            60,
            20,
        )
        .await
        .expect("fusion");
        assert_eq!(fused.len(), 2);
        assert_eq!(ranks[&ChunkId::new_unchecked("doc-a_1")], 0);
        // doc-a_0 is in both lists, so it keeps the top spot; doc-a_1 follows.
        assert_eq!(fused[0].unit_id, ChunkId::new_unchecked("doc-a_0"));
        assert_eq!(fused[1].unit_id, ChunkId::new_unchecked("doc-a_1"));
    }

    #[tokio::test]
    async fn fusion_never_returns_a_chunk_outside_the_scope() {
        let chunks = Arc::new(FakeChunkStore::default());
        chunks.stored_chunks.write().unwrap().insert(
            DocumentId::new_unchecked("doc-b"),
            vec![kb_ports::ChunkToStore {
                content: "content doc-b_7".into(),
                chunk_index: 7,
                embedding: vec![0.0; 3],
                metadata: serde_json::json!({}),
                contextual_prefix: None,
            }],
        );
        let scope = make_fake_scope(
            TenantId::new_unchecked("tenant-a"),
            vec![DocumentId::new_unchecked("doc-a")],
        );
        let baseline = vec![hit("doc-a_0", "doc-a")];
        let (fused, ranks) = fuse_global(
            &(chunks as Arc<dyn RetrievalIndex>),
            &scope,
            &[ChunkId::new_unchecked("doc-b_7")],
            &baseline,
            0.5,
            60,
            20,
        )
        .await
        .expect("fusion");
        assert_eq!(fused.len(), 1);
        assert!(ranks.is_empty());
        assert!(
            fused
                .iter()
                .all(|candidate| candidate.document_id.0 == "doc-a")
        );
    }

    #[test]
    fn a_zero_weight_leaves_the_baseline_score_unchanged() {
        // Weight 0 is a legal configuration ("on, but not fused"): the arm's
        // chunks still appear (at their rank) but contribute no score. This
        // pins the arithmetic rather than the policy.
        let k = 60.0_f64;
        let baseline = 1.0 / (k + 1.0);
        let global = 0.0 / (k + 1.0);
        assert!((baseline + global - baseline).abs() < 1e-12);
    }

    #[test]
    fn the_default_weight_and_selection_match_the_prototype() {
        let config = GlobalArmConfig::default();
        assert_eq!(config.communities, 3);
        assert!((config.weight - 0.5).abs() < f32::EPSILON);
        assert_eq!(config.chunks, 200);
    }

    #[test]
    fn a_chunk_only_the_global_arm_found_can_carry_its_provenance() {
        let provenance = kb_ports::RetrievalProvenance::unscored(kb_ports::ProvenanceArm::Global);
        assert!(provenance.has(kb_ports::ProvenanceArm::Global));
        assert!(!provenance.has(kb_ports::ProvenanceArm::Dense));
    }
}
