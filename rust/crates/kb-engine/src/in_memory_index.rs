//! `InMemoryIndex`: the reference `RetrievalIndex`.
//!
//! Exact by construction — a linear cosine scan and a BM25 score computed over
//! exactly the units in scope — so it is the implementation every other adapter
//! is checked against, not just a test double. When Qdrant and this disagree,
//! this one is right by definition, which is what makes the disagreement
//! debuggable.
//!
//! Deliberate simplifications, each of which a real adapter must match:
//!
//! * **IDF is computed over the units in scope**, not over the whole corpus.
//!   Qdrant does the same with its `idf` corpus filter (v1.19+); the point of
//!   computing it here is that another tenant's vocabulary cannot skew a score.
//! * **RRF is fused in process**, because there is no server side to fuse in.
//!   The arithmetic is Qdrant's, exactly: `sum over arms of 1 / (2 + rank)`,
//!   rank 0-based ([`kb_core::rrf_contribution`]). The reference index
//!   and the store that fuses server-side have to agree on the numbers, not
//!   merely on the order, or the contract suite proves nothing.
//! * **`Fusion::None`** is honest rather than clever: with one arm the fused
//!   score is that arm's score. With two arms of different scales there is no
//!   meaningful "unfused" order, so the dense score is used as the ranking key
//!   and the sparse score is still reported. Callers that want a real ordering
//!   of two arms use [`Fusion::Rrf`].

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use kb_core::sparse::{BM25_K1, SparseVector};
use kb_ports::{
    ArmRanks, ArmScores, Candidate, ChunkId, DocumentId, DocumentScope, Fusion, HybridQuery,
    IndexCapabilities, IndexUnit, KeywordQuery, KeywordSemantics, PortError, RetrievalIndex,
    TenantId, sort_candidates,
};

/// One stored unit plus everything an arm needs to score it.
#[derive(Debug, Clone)]
struct StoredUnit {
    unit: IndexUnit,
}

/// The in-memory reference index. Cheap to clone: clones share storage, so a
/// test can hand the same index to several callers.
#[derive(Clone)]
pub struct InMemoryIndex {
    inner: Arc<RwLock<HashMap<(TenantId, ChunkId), StoredUnit>>>,
    dims: u32,
}

impl InMemoryIndex {
    /// Take the read lock, recovering from poisoning rather than panicking.
    ///
    /// A panic while the lock was held means some *other* test panicked with a
    /// reference borrowed from this index; the stored units are plain data with
    /// no invariant a panic could have broken, so recovering is correct and
    /// keeps one failing test from cascading into every other.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<(TenantId, ChunkId), StoredUnit>> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<(TenantId, ChunkId), StoredUnit>> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// An empty index over `dims`-dimensional dense vectors.
    #[must_use]
    pub fn new(dims: u32) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            dims,
        }
    }

    /// Every unit currently stored, in no particular order. For assertions.
    pub fn stored_count(&self) -> usize {
        self.read().len()
    }

    /// Units matching a scope, as clones. The single place the scope filter is
    /// applied, so no arm can forget it.
    fn in_scope(&self, scope: &DocumentScope) -> Vec<StoredUnit> {
        let guard = self.read();
        let mut out: Vec<StoredUnit> = guard
            .values()
            .filter(|s| {
                s.unit.tenant == *scope.tenant()
                    && scope.document_ids().contains(&s.unit.document_id)
            })
            .cloned()
            .collect();
        // HashMap iteration order is arbitrary; the arms must not inherit it,
        // or a tie would resolve differently between runs.
        out.sort_by(|a, b| {
            a.unit
                .document_id
                .as_str()
                .cmp(b.unit.document_id.as_str())
                .then_with(|| a.unit.chunk_index.cmp(&b.unit.chunk_index))
        });
        out
    }
}

#[async_trait]
impl RetrievalIndex for InMemoryIndex {
    async fn upsert(&self, units: Vec<IndexUnit>) -> Result<(), PortError> {
        if units.is_empty() {
            return Ok(());
        }
        // Reject a ragged batch before anything is written, so a caller cannot
        // end up with an index half in one vector space.
        let mut expected: Option<usize> = None;
        for unit in &units {
            if unit.dense.is_empty() {
                continue;
            }
            match expected {
                None => expected = Some(unit.dense.len()),
                Some(n) if n != unit.dense.len() => {
                    return Err(PortError::InvalidArgument(format!(
                        "dense dimension drift: {:?} is {} wide, expected {n}",
                        unit.id,
                        unit.dense.len()
                    )));
                }
                Some(_) => {}
            }
        }
        if let Some(n) = expected
            && self.dims > 0
            && self.dims as usize != n
        {
            return Err(PortError::InvalidArgument(format!(
                "dense dimension {n} does not match this index's {}",
                self.dims
            )));
        }
        let mut guard = self.write();
        for unit in units {
            let key = (unit.tenant.clone(), unit.id.clone());
            guard.insert(key, StoredUnit { unit });
        }
        Ok(())
    }

    async fn delete_document(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<u64, PortError> {
        let mut guard = self.write();
        let before = guard.len();
        guard.retain(|(t, _), stored| !(t == tenant && stored.unit.document_id == *document_id));
        Ok((before - guard.len()) as u64)
    }

    async fn search(
        &self,
        query: &HybridQuery,
        scope: &DocumentScope,
    ) -> Result<Vec<Candidate>, PortError> {
        // An empty scope means "nothing is visible"; returning everything here
        // is the leak the port exists to prevent.
        if scope.document_ids().is_empty() {
            return Ok(Vec::new());
        }
        let units = self.in_scope(scope);
        if units.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(vector) = &query.dense {
            validate_dims(vector, self.dims, "query")?;
        }
        if query.dense.is_none() && query.sparse.is_none() {
            return Ok(Vec::new());
        }

        let dense_list = query
            .dense
            .as_ref()
            .map(|v| self.dense_arm(&units, v, query.per_arm_k));
        let sparse_list = query
            .sparse
            .as_ref()
            .map(|q| self.sparse_arm(&units, q, query.per_arm_k));

        let fused = match query.fusion {
            Fusion::Rrf => fuse(dense_list, sparse_list),
            Fusion::None => unfuse(dense_list, sparse_list),
        };

        let mut candidates: Vec<Candidate> = fused
            .into_iter()
            .filter_map(|(id, arms)| {
                let stored = units.iter().find(|s| s.unit.id == id)?;
                Some(candidate_of(&stored.unit, arms))
            })
            .collect();
        sort_candidates(&mut candidates);
        candidates.truncate(query.k);
        // sort_candidates wrote `fused` ranks before the truncation, so they are
        // still the positions the candidate held in the full list, which is
        // what an arm rank means.
        Ok(candidates)
    }

    async fn fetch(
        &self,
        scope: &DocumentScope,
        ids: &[ChunkId],
    ) -> Result<Vec<Candidate>, PortError> {
        if ids.is_empty() || scope.document_ids().is_empty() {
            return Ok(Vec::new());
        }
        let guard = self.read();
        let mut out: Vec<Candidate> = Vec::new();
        for id in ids {
            let Some(stored) = guard.get(&(scope.tenant().clone(), id.clone())) else {
                continue;
            };
            // A unit in the caller's tenant but a document they cannot see is
            // still out of scope: returning it would leak across knowledge bases.
            if !scope.document_ids().contains(&stored.unit.document_id) {
                continue;
            }
            out.push(candidate_of(
                &stored.unit,
                (ArmScores::default(), ArmRanks::default()),
            ));
        }
        Ok(out)
    }

    async fn neighbours(
        &self,
        scope: &DocumentScope,
        document_id: &DocumentId,
        chunk_indices: &[u32],
    ) -> Result<Vec<Candidate>, PortError> {
        if chunk_indices.is_empty() || !scope.document_ids().contains(document_id) {
            return Ok(Vec::new());
        }
        let guard = self.read();
        let mut out: Vec<Candidate> = Vec::new();
        for stored in guard.values() {
            if stored.unit.tenant != *scope.tenant() || stored.unit.document_id != *document_id {
                continue;
            }
            // Adjacent = index ±1 of an anchor, and never an anchor itself. The
            // bound is what lets a caller expand without trusting the adapter to
            // have applied one.
            let adjacent = chunk_indices.iter().any(|a| {
                let d = i64::from(stored.unit.chunk_index) - i64::from(*a);
                d == 1 || d == -1
            });
            if !adjacent {
                continue;
            }
            out.push(candidate_of(
                &stored.unit,
                (ArmScores::default(), ArmRanks::default()),
            ));
        }
        // Ascending by chunk index: the order a reader assembles a document in.
        out.sort_by_key(|c| c.chunk_index);
        Ok(out)
    }

    async fn figures(
        &self,
        scope: &DocumentScope,
        document_ids: &[DocumentId],
    ) -> Result<Vec<Candidate>, PortError> {
        if document_ids.is_empty() || scope.document_ids().is_empty() {
            return Ok(Vec::new());
        }
        let units = self.in_scope(scope);
        let mut out = Vec::new();
        for stored in units {
            if stored.unit.chunk_type == kb_ports::ChunkType::Figure
                && document_ids.contains(&stored.unit.document_id)
            {
                out.push(candidate_of(
                    &stored.unit,
                    (ArmScores::default(), ArmRanks::default()),
                ));
            }
        }
        out.sort_by(|a, b| a.unit_id.as_str().cmp(b.unit_id.as_str()));
        Ok(out)
    }

    async fn ordered_chunks(
        &self,
        tenant: &TenantId,
        document_id: &DocumentId,
    ) -> Result<Vec<Candidate>, PortError> {
        let guard = self.read();
        let mut out: Vec<Candidate> = guard
            .values()
            .filter(|s| s.unit.tenant == *tenant && s.unit.document_id == *document_id)
            .map(|s| candidate_of(&s.unit, (ArmScores::default(), ArmRanks::default())))
            .collect();
        out.sort_by_key(|c| c.chunk_index);
        Ok(out)
    }

    async fn health_check(&self) -> Result<(), PortError> {
        Ok(())
    }

    fn capabilities(&self) -> IndexCapabilities {
        IndexCapabilities {
            dense_dims: Some(self.dims),
            sparse: Some(KeywordSemantics::AnyTerm),
            // There is no server: fusion happens in process, exactly as Qdrant
            // does it before sending a prefetch.
            server_side_fusion: false,
            multivector: false,
            // A linear scan is exact by definition.
            approximate_dense: false,
        }
    }
}

impl InMemoryIndex {
    /// Exact cosine over every in-scope unit, top `per_arm_k`.
    fn dense_arm(
        &self,
        units: &[StoredUnit],
        query: &[f32],
        per_arm_k: usize,
    ) -> Vec<(ChunkId, f32)> {
        let mut scored: Vec<(ChunkId, f32)> = units
            .iter()
            .filter(|s| !s.unit.dense.is_empty())
            .map(|s| (s.unit.id.clone(), cosine(&s.unit.dense, query)))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.as_str().cmp(b.0.as_str()))
        });
        scored.truncate(per_arm_k);
        scored
    }

    /// BM25 with IDF over the in-scope units, top `per_arm_k`.
    ///
    /// The unit's stored sparse vector already carries saturated tf (from
    /// `kb_core::sparse::encode_document`); this adds the idf factor and sums
    /// over the query terms the unit actually contains. OR-of-terms: a unit with
    /// one shared term scores, it does not get discarded for missing the rest.
    ///
    /// `n_docs` and the document frequencies come from *these* units, i.e. the
    /// scope. That is the property the contract suite's scope property checks:
    /// another tenant's vocabulary cannot change a score here, because it is not
    /// in the count.
    fn sparse_arm(
        &self,
        units: &[StoredUnit],
        query: &KeywordQuery,
        per_arm_k: usize,
    ) -> Vec<(ChunkId, f32)> {
        let n_docs = units.len() as f32;
        let mut scored: Vec<(ChunkId, f32)> = Vec::new();
        for stored in units {
            let score = bm25(&stored.unit.sparse, &query.vector, n_docs, |term| {
                count_df(units.iter().map(|u| &u.unit), term)
            });
            if score > 0.0 {
                scored.push((stored.unit.id.clone(), score));
            }
        }
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.as_str().cmp(b.0.as_str()))
        });
        scored.truncate(per_arm_k);
        scored
    }
}

/// BM25 sum over the query terms a document contains.
///
/// `idf(t) = ln(1 + (N - df + 0.5) / (df + 0.5))`, the non-negative form: a term
/// in every document contributes zero rather than a negative score. `df_of`
/// counts the corpus occurrences of one term id.
fn bm25<F>(doc: &SparseVector, query: &SparseVector, n_docs: f32, df_of: F) -> f32
where
    F: Fn(u32) -> usize,
{
    let mut total = 0.0f32;
    for (qi, qv) in query.indices.iter().zip(&query.values) {
        let Some(pos) = doc.indices.binary_search(qi).ok() else {
            // OR-of-terms: a query term the document does not contain simply
            // contributes nothing. It never disqualifies the document.
            continue;
        };
        let df = df_of(*qi) as f32;
        if df <= 0.0 {
            continue;
        }
        let idf = (1.0 + (n_docs - df + 0.5) / (df + 0.5)).ln();
        // The query term's own value is 1.0 for every term by construction, but
        // a caller may weight one up, so it multiplies rather than is assumed.
        total += idf * doc.values[pos] * qv;
    }
    total
}

/// How many of `docs` contain `term`.
fn count_df<'a, I, S>(docs: I, term: u32) -> usize
where
    I: Iterator<Item = &'a S>,
    S: 'a + HasSparse,
{
    docs.filter(|d| d.sparse().indices.binary_search(&term).is_ok())
        .count()
}

/// A stored unit's sparse vector, so [`count_df`] can be generic over a unit or
/// a borrowed one.
trait HasSparse {
    fn sparse(&self) -> &SparseVector;
}

impl HasSparse for IndexUnit {
    fn sparse(&self) -> &SparseVector {
        &self.sparse
    }
}

/// Reciprocal Rank Fusion, Qdrant's arithmetic.
///
/// One contribution per arm, `1 / (2 + rank)` with the rank 0-based inside
/// that arm's list — the same number the store computes server-side, so a
/// candidate's fused score here is the score it would get there.
fn fuse(
    dense: Option<Vec<(ChunkId, f32)>>,
    sparse: Option<Vec<(ChunkId, f32)>>,
) -> Vec<(ChunkId, (ArmScores, ArmRanks))> {
    // An index of every candidate, keyed by id, in arm order so the tie-break
    // never depends on a hash map's iteration order.
    let mut order: Vec<ChunkId> = Vec::new();
    let mut scores: HashMap<ChunkId, (ArmScores, ArmRanks)> = HashMap::new();
    let insert = |order: &mut Vec<ChunkId>,
                  scores: &mut HashMap<ChunkId, (ArmScores, ArmRanks)>,
                  list: Vec<(ChunkId, f32)>,
                  is_dense: bool| {
        for (rank, (id, score)) in list.into_iter().enumerate() {
            let entry = scores.entry(id.clone()).or_insert_with(|| {
                order.push(id.clone());
                (
                    ArmScores {
                        fused: 0.0,
                        ..ArmScores::default()
                    },
                    ArmRanks::default(),
                )
            });
            entry.0.fused += kb_core::rrf_contribution(rank as u32);
            if is_dense {
                entry.0.dense = Some(score);
                entry.1.dense = Some(rank as u32);
            } else {
                entry.0.sparse = Some(score);
                entry.1.sparse = Some(rank as u32);
            }
        }
    };
    if let Some(list) = dense {
        insert(&mut order, &mut scores, list, true);
    }
    if let Some(list) = sparse {
        insert(&mut order, &mut scores, list, false);
    }
    order
        .into_iter()
        .map(|id| {
            let s = scores.remove(&id).unwrap_or_default();
            (id, s)
        })
        .collect()
}

/// `Fusion::None`: no rank fusion. See the module docs for why that is the
/// honest choice rather than a clever one.
fn unfuse(
    dense: Option<Vec<(ChunkId, f32)>>,
    sparse: Option<Vec<(ChunkId, f32)>>,
) -> Vec<(ChunkId, (ArmScores, ArmRanks))> {
    let mut order: Vec<ChunkId> = Vec::new();
    let mut scores: HashMap<ChunkId, (ArmScores, ArmRanks)> = HashMap::new();
    let add = |order: &mut Vec<ChunkId>,
               scores: &mut HashMap<ChunkId, (ArmScores, ArmRanks)>,
               list: Vec<(ChunkId, f32)>,
               is_dense: bool| {
        for (rank, (id, score)) in list.into_iter().enumerate() {
            let entry = scores.entry(id.clone()).or_insert_with(|| {
                order.push(id.clone());
                (ArmScores::default(), ArmRanks::default())
            });
            if is_dense {
                // The dense score is the ranking key; a single-arm caller sees
                // its own arm's score, which is the case that matters.
                entry.0.dense = Some(score);
                entry.0.fused = score;
                entry.1.dense = Some(rank as u32);
            } else {
                entry.0.sparse = Some(score);
                if entry.0.fused == 0.0 {
                    entry.0.fused = score;
                }
                entry.1.sparse = Some(rank as u32);
            }
        }
    };
    if let Some(list) = dense {
        add(&mut order, &mut scores, list, true);
    }
    if let Some(list) = sparse {
        add(&mut order, &mut scores, list, false);
    }
    order
        .into_iter()
        .map(|id| {
            let s = scores.remove(&id).unwrap_or_default();
            (id, s)
        })
        .collect()
}

fn candidate_of(unit: &IndexUnit, (scores, ranks): (ArmScores, ArmRanks)) -> Candidate {
    Candidate {
        unit_id: unit.id.clone(),
        document_id: unit.document_id.clone(),
        tenant: unit.tenant.clone(),
        chunk_index: unit.chunk_index,
        page: unit.page,
        chunk_type: unit.chunk_type,
        content: unit.content.clone(),
        contextual_prefix: unit.contextual_prefix.clone(),
        metadata: unit.metadata.clone(),
        scores,
        ranks,
    }
}

fn validate_dims(vector: &[f32], dims: u32, what: &str) -> Result<(), PortError> {
    if vector.is_empty() {
        return Err(PortError::InvalidArgument(format!(
            "{what} vector is empty"
        )));
    }
    if dims > 0 && vector.len() != dims as usize {
        return Err(PortError::InvalidArgument(format!(
            "{what} vector is {} wide, index expects {dims}",
            vector.len()
        )));
    }
    Ok(())
}

/// Cosine similarity. A zero vector scores 0 rather than NaN, so one unembedded
/// unit cannot poison a whole ranking.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..n {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    let score = dot / (na.sqrt() * nb.sqrt());
    if score.is_finite() { score } else { 0.0 }
}

/// BM25's `k1` is re-exported for adapters that want to document which
/// saturation their stored vectors used.
pub const SATURATION_K1: f32 = BM25_K1;
