//! The graph retrieval arm: seed, walk, and rank chunks — behind a flag.
//!
//! `docs/architecture-target.md` §11 asks for "a `graph` prefetch candidate list
//! (chunks ranked by PPR), fused like the other arms. It stays behind a flag
//! until the eval shows gains on multi-hop and cross-page questions." This is
//! that arm. `docs/eval-baseline.md` §6.4 is the eval, and it decides whether the
//! flag ever becomes a default.
//!
//! # Why the cache is keyed by scope and not by tenant
//!
//! [`GraphStore::load_tenant_graph`] returns a graph that is **already filtered
//! to the caller's visible documents**. Caching it under the tenant alone would
//! mean the first caller's graph answering the second caller's query: one KB's
//! entities steering another's results, through paths neither can see. That is the
//! leak the whole port exists to prevent, and it would be introduced by the cache
//! rather than avoided by it.
//!
//! So the key is `(tenant, the scope's document ids)`, and the entry *stores* the
//! resolved key rather than only a digest of it. A digest collision would then have
//! to survive a full comparison of both id lists, which makes serving scope X for
//! scope Y impossible by construction rather than by unlikely accident.
//!
//! # Why there is a version counter
//!
//! §6.3 measured `load_tenant_graph` at 855 ms p50 for 100k relations. That is
//! fine once per graph mutation and unsalvageable once per query. Every read
//! checks [`GraphStore::graph_version`] first and reloads on a mismatch, which is
//! what the port's `version_bumps_on_every_write` contract property exists to make
//! true: a write that does not bump the version is a stale cache, and a stale
//! cache reads as "the answer changed and nobody knows why".

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kb_core::graph::{ExpandedNode, ScoredNode, WalkOptions};
use kb_ports::{ChunkId, DocumentScope, GraphStore, PortError, TenantGraph};

use crate::timing::{Clock, Stopwatch};

/// What the arm produces for one query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphRanking {
    /// Chunk ids, best first.
    pub chunks: Vec<ChunkId>,
    /// Entities that seeded the walk.
    pub seeds: usize,
    /// Entities in the scoped graph the walk ran on.
    pub entities: usize,
    /// True when the scoped graph had to be loaded from the store, i.e. this is a
    /// cold query. Reported so the latency numbers can be split rather than
    /// averaged into one meaningless figure.
    pub cold: bool,
    /// A normalised seed key that did not resolve to an entity in this scope.
    pub unmatched_terms: usize,
}

/// Arm tunables.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphArmConfig {
    /// Entities seeded into PageRank, best match first.
    pub max_seeds: usize,
    /// Query terms offered to `match_entities`.
    pub max_terms: usize,
    /// PPR teleport probability.
    pub damping: f32,
    /// PPR convergence tolerance (also the push contribution floor).
    pub tolerance: f32,
    /// Expansion depth in hops, on top of the PageRank mass.
    pub expand_depth: usize,
    /// How much of an expanded node's score counts next to its PageRank score.
    pub expansion_weight: f32,
    /// Chunks returned to the fusion.
    pub candidates: usize,
    /// Cached graphs and the byte budget across them.
    pub cache_entries: usize,
    pub cache_bytes: usize,
}

impl Default for GraphArmConfig {
    fn default() -> Self {
        Self {
            max_seeds: 8,
            max_terms: 24,
            damping: 0.85,
            tolerance: 1e-3,
            expand_depth: 2,
            expansion_weight: 0.5,
            candidates: 20,
            cache_entries: 16,
            cache_bytes: 256 * 1024 * 1024,
        }
    }
}

/// One cached scoped graph: the graph, the store version it was loaded at, and
/// the identity of the scope it is allowed to answer for.
#[derive(Clone)]
struct CacheEntry {
    tenant: String,
    /// The scope's document ids, **sorted**. Sorted because
    /// [`DocumentScope::document_ids`] is explicitly in arbitrary order, and two
    /// scopes with the same visible set must land on the same entry.
    documents: Arc<Vec<String>>,
    version: u64,
    graph: Arc<TenantGraph>,
    bytes: usize,
}

/// LRU over `(tenant, scope)`.
pub struct GraphCache {
    entries: Mutex<Vec<CacheEntry>>,
    max_entries: usize,
    max_bytes: usize,
}

impl std::fmt::Debug for GraphCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let entries = self.entries.lock().map(|e| e.len()).unwrap_or(0);
        f.debug_struct("GraphCache")
            .field("entries", &entries)
            .field("max_entries", &self.max_entries)
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

impl GraphCache {
    #[must_use]
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            max_entries: max_entries.max(1),
            max_bytes,
        }
    }

    /// The entry for `scope` at `version`, if one is cached.
    fn get(&self, tenant: &str, documents: &[String], version: u64) -> Option<Arc<TenantGraph>> {
        let mut entries = self.entries.lock().ok()?;
        let hit = entries.iter().position(|e| {
            e.tenant == tenant && e.documents.as_ref() == documents && e.version == version
        })?;
        let entry = entries.remove(hit);
        let graph = Arc::clone(&entry.graph);
        entries.insert(0, entry);
        Some(graph)
    }

    fn put(&self, entry: CacheEntry) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.retain(|e| {
            !(e.tenant == entry.tenant && e.documents.as_ref() == entry.documents.as_ref())
        });
        entries.insert(0, entry);
        // Least recently used goes last, so evict from the back.
        while entries.len() > self.max_entries {
            entries.pop();
        }
        let mut total: usize = entries.iter().map(|e| e.bytes).sum();
        while total > self.max_bytes && entries.len() > 1 {
            match entries.pop() {
                Some(dropped) => total = total.saturating_sub(dropped.bytes),
                None => break,
            }
        }
    }

    /// Cached graphs, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().map_or(0, |e| e.len())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes currently held, for tests and diagnostics.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.entries
            .lock()
            .map_or(0, |e| e.iter().map(|x| x.bytes).sum())
    }
}

/// The identity a cached graph is allowed to answer for.
fn scope_key(scope: &DocumentScope) -> (String, Vec<String>) {
    let mut documents: Vec<String> = scope
        .document_ids()
        .iter()
        .map(ToString::to_string)
        .collect();
    documents.sort();
    (scope.tenant().as_str().to_string(), documents)
}

/// Seed a walk, run it, and rank the caller's chunks by it.
#[derive(Clone)]
pub struct GraphArm {
    store: Arc<dyn GraphStore>,
    cache: Arc<GraphCache>,
    config: GraphArmConfig,
}

impl GraphArm {
    #[must_use]
    pub fn new(store: Arc<dyn GraphStore>, config: GraphArmConfig) -> Self {
        let cache = Arc::new(GraphCache::new(config.cache_entries, config.cache_bytes));
        Self {
            store,
            cache,
            config,
        }
    }

    #[must_use]
    pub fn with_cache(
        store: Arc<dyn GraphStore>,
        cache: Arc<GraphCache>,
        config: GraphArmConfig,
    ) -> Self {
        Self {
            store,
            cache,
            config,
        }
    }

    #[must_use]
    pub fn config(&self) -> &GraphArmConfig {
        &self.config
    }

    /// The scoped graph, from the cache when the store's version has not moved.
    async fn scoped_graph(
        &self,
        scope: &DocumentScope,
    ) -> Result<(Arc<TenantGraph>, bool), PortError> {
        let (tenant, documents) = scope_key(scope);
        let version = self.store.graph_version(scope.tenant()).await?;
        if let Some(graph) = self.cache.get(&tenant, &documents, version) {
            return Ok((graph, false));
        }
        let graph = Arc::new(self.store.load_tenant_graph(scope).await?);
        self.cache.put(CacheEntry {
            tenant,
            documents: Arc::new(documents),
            version,
            bytes: graph.approx_heap_bytes(),
            graph: Arc::clone(&graph),
        });
        Ok((graph, true))
    }

    /// Rank chunks for `query` under `scope`.
    ///
    /// An empty result is a legitimate answer, not an error: a query with no
    /// entity in this scope has nothing for the graph to say, and the other arms
    /// still have theirs. Only a store failure is an error, and the caller turns
    /// that into a degrade rather than dropping the request.
    pub async fn rank(
        &self,
        scope: &DocumentScope,
        query: &str,
        clock: &dyn Clock,
    ) -> Result<GraphRanking, PortError> {
        let terms = kb_core::query_terms(query, self.config.max_terms);
        let matched = self.store.match_entities(scope, &terms).await?;
        let matched_count = matched.len();
        let (seeds, seeds_truncated) = take_seeds(matched, self.config.max_seeds);
        let unmatched_terms = terms.len().saturating_sub(matched_count);

        if seeds.is_empty() {
            return Ok(GraphRanking {
                unmatched_terms,
                ..GraphRanking::default()
            });
        }
        let seeds = normalise_seeds(seeds);

        let graph_sw = Stopwatch::start(clock);
        let (graph, cold) = self.scoped_graph(scope).await?;
        let _ = graph_sw;

        let walk_sw = Stopwatch::start(clock);
        let options = WalkOptions::default()
            .with_damping(self.config.damping)
            .with_tolerance(self.config.tolerance)
            .with_bounded_push(self.config.tolerance);
        let scored = graph.ppr(&seeds, &options);
        let expanded: Vec<ExpandedNode> = graph.expand(&seeds, self.config.expand_depth);
        // The seeds themselves are candidates. PageRank's teleport term says a
        // seed always keeps mass, but the push solver only emits nodes it pushed
        // into, so an entity whose chunk mentions nothing else — the common case
        // for a one-entity document — would otherwise contribute nothing at all.
        // Adding the seed mass back is the teleport term made explicit, not a
        // second opinion about it.
        let seed_nodes: Vec<ScoredNode> = seeds
            .iter()
            .filter_map(|(key, weight)| {
                graph.map().id_of(key).map(|node| ScoredNode {
                    node,
                    score: *weight,
                })
            })
            .collect();
        let ranked = merge(
            &scored,
            &expanded,
            &seed_nodes,
            self.config.expansion_weight,
        );
        let mut chunks = graph.chunks_ranked_by(&ranked);
        chunks.truncate(self.config.candidates);
        let _ = walk_sw;

        Ok(GraphRanking {
            chunks,
            seeds: seeds.len() + seeds_truncated,
            entities: graph.entity_count(),
            cold,
            unmatched_terms,
        })
    }
}

/// Keep the best `max` seeds and report how many were dropped.
fn take_seeds(matched: Vec<(String, f32)>, max: usize) -> (Vec<(String, f32)>, usize) {
    let mut matched = matched;
    matched.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let dropped = matched.len().saturating_sub(max);
    matched.truncate(max);
    (matched, dropped)
}

/// Rescale seed weights so the best match is 1.0.
///
/// `match_entities` divides hits by the number of query terms, so the absolute
/// value depends on how the question was worded. PageRank only cares about
/// relative seed mass, and leaving the scale to the port would make the walk's
/// behaviour depend on the query's length rather than on the graph.
fn normalise_seeds(mut seeds: Vec<(String, f32)>) -> Vec<(String, f32)> {
    let top = seeds
        .iter()
        .map(|(_, w)| *w)
        .fold(0.0f32, f32::max)
        .max(f32::EPSILON);
    for (_, weight) in &mut seeds {
        *weight /= top;
    }
    seeds
}

/// PageRank mass and expansion proximity in one ranking.
///
/// Expansion is added, not merged: PPR says "this entity is central to what the
/// query is about", expansion says "this entity is close to a seed". A multi-hop
/// question needs both — the bridge entity is neither central nor a seed — so
/// expansion's contribution is scaled and added, and a node reached by both wins.
///
/// The seeds go in at their own weight, which is the teleport term.
fn merge(
    ppr: &[ScoredNode],
    expanded: &[ExpandedNode],
    seeds: &[ScoredNode],
    weight: f32,
) -> Vec<ScoredNode> {
    let mut combined: HashMap<u32, f32> =
        HashMap::with_capacity(ppr.len() + expanded.len() + seeds.len());
    for node in ppr.iter().chain(seeds) {
        combined
            .entry(node.node)
            .and_modify(|score| *score = score.max(node.score))
            .or_insert(node.score);
    }
    // An expanded node carries the score it was reached *from* and the weight of
    // the edge, which together are how near the seed it is. `min` takes the
    // weakest hop of the walk: a node two hops from a strong seed is still further
    // than a node one hop from a weak one.
    for node in expanded {
        let reached = weight * node.from_score.min(node.edge_weight);
        *combined.entry(node.node).or_insert(0.0) += reached;
    }
    let mut out: Vec<ScoredNode> = combined
        .into_iter()
        .map(|(node, score)| ScoredNode { node, score })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node.cmp(&b.node))
    });
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use kb_ports::{DocumentId, EntityMentions, EntityUpsert, RelationUpsert, TenantId};

    use crate::fakes::make_fake_scope;

    fn scope_of(tenant: &str, documents: &[&str]) -> DocumentScope {
        make_fake_scope(
            TenantId::new_unchecked(tenant),
            documents
                .iter()
                .map(|d| DocumentId::new_unchecked(*d))
                .collect(),
        )
    }

    /// A store that counts loads and can be told to fail, so the cache's
    /// behaviour is observable rather than inferred.
    #[derive(Default)]
    struct CountingStore {
        inner: Mutex<CountingInner>,
    }

    #[derive(Default)]
    struct CountingInner {
        loads: usize,
        versions: usize,
        version: u64,
        fail: bool,
        labels: Vec<(String, String)>,
        mentions: HashMap<String, Vec<ChunkId>>,
    }

    impl CountingStore {
        fn bump_version(&self) {
            let mut inner = self.inner.lock().unwrap();
            inner.version = inner.version.wrapping_add(1);
        }

        fn set_fail(&self, fail: bool) {
            self.inner.lock().unwrap().fail = fail;
        }

        fn loads(&self) -> usize {
            self.inner.lock().unwrap().loads
        }

        fn seed(&self, labels: &[(&str, &str)], mentions: &[(&str, &[&str])]) {
            let mut inner = self.inner.lock().unwrap();
            inner.labels = labels
                .iter()
                .map(|(k, n)| ((*k).to_string(), (*n).to_string()))
                .collect();
            inner.mentions = mentions
                .iter()
                .map(|(key, chunks)| {
                    (
                        (*key).to_string(),
                        chunks.iter().map(|c| ChunkId((*c).to_string())).collect(),
                    )
                })
                .collect();
        }
    }

    #[async_trait::async_trait]
    impl GraphStore for CountingStore {
        async fn upsert_entities(
            &self,
            _t: &TenantId,
            _d: &DocumentId,
            _e: Vec<EntityUpsert>,
        ) -> Result<(), PortError> {
            Ok(())
        }

        async fn relate(
            &self,
            _t: &TenantId,
            _d: &DocumentId,
            _r: Vec<RelationUpsert>,
        ) -> Result<(), PortError> {
            Ok(())
        }

        async fn delete_document(&self, _t: &TenantId, _d: &DocumentId) -> Result<(), PortError> {
            Ok(())
        }

        async fn load_tenant_graph(&self, scope: &DocumentScope) -> Result<TenantGraph, PortError> {
            let mut inner = self.inner.lock().unwrap();
            if inner.fail {
                return Err(PortError::Unavailable("graph store is down".into()));
            }
            inner.loads += 1;
            let map_len = inner.labels.len();
            let mention_pairs: Vec<(String, Vec<ChunkId>)> = inner
                .mentions
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let mut map = kb_core::graph::NodeMap::new();
            let mut edges: Vec<kb_core::graph::Edge> = Vec::new();
            for (key, _) in &inner.labels {
                map.intern(key);
            }
            // Chain every pair so the graph is connected and a walk can move.
            for i in 1..map.len() {
                edges.push(kb_core::graph::Edge {
                    src: 0,
                    dst: i as u32,
                    weight: 1.0,
                });
            }
            let csr = kb_core::graph::Csr::from_edges(map_len, &edges);
            let mut mentions: Vec<EntityMentions> = inner
                .labels
                .iter()
                .map(|(key, _)| EntityMentions {
                    key: key.clone(),
                    chunk_ids: Vec::new(),
                })
                .collect();
            for (key, chunks) in &mention_pairs {
                if let Some(id) = map.id_of(key) {
                    mentions[id as usize].chunk_ids = chunks.clone();
                }
            }
            let _ = scope;
            Ok(TenantGraph::new(map, csr, mentions))
        }

        async fn graph_version(&self, _t: &TenantId) -> Result<u64, PortError> {
            let mut inner = self.inner.lock().unwrap();
            if inner.fail {
                return Err(PortError::Unavailable("graph store is down".into()));
            }
            inner.versions += 1;
            Ok(inner.version)
        }

        async fn match_entities(
            &self,
            scope: &DocumentScope,
            terms: &[String],
        ) -> Result<Vec<(String, f32)>, PortError> {
            let inner = self.inner.lock().unwrap();
            if inner.fail {
                return Err(PortError::Unavailable("graph store is down".into()));
            }
            Ok(inner
                .labels
                .iter()
                .filter(|(key, _)| {
                    scope.document_ids().iter().any(|d| !d.0.is_empty())
                        && terms.iter().any(|t| key.contains(t.as_str()))
                })
                .map(|(key, _)| (key.clone(), 1.0))
                .collect())
        }

        async fn entity_mentions(
            &self,
            _s: &DocumentScope,
            _k: &str,
        ) -> Result<Vec<ChunkId>, PortError> {
            Ok(Vec::new())
        }
    }

    fn arm(store: &Arc<CountingStore>) -> GraphArm {
        GraphArm::new(
            Arc::clone(store) as Arc<dyn GraphStore>,
            GraphArmConfig {
                tolerance: 1e-3,
                ..GraphArmConfig::default()
            },
        )
    }

    fn fixture() -> Arc<CountingStore> {
        let store = Arc::new(CountingStore::default());
        store.seed(
            &[
                ("bank indonesia", "Bank Indonesia"),
                ("bank mandiri", "Bank Mandiri"),
            ],
            &[
                ("bank indonesia", &["doc_1_0", "doc_2_0"]),
                ("bank mandiri", &["doc_2_1"]),
            ],
        );
        store
    }

    #[tokio::test]
    async fn it_ranks_a_mention_of_a_seeded_entity() {
        let store = fixture();
        let arm = arm(&store);
        let scope = scope_of("tenant-a", &["doc-1", "doc-2"]);
        let ranking = arm
            .rank(&scope, "Bank Indonesia", &crate::timing::SystemClock)
            .await
            .expect("ranking");
        assert!(!ranking.chunks.is_empty(), "got {ranking:?}");
        assert!(ranking.chunks.contains(&ChunkId("doc_2_1".to_string())));
        // The query names one entity, but `match_entities` matches on substrings,
        // so the unigram "bank" also reaches "bank mandiri". Both are seeds; the
        // one the question names has to win on PageRank mass and therefore rank
        // its own chunks first. That is the property a multi-hop question
        // depends on: the named entity's chunk leads, the neighbour's follows.
        assert_eq!(ranking.seeds, 2);
        assert!(
            matches!(ranking.chunks.first(), Some(id) if id.0 == "doc_1_0" || id.0 == "doc_2_0"),
            "the named entity's chunks must lead: {:?}",
            ranking.chunks
        );
    }

    #[tokio::test]
    async fn a_query_with_no_entity_in_scope_is_an_empty_ranking_not_an_error() {
        let store = fixture();
        let arm = arm(&store);
        let scope = scope_of("tenant-a", &["doc-1"]);
        let ranking = arm
            .rank(
                &scope,
                "quantum chromodynamics",
                &crate::timing::SystemClock,
            )
            .await
            .expect("an empty answer is not a failure");
        assert!(ranking.chunks.is_empty());
        assert_eq!(ranking.seeds, 0);
        assert!(ranking.unmatched_terms > 0);
    }

    #[tokio::test]
    async fn the_first_query_is_cold_and_the_second_is_warm() {
        let store = fixture();
        let arm = arm(&store);
        let scope = scope_of("tenant-a", &["doc-1", "doc-2"]);
        let clock = crate::timing::SystemClock;
        assert!(
            arm.rank(&scope, "Bank Indonesia", &clock)
                .await
                .expect("ok")
                .cold
        );
        assert!(
            !arm.rank(&scope, "Bank Indonesia", &clock)
                .await
                .expect("ok")
                .cold
        );
        assert_eq!(store.loads(), 1, "the second query must hit the cache");
    }

    /// The one the whole module exists for.
    ///
    /// A cache entry built for scope X must never serve scope Y, even though both
    /// are the same tenant and even though the same store answered both. The
    /// graph is scope-filtered *before* the CSR is built, so a shared entry would
    /// be a shared leak — and the documents in scope Y are exactly the ones scope
    /// X must not learn about.
    #[tokio::test]
    async fn a_cache_entry_built_for_one_scope_never_serves_another() {
        let store = fixture();
        let arm = arm(&store);
        let clock = crate::timing::SystemClock;

        let narrow = scope_of("tenant-a", &["doc-1"]);
        let wide = scope_of("tenant-a", &["doc-1", "doc-2"]);

        let narrow_ranking = arm
            .rank(&narrow, "Bank Indonesia", &clock)
            .await
            .expect("ok");
        assert!(narrow_ranking.cold, "the first query for a scope is cold");
        let wide_ranking = arm.rank(&wide, "Bank Indonesia", &clock).await.expect("ok");
        assert!(
            wide_ranking.cold,
            "a different scope must be a different cache entry"
        );

        // And the other way round, because LRU eviction order must not matter.
        let wide_again = arm.rank(&wide, "Bank Indonesia", &clock).await.expect("ok");
        assert!(!wide_again.cold);
        let narrow_again = arm
            .rank(&narrow, "Bank Indonesia", &clock)
            .await
            .expect("ok");
        assert!(
            !narrow_again.cold,
            "narrow must still be cached alongside wide"
        );
        assert_eq!(store.loads(), 2, "exactly one load per scope");
    }

    #[tokio::test]
    async fn a_version_bump_invalidates_the_cache() {
        let store = fixture();
        let arm = arm(&store);
        let scope = scope_of("tenant-a", &["doc-1"]);
        let clock = crate::timing::SystemClock;
        assert!(
            arm.rank(&scope, "Bank Indonesia", &clock)
                .await
                .expect("ok")
                .cold
        );
        assert!(
            !arm.rank(&scope, "Bank Indonesia", &clock)
                .await
                .expect("ok")
                .cold
        );
        store.bump_version();
        assert!(
            arm.rank(&scope, "Bank Indonesia", &clock)
                .await
                .expect("ok")
                .cold,
            "a graph that changed must be reloaded"
        );
        assert_eq!(store.loads(), 2);
    }

    #[tokio::test]
    async fn a_failing_store_is_an_error_the_caller_can_degrade_on() {
        let store = fixture();
        let arm = arm(&store);
        let scope = scope_of("tenant-a", &["doc-1"]);
        let clock = crate::timing::SystemClock;
        arm.rank(&scope, "Bank Indonesia", &clock)
            .await
            .expect("ok");

        store.set_fail(true);
        let err = arm
            .rank(&scope, "Bank Indonesia", &clock)
            .await
            .expect_err("a down store is an error, not an empty answer");
        assert!(matches!(err, PortError::Unavailable(_)));
    }

    #[tokio::test]
    async fn a_failing_store_before_the_walk_is_also_an_error() {
        let store = fixture();
        store.set_fail(true);
        let arm = arm(&store);
        let scope = scope_of("tenant-a", &["doc-1"]);
        assert!(
            arm.rank(&scope, "Bank Indonesia", &crate::timing::SystemClock)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_cache_evicts_least_recently_used_first_within_its_entry_budget() {
        let cache = Arc::new(GraphCache::new(2, usize::MAX));
        let make = |documents: Vec<String>, version: u64| CacheEntry {
            tenant: "tenant-a".to_string(),
            documents: Arc::new(documents),
            version,
            bytes: 10,
            graph: Arc::new(TenantGraph::empty()),
        };
        cache.put(make(vec!["d1".to_string()], 1));
        cache.put(make(vec!["d2".to_string()], 1));
        assert!(cache.get("tenant-a", &["d1".to_string()], 1).is_some());
        cache.put(make(vec!["d3".to_string()], 1));
        assert_eq!(cache.len(), 2);
        assert!(
            cache.get("tenant-a", &["d1".to_string()], 1).is_some(),
            "d1 was just used, so d2 is the eviction"
        );
        assert!(cache.get("tenant-a", &["d2".to_string()], 1).is_none());
    }

    #[tokio::test]
    async fn the_cache_evicts_on_its_byte_budget() {
        let cache = Arc::new(GraphCache::new(16, 25));
        let make = |document: &str, bytes: usize| CacheEntry {
            tenant: "tenant-a".to_string(),
            documents: Arc::new(vec![document.to_string()]),
            version: 1,
            bytes,
            graph: Arc::new(TenantGraph::empty()),
        };
        cache.put(make("d1", 10));
        cache.put(make("d2", 10));
        cache.put(make("d3", 10));
        assert!(cache.bytes() <= 25, "held {} bytes", cache.bytes());
        assert!(cache.get("tenant-a", &["d1".to_string()], 1).is_none());
    }

    #[tokio::test]
    async fn a_stale_version_never_hits() {
        let cache = Arc::new(GraphCache::new(4, usize::MAX));
        cache.put(CacheEntry {
            tenant: "tenant-a".to_string(),
            documents: Arc::new(vec!["d1".to_string()]),
            version: 1,
            bytes: 1,
            graph: Arc::new(TenantGraph::empty()),
        });
        assert!(cache.get("tenant-a", &["d1".to_string()], 1).is_some());
        assert!(cache.get("tenant-a", &["d1".to_string()], 2).is_none());
    }

    #[test]
    fn seed_weights_are_rescaled_so_the_best_match_is_one() {
        let seeds = normalise_seeds(vec![
            ("a".to_string(), 0.2),
            ("b".to_string(), 0.5),
            ("c".to_string(), 0.0),
        ]);
        let weight = |key: &str| seeds.iter().find(|(k, _)| k == key).map(|(_, w)| *w);
        assert_eq!(weight("b"), Some(1.0), "the best match becomes 1.0");
        assert!((weight("a").expect("a") - 0.4).abs() < 1e-6);
        assert_eq!(weight("c"), Some(0.0));
    }

    #[test]
    fn a_zero_weighted_seed_does_not_divide_by_zero() {
        let seeds = normalise_seeds(vec![("a".to_string(), 0.0)]);
        assert!(seeds[0].1.is_finite());
    }

    #[test]
    fn seeds_are_capped_best_first_and_the_drop_count_is_reported() {
        let matched: Vec<(String, f32)> = (0..10)
            .map(|i| (format!("e{i}"), i as f32 / 10.0))
            .collect();
        let (kept, dropped) = take_seeds(matched, 3);
        assert_eq!(kept.len(), 3);
        assert_eq!(dropped, 7);
        assert_eq!(kept[0].0, "e9", "the best match seeds first");
    }

    #[test]
    fn expansion_adds_to_the_ranking_rather_than_replacing_it() {
        let ppr = vec![ScoredNode {
            node: 0,
            score: 0.5,
        }];
        let expanded = vec![
            ExpandedNode {
                node: 0,
                from_score: 1.0,
                edge_weight: 1.0,
            },
            ExpandedNode {
                node: 7,
                from_score: 1.0,
                edge_weight: 1.0,
            },
        ];
        let merged = merge(&ppr, &expanded, &[], 0.5);
        assert_eq!(merged[0].node, 0, "a node reached by both wins");
        assert_eq!(merged[1].node, 7);
        assert_eq!(merged[1].score, 0.5);
    }

    #[test]
    fn a_seed_with_no_edges_is_still_a_candidate() {
        // The push solver emits nothing for a node with no arcs, so without the
        // seed term an entity mentioned on its own contributes nothing.
        let seeds = [ScoredNode {
            node: 4,
            score: 1.0,
        }];
        assert!(merge(&[], &[], &seeds, 0.5).iter().any(|n| n.node == 4));
    }

    #[test]
    fn a_seed_outranks_a_node_the_walk_only_reached() {
        let seeds = [ScoredNode {
            node: 0,
            score: 1.0,
        }];
        let walked = [ScoredNode {
            node: 9,
            score: 0.01,
        }];
        let merged = merge(&walked, &[], &seeds, 0.5);
        assert_eq!(merged[0].node, 0, "the entity the query names comes first");
    }

    #[test]
    fn the_ranking_is_deterministic_for_the_same_walk() {
        let ppr = [
            ScoredNode {
                node: 3,
                score: 0.2,
            },
            ScoredNode {
                node: 1,
                score: 0.2,
            },
        ];
        assert_eq!(merge(&ppr, &[], &[], 0.5), merge(&ppr, &[], &[], 0.5));
        // Equal scores tie-break on node id, so two runs cannot disagree.
        let merged = merge(&ppr, &[], &[], 0.5);
        assert!(merged[0].node < merged[1].node);
    }
}
