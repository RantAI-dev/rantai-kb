//! Compressed adjacency (CSR) over a dense node numbering, plus the two graph
//! walks the retrieval graph arm needs: personalised PageRank and 1–2 hop
//! expansion.
//!
//! # Why a CSR in process
//!
//! `docs/architecture-target.md` §11: the entity graph lives in Postgres tables
//! and the traversal happens in Rust, in memory, with no network hop. A tenant's
//! graph is loaded once as a [`Csr`] — `offsets`/`targets`/`weights` — cached
//! behind a version counter that ingest bumps, and every query then costs
//! microseconds of pointer chasing instead of a round trip.
//!
//! Pure and allocation-explicit: no I/O, no async, no environment. The store
//! adapter decides where the rows come from; this module only walks them.
//!
//! # Nodes are dense `u32` ids
//!
//! Entity keys are strings. Mapping them to a dense `u32` is the adapter's job
//! ([`NodeMap`] is the tool), because the CSR needs `offsets[v]..offsets[v+1]`
//! to index directly. `u32` caps a tenant graph at 4.29e9 nodes, which is four
//! orders of magnitude above any corpus this service will see.
//!
//! # Weights
//!
//! An edge carries a non-negative weight: an extractor confidence, a mention
//! count, a co-occurrence count. It is used two ways, and the difference
//! matters:
//!
//! * *Rank weighting* (the default): outgoing weight mass is split in
//!   proportion to weights, so a strong edge carries more of the walk.
//! * *Structure weighting*: the neighbour's structure vector `s(v)` is used
//!   instead of its raw weight, i.e. `sum(w_u^out * s(w)) / sum(w_u^out)`. This
//!   is the HIPRAG formulation, where `s(v) = w_in(v) / out_deg(v)` — an
//!   entity is a good conduit when it is pointed at more than it points out.
//!
//! A weight of `0` is treated as an absent edge for traversal: a dangling
//! extraction with zero confidence should not become a graph path. Negative
//! weights are rejected at construction.
//!
//! # PageRank semantics (and why they are pinned here)
//!
//! [`ppr`] solves the personalised random-teleport problem
//!
//! ```text
//! r ← (1 - d)·s + d·Aᵀ D⁻¹ r          where Aᵀ D⁻¹ is row-stochastic on
//!                                             the *out*-edge weights
//! s = seed mass, normalised to sum 1
//! ```
//!
//! on the **undirected** projection of the stored edges: every directed edge
//! `(u, v)` is loaded as both `u → v` and `v → u`, merged with `w_max`. Two
//! reasons, and both are load-bearing:
//!
//! 1. An entity relation extracted from one sentence is evidence in both
//!    directions for retrieval ("who is related to X" is the same question as
//!    "what is X related to"), and HippoRAG2's `passages` graph is undirected.
//!    Traversal in a directed graph would let mass leak *out* of a component and
//!    never come back, so a fact that only appears as `A → B` would not rank
//!    B for a query about A.
//! 2. Making it explicit beats letting the adapter decide: two adapters that
//!    disagreed about direction would disagree about reachability, which is
//!    precisely the class of bug the GraphStore contract suite exists to catch.
//!
//! Absorbing nodes: a node with no usable out-edge distributes its teleport
//! mass back over the seed vector instead of dropping it, so total mass stays 1
//! and a dangling node does not silently delete probability.
//!
//! Determinism: power iteration in node-id order, then the result is sorted by
//! `(score desc, node id asc)`. Two runs on the same graph produce
//! bit-identical vectors.

use std::collections::HashMap;
use std::collections::HashSet;

/// Default teleport probability: the PageRank damping factor.
pub const DEFAULT_DAMPING: f32 = 0.85;
/// Default L1 convergence tolerance for the power iteration.
pub const DEFAULT_TOLERANCE: f32 = 1e-7;
/// Default iteration cap. 200 power iterations is far past convergence for any
/// graph this size; hitting it is a degenerate input, not a slow one.
pub const DEFAULT_MAX_ITERATIONS: usize = 200;
/// Default operation cap for [`Algorithm::BoundedPush`]. A push walk is not
/// bounded by a tolerance, so it needs its own cap; this one is generous enough
/// that a converged push stops on the epsilon floor rather than on the cap, and
/// the benchmark is what says whether it is generous enough.
pub const DEFAULT_MAX_PUSH_OPERATIONS: usize = 1_000_000;
/// Default push floor for [`Algorithm::BoundedPush`]: contributions below this
/// are dropped instead of propagated.
///
/// 1e-5 is where the truncation is invisible to a chunk ranking. PPR is used to
/// order chunks, so an error of `n * eps` in the node score — 100k nodes at 1e-5
/// is 1.0 in L1 over a score vector that sums to 1 — is already too coarse to be
/// useful; 1e-5 leaves three orders of magnitude of headroom on that bound and
/// still prunes the long tail that costs the iterations.
pub const DEFAULT_EPSILON: f32 = 1e-5;

/// Which walk answers a PPR request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Algorithm {
    /// Power iteration to a fixed point. Exact (to the tolerance), and its cost
    /// is `iterations x arcs` whether or not the mass is local.
    #[default]
    Exact,
    /// Andersen–Chung–Lang local push (2009), "Approximate Personalized
    /// PageRank": a frontier of nodes with unspent mass, each push scaled down
    /// by `alpha`, contributions below [`DEFAULT_EPSILON`] dropped.
    ///
    /// The L1 error against the exact vector is bounded by `epsilon`
    /// (Andersen, Chung & Lang, Theorem 3.2), and the cost is
    /// `O(alpha * epsilon^-1 * nnz)` in the worst case but in practice the
    /// frontier is the set of nodes whose score is above the floor — which on a
    /// power-law entity graph is a small neighbourhood of the seeds, not the whole
    /// graph. That difference is why it exists: the exact walk's cost does not
    /// shrink when the answer does.
    BoundedPush,
    /// Power iteration that drops nodes whose score falls below a floor relative
    /// to the current maximum, and never traverses an inactive node's arcs.
    ///
    /// The point is that an in-memory CSR makes a *dense* pass cheap (a 100k
    /// `f64` scan is tens of microseconds) while the arcs are the expensive part,
    /// so truncating the active set is what buys the time. Exact and push each
    /// waste one end of that trade-off: exact touches every arc every iteration
    /// regardless of how far the mass has spread, and push chases a frontier one
    /// node at a time with no dense pass to lean on.
    ///
    /// Cost is `iterations x active arcs`, so a query whose mass stays local is
    /// cheap and a query in a giant component still is not — the floor, not the
    /// data structure, is what bounds the work. The error is bounded per
    /// iteration by `n * floor` in L1, which is deliberately reported rather than
    /// assumed: the benchmark measures it against the exact walk.
    Truncated,
}

// HIPRAG's structure weighting (`s(v) = w_in(v) / out_deg(v)`, propagated instead
// of the raw edge weight) is deliberately *not* exposed as a knob on
// `WalkOptions`, and the reason is worth recording rather than shipping a dead
// option: the CSR is the undirected projection of the stored relations, so every
// node's in-edge weights are the same multiset as its out-edge weights and
// `s(v) == 1` identically. The variant would be a switch that changes nothing,
// which is worse than no switch, because a later reader would assume it means
// something. Making it real would mean carrying the *directed* in-weights
// alongside the CSR (4 bytes per node) and scoring an asymmetry that ingest
// mostly makes up anyway. If that is ever wanted it is a new field with its own
// benchmark, not a defaulted enum arm.

/// Options for [`ppr`] and [`expand`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkOptions {
    /// Teleport probability. 0.85 is the value the graph-RAG literature uses.
    pub damping: f32,
    /// Stop when the L1 change of the whole score vector drops below this.
    pub tolerance: f32,
    /// Hard cap on power iterations (`Exact`) or push operations (`BoundedPush`).
    pub max_iterations: usize,
    /// Which walk to run.
    pub algorithm: Algorithm,
    /// Push contribution floor for [`Algorithm::BoundedPush`], and the L1 error
    /// bound it implies. Ignored by `Exact`.
    pub epsilon: f32,
    /// Operation cap for [`Algorithm::BoundedPush`]. Ignored by `Exact`, which
    /// uses [`WalkOptions::max_iterations`].
    pub max_operations: usize,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            damping: DEFAULT_DAMPING,
            tolerance: DEFAULT_TOLERANCE,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            algorithm: Algorithm::default(),
            epsilon: DEFAULT_EPSILON,
            max_operations: DEFAULT_MAX_PUSH_OPERATIONS,
        }
    }
}

impl WalkOptions {
    /// Same walk, different damping factor.
    #[must_use]
    pub fn with_damping(mut self, damping: f32) -> Self {
        self.damping = damping;
        self
    }

    /// Same walk, different convergence tolerance.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: f32) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Same walk, different iteration cap.
    #[must_use]
    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    /// Truncated iteration with the given relative floor, instead of exact power
    /// iteration.
    #[must_use]
    pub fn with_truncated(mut self, floor: f32) -> Self {
        self.algorithm = Algorithm::Truncated;
        self.epsilon = floor;
        self
    }

    /// Local push with the given contribution floor, instead of exact power
    /// iteration.
    #[must_use]
    pub fn with_bounded_push(mut self, epsilon: f32) -> Self {
        self.algorithm = Algorithm::BoundedPush;
        self.epsilon = epsilon;
        self
    }

    /// Local push with its own operation cap.
    #[must_use]
    pub fn with_max_operations(mut self, max_operations: usize) -> Self {
        self.max_operations = max_operations;
        self
    }
}

/// One edge of the input edge list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    pub src: u32,
    pub dst: u32,
    /// Non-negative. `0` means "no usable edge" and is dropped at build time.
    pub weight: f32,
}

impl Edge {
    /// A directed edge of the given weight.
    #[must_use]
    pub const fn new(src: u32, dst: u32, weight: f32) -> Self {
        Self { src, dst, weight }
    }
}

/// Bidirectional mapping between dense `u32` node ids and entity keys.
///
/// The port speaks entity keys; the CSR speaks node ids; this is the only place
/// that translates, so neither the walk nor the adapter has to. An adapter that
/// loads a tenant's rows builds the map in key order, so the ids it assigns are
/// reproducible for the same row set — determinism of the walk depends on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeMap {
    keys: Vec<String>,
    by_key: HashMap<String, u32>,
}

impl NodeMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern a key, returning its stable dense id. Idempotent.
    ///
    /// The upper bound on a single id is [`u32::MAX`]; one more key than that
    /// cannot be represented in CSR offsets, so interning stops there.
    pub fn intern(&mut self, key: &str) -> Option<u32> {
        if let Some(&id) = self.by_key.get(key) {
            return Some(id);
        }
        let id = u32::try_from(self.keys.len()).ok()?;
        self.by_key.insert(key.to_string(), id);
        self.keys.push(key.to_string());
        Some(id)
    }

    /// Node count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True when nothing is interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Dense id of a key, if interned.
    #[must_use]
    pub fn id_of(&self, key: &str) -> Option<u32> {
        self.by_key.get(key).copied()
    }

    /// Key of a dense id, if in range.
    #[must_use]
    pub fn key_of(&self, id: u32) -> Option<&str> {
        self.keys.get(id as usize).map(String::as_str)
    }

    /// Every key in id order.
    #[must_use]
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// Every id in key order.
    pub fn ids(&self) -> impl Iterator<Item = u32> {
        0..self.keys.len() as u32
    }
}

/// A compressed-sparse-row adjacency over dense `u32` node ids.
///
/// Both directions of an edge are stored, so a walk treats the graph as
/// undirected: see the module docs for why.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Csr {
    /// `offsets.len() == nodes + 1`; the neighbours of `v` are
    /// `targets[offsets[v]..offsets[v + 1]]`.
    offsets: Vec<u32>,
    /// Neighbour ids, ascending within each node's slice.
    targets: Vec<u32>,
    /// Edge weights, parallel to `targets`. Always finite and positive.
    weights: Vec<f32>,
}

impl Csr {
    /// Build from a directed edge list over `node_count` nodes.
    ///
    /// Every edge becomes two arcs (`src → dst`, `dst → dst`), so callers pass
    /// the relation list exactly as the store holds it. Parallel edges between
    /// the same pair are merged with `w_max`, so a duplicated relation cannot
    /// inflate a neighbour's share by being counted twice. Self-loops are kept
    /// (they are real self-referential relations) but cannot teleport anywhere
    /// new.
    ///
    /// Nodes with no usable edges are kept as empty rows, so the numbering is
    /// dense and `id_of` stays valid.
    ///
    /// # Panics
    ///
    /// Panics when an edge references a node id `>= node_count`, or when
    /// `node_count` exceeds what the `u32` offsets can address, or when a weight
    /// is negative or NaN. A corrupt edge list is a programming error, not a
    /// runtime condition.
    #[must_use]
    pub fn from_edges(node_count: usize, edges: &[Edge]) -> Self {
        assert!(
            node_count < (u32::MAX as usize),
            "node_count must fit in u32 CSR offsets"
        );
        let mut arcs: Vec<(u32, u32, f32)> = Vec::with_capacity(edges.len() * 2);
        for edge in edges {
            assert!(
                (edge.src as usize) < node_count && (edge.dst as usize) < node_count,
                "edge {} -> {} is out of range for {node_count} nodes",
                edge.src,
                edge.dst
            );
            assert!(
                !edge.weight.is_nan() && edge.weight >= 0.0,
                "edge weights must be non-negative and finite, got {}",
                edge.weight
            );
            if edge.weight <= 0.0 {
                continue;
            }
            arcs.push((edge.src, edge.dst, edge.weight));
            if edge.src != edge.dst {
                arcs.push((edge.dst, edge.src, edge.weight));
            }
        }

        // Counting sort by source, then sort each slice by target. Both passes
        // are O(E + V); the per-slice sort is what makes the traversal order a
        // function of the node id alone rather than of the insertion order.
        let mut offsets = vec![0u32; node_count + 1];
        for &(src, _, _) in &arcs {
            offsets[src as usize + 1] += 1;
        }
        for i in 0..node_count {
            offsets[i + 1] += offsets[i];
        }
        let mut targets = vec![0u32; arcs.len()];
        let mut weights = vec![0f32; arcs.len()];
        let mut cursor: Vec<u32> = offsets[..node_count].to_vec();
        for &(src, dst, weight) in &arcs {
            let slot = cursor[src as usize] as usize;
            targets[slot] = dst;
            weights[slot] = weight;
            cursor[src as usize] += 1;
        }

        // Compact: sort each node's slice by target and collapse duplicates in
        // place, moving every group down to close the gaps. Both passes are
        // O(E + V), and in place because the cursor only ever lags the reads.
        let mut order: Vec<usize> = Vec::new();
        let mut scratch: Vec<(u32, f32)> = Vec::new();
        let mut out = 0usize;
        let mut compacted = vec![0u32; node_count + 1];
        for v in 0..node_count {
            let (start, end) = (offsets[v] as usize, offsets[v + 1] as usize);
            order.clear();
            order.extend(start..end);
            // Sort the permutation, not the target column: `weights` is a
            // parallel array, so sorting one without the other would silently
            // attach every weight to the wrong neighbour.
            order.sort_unstable_by_key(|&i| targets[i]);
            compacted[v] = out as u32;
            scratch.clear();
            scratch.extend(order.iter().map(|&i| (targets[i], weights[i])));
            out = merge_group(&mut targets, &mut weights, &scratch, out);
        }
        compacted[node_count] = out as u32;
        targets.truncate(out);
        weights.truncate(out);
        Self {
            offsets: compacted,
            targets,
            weights,
        }
    }

    /// Node count. A default-constructed CSR holds no nodes, so this
    /// saturates rather than underflowing on an empty `offsets`.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// Number of stored arcs. Undirected edges count twice.
    #[must_use]
    pub fn arc_count(&self) -> usize {
        self.targets.len()
    }

    /// Out-arcs of `v` as `(target, weight)` pairs, ascending by target.
    ///
    /// Allocates, so it is the readable form for callers and tests. The walks
    /// use [`Csr::neighbour_slices`] instead.
    #[must_use]
    pub fn neighbours(&self, v: u32) -> Vec<(u32, f32)> {
        if self.node_count() == 0 || (v as usize) >= self.node_count() {
            return Vec::new();
        }
        let start = self.offsets[v as usize] as usize;
        let end = self.offsets[v as usize + 1] as usize;
        let mut out = Vec::with_capacity(end - start);
        for i in start..end {
            out.push((self.targets[i], self.weights[i]));
        }
        out
    }

    /// Neighbours of `v` as parallel `(targets, weights)` slices, without
    /// allocating. The walk uses this; [`Csr::neighbours`] is the readable form
    /// for callers and tests.
    pub fn neighbour_slices(&self, v: u32) -> (&[u32], &[f32]) {
        if self.node_count() == 0 || (v as usize) >= self.node_count() {
            return (&[], &[]);
        }
        let start = self.offsets[v as usize] as usize;
        let end = self.offsets[v as usize + 1] as usize;
        (&self.targets[start..end], &self.weights[start..end])
    }

    /// Heap bytes the three arrays occupy. The 200 MB / 1M-edge budget from
    /// `docs/architecture-target.md` §11 is measured against this.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.offsets.len() * std::mem::size_of::<u32>()
            + self.targets.len() * std::mem::size_of::<u32>()
            + self.weights.len() * std::mem::size_of::<f32>()
    }

    /// A clustered synthetic graph: `clusters` communities, `intra_ratio` of the
    /// edges inside one and the rest between them.
    ///
    /// This is the shape a document-derived entity graph actually has: entities
    /// co-occur inside documents, so most relations are local, and a few bridge
    /// documents connect communities. [`Csr::power_law`] is the opposite
    /// assumption — one giant component, no structure — and it is the worst case
    /// for any walk whose cost is proportional to the mass it has to move, which
    /// is why both are measured rather than one being assumed.
    #[must_use]
    pub fn clustered(
        node_count: u32,
        edge_count: usize,
        clusters: u32,
        intra_ratio: f32,
        seed: u64,
    ) -> Self {
        assert!(
            clusters > 0 && clusters <= node_count,
            "clusters must be 1..=nodes"
        );
        let mut rng = Xorshift(seed);
        let per_cluster = (node_count / clusters).max(1);
        let intra = intra_ratio.clamp(0.0, 1.0);
        let mut edges = Vec::with_capacity(edge_count);
        for _ in 0..edge_count {
            let a = rng.below(node_count);
            let b = if rng.next_u64() % 10_000 < (intra * 10_000.0) as u64 {
                // Same community: same cluster index, uniform offset inside it.
                let base = (a / per_cluster) * per_cluster;
                let span = per_cluster.min(node_count - base);
                base + rng.below(span)
            } else {
                rng.below(node_count)
            };
            edges.push(Edge::new(a, b, 1.0));
        }
        Self::from_edges(node_count as usize, &edges)
    }

    /// A spread-out synthetic graph: `n` nodes, `2 * m / n` arcs each on
    /// average, attached preferentially to already-connected nodes so the degree
    /// distribution is power-law rather than uniform.
    ///
    /// Deterministic for a given seed, so a benchmark on it is comparable
    /// between runs and machines. Used by the 1M-edge benchmark; exposed because
    /// the contract of the walk is easier to test on a graph that has the
    /// skewed degree real entity graphs have than on a uniform one.
    #[must_use]
    pub fn power_law(node_count: u32, edge_count: usize, seed: u64) -> Self {
        let mut rng = Xorshift(seed);
        let mut edges = Vec::with_capacity(edge_count);
        // Preferential attachment: pick the first endpoint uniformly (so every
        // node can be a hub), the second from the existing pool with a bias
        // toward recently-used nodes.
        let mut pool: Vec<u32> = Vec::with_capacity(edge_count);
        for _ in 0..edge_count {
            let a = rng.below(node_count);
            let b = if pool.is_empty() {
                rng.below(node_count)
            } else {
                let biased = (rng.below(pool.len().min(4096) as u32) as usize) * 2;
                pool[biased.min(pool.len() - 1)]
            };
            pool.push(a);
            pool.push(b);
            edges.push(Edge::new(a, b, 1.0));
        }
        Self::from_edges(node_count as usize, &edges)
    }
}

/// Merge one node's arcs — `group` already sorted by target — into
/// `targets`/`weights` starting at `out`, and return the new write cursor.
///
/// Duplicates are the same neighbour reached by two stored relations; they
/// collapse with `w_max`, so a duplicated relation cannot inflate a neighbour's
/// share of the teleport mass by being counted twice.
///
/// The group is read from a snapshot rather than in place: the permutation that
/// sorts a group can be descending, so writing the smallest target first can land
/// on the slot a later read still needs — which silently drops arcs. The
/// snapshot is one node's arcs, so it is bounded by the maximum degree.
fn merge_group(
    targets: &mut [u32],
    weights: &mut [f32],
    group: &[(u32, f32)],
    out: usize,
) -> usize {
    let mut write = out;
    for &(target, weight) in group {
        if write > out && targets[write - 1] == target {
            if weight > weights[write - 1] {
                weights[write - 1] = weight;
            }
        } else {
            targets[write] = target;
            weights[write] = weight;
            write += 1;
        }
    }
    write
}

/// One node's expansion result: its id and the weight that reached it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoredNode {
    pub node: u32,
    pub score: f32,
}

/// One hop's expansion: the node, its own score and the edge weight it was
/// reached through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExpandedNode {
    pub node: u32,
    /// The score of the node this one was reached from.
    pub from_score: f32,
    /// Weight of the edge between the two.
    pub edge_weight: f32,
}

/// Personalised PageRank from a weighted seed set.
///
/// `seed_weights` are `[node, weight]` pairs; non-positive weights are dropped
/// and the rest are normalised to sum 1. A seed set that is empty after that
/// filtering returns an empty result rather than a uniform vector: "no seeds" is
/// a fact about the query, not a request for a graph-wide ranking.
///
/// Returns nodes with a non-zero score, sorted by `(score desc, node asc)`.
///
/// # Determinism
///
/// Power iteration accumulates in node-id order and the output is sorted with an
/// explicit tiebreak, so the same graph and seeds give the same bytes every run.
#[must_use]
pub fn ppr(csr: &Csr, seed_weights: &[(u32, f32)], opts: &WalkOptions) -> Vec<ScoredNode> {
    match opts.algorithm {
        Algorithm::Exact => ppr_inner(csr, seed_weights, opts).0,
        Algorithm::BoundedPush => ppr_push(csr, seed_weights, opts),
        Algorithm::Truncated => ppr_truncated(csr, seed_weights, opts).0,
    }
}

/// Truncated power iteration. Returns the ranking, the iteration count, and
/// whether it stopped on the tolerance or on an empty active set.
///
/// Each pass walks only the arcs of active nodes — the ones whose score is at
/// least `floor` times the current maximum — and then rebuilds the active set
/// from the new scores. A node that falls under the floor is dropped, and its
/// score with it, so the dropped mass is what the floor is buying.
fn ppr_truncated(
    csr: &Csr,
    seed_weights: &[(u32, f32)],
    opts: &WalkOptions,
) -> (Vec<ScoredNode>, usize, bool) {
    let n = csr.node_count();
    if n == 0 {
        return (Vec::new(), 0, true);
    }
    let Some(seeds) = normalise_seeds(seed_weights, n) else {
        return (Vec::new(), 0, true);
    };

    let damping = f64::from(opts.damping.clamp(0.0, 1.0));
    let one_minus_d = 1.0 - damping;
    let tolerance = f64::from(opts.tolerance);
    let floor = f64::from(opts.epsilon.max(0.0));

    let mut rank = seeds.clone();
    let mut next = vec![0f64; n];
    let mut active: Vec<u32> = (0..n as u32).filter(|&v| seeds[v as usize] > 0.0).collect();
    let mut next_active: Vec<u32> = Vec::with_capacity(n);
    let mut iterations = 0usize;

    for _ in 0..opts.max_iterations {
        iterations += 1;
        next.fill(0.0);
        for &v in &active {
            let u = v as usize;
            let r = rank[u];
            if r <= 0.0 {
                continue;
            }
            let (targets, weights) = csr.neighbour_slices(v);
            if targets.is_empty() {
                continue;
            }
            let total = arc_mass(weights);
            if total <= 0.0 {
                continue;
            }
            let share = r * damping / total;
            for (i, &t) in targets.iter().enumerate() {
                next[t as usize] += share * f64::from(weights[i]);
            }
        }
        // Absorbing nodes hand their teleport share back to the seed vector.
        for &v in &active {
            let u = v as usize;
            if csr.offsets[u + 1] == csr.offsets[u] && rank[u] > 0.0 {
                for (seed_node, seed_weight) in seeds.iter().enumerate() {
                    next[seed_node] += damping * rank[u] * seed_weight;
                }
            }
        }

        // One dense pass: blend the teleport term in, measure the step, and leave
        // `rank` holding the new values. Nodes outside the active set keep their
        // old value, which is below the floor by construction.
        let mut delta = 0.0f64;
        for node in 0..n {
            let value = one_minus_d * seeds[node] + next[node];
            delta += (value - rank[node]).abs();
            next[node] = value;
        }
        std::mem::swap(&mut rank, &mut next);

        // Rebuild the active set over the *whole* vector, not over the nodes
        // that happened to be active last pass: most of the mass after one
        // iteration sits on nodes that were not seeds, so a rebuild that only
        // looked at the previous active set would never pick them up and the walk
        // would stop after two passes. The scan is O(nodes) of dense `f64`
        // compares, which is cheap next to the arcs it saves.
        let peak = rank.iter().copied().fold(0.0f64, f64::max);
        next_active.clear();
        if peak > 0.0 {
            let threshold = floor * peak;
            for (node, &score) in rank.iter().enumerate() {
                if score >= threshold && score > 0.0 {
                    next_active.push(node as u32);
                }
            }
        }
        std::mem::swap(&mut active, &mut next_active);
        if active.is_empty() {
            break;
        }
        if delta < tolerance {
            break;
        }
    }

    // The pruned nodes still hold their last computed score, which is below the
    // floor. They are zeroed here so "not in the answer" and "score zero" are the
    // same thing, and the vector is rescaled so scores compare across queries.
    let total: f64 = rank.iter().sum();
    if total > 0.0 {
        for score in &mut rank {
            *score /= total;
        }
    }
    let converged = iterations < opts.max_iterations;
    let mut out: Vec<ScoredNode> = rank
        .iter()
        .enumerate()
        .filter(|&(_, &score)| score > 0.0)
        .map(|(i, &score)| ScoredNode {
            node: i as u32,
            score: score as f32,
        })
        .collect();
    sort_scored(&mut out);
    (out, iterations, converged)
}

/// Iterations a truncated walk needed, and whether it stayed inside the cap.
#[must_use]
pub fn ppr_truncated_iterations(
    csr: &Csr,
    seed_weights: &[(u32, f32)],
    opts: &WalkOptions,
) -> (usize, bool) {
    let (_, iterations, _) = ppr_truncated(csr, seed_weights, opts);
    (iterations, iterations < opts.max_iterations)
}

/// Andersen–Chung–Lang local push.
///
/// `q` is the frontier: mass that has been credited to a node but not yet spent.
/// A push takes all of `q[u]`, scales it by `alpha`, splits it across the arcs in
/// proportion to their weights, and skips any single contribution below
/// `epsilon`. Skipping is the whole point: it bounds the L1 error at `epsilon`
/// (Andersen, Chung & Lang 2009, Theorem 3.2) and, because a node whose every
/// outgoing contribution is below the floor never enters the frontier, it also
/// bounds how much of the graph is ever touched. On an entity graph the mass that
/// matters sits within a couple of hops of the seeds; the exact walk spends the
/// same work spreading `1e-30` of mass into a leaf ten hops away.
///
/// `p` starts empty and is credited as mass is pushed, so `p + q` conserves the
/// seed mass and the ε bound is about what is *missing* from `p`: every push
/// multiplies by `alpha`, so each hop keeps only `alpha` of what it carried and
/// the tail that falls under `epsilon` is simply dropped. The result is therefore
/// rescaled to sum 1 — a ranking is invariant under that, and the graph arm joins
/// scores across seeds, which are not comparable without it. The ε bound applies
/// to the pre-rescaling vector; the benchmark measures the rescaled one, which is
/// the honest comparison for a ranking.
///
/// The frontier is a LIFO stack with an explicit "in flight" flag rather than a
/// heap: both are deterministic, and the stack keeps the hot nodes close in memory
/// and needs no comparison per push.
#[must_use]
pub fn ppr_push(csr: &Csr, seed_weights: &[(u32, f32)], opts: &WalkOptions) -> Vec<ScoredNode> {
    let n = csr.node_count();
    if n == 0 {
        return Vec::new();
    }
    let Some(seeds) = normalise_seeds(seed_weights, n) else {
        return Vec::new();
    };
    let alpha = f64::from(opts.damping.clamp(0.0, 1.0));
    let epsilon = f64::from(opts.epsilon.max(0.0));

    // `p` is the answer and starts empty: mass moves seed -> q -> p as it is
    // pushed. Seeding `p` with the seeds as well would count it twice, and the
    // only symptom would be a score vector that sums to more than 1.
    let mut p = vec![0f64; n];
    let mut q = vec![0f64; n];
    let mut frontier: Vec<u32> = Vec::new();
    let mut queued = vec![false; n];
    // Seeds are the starting frontier. Ascending so the first pushes are in node
    // order, which is what makes the arithmetic reproducible.
    for (node, &seed) in seeds.iter().enumerate() {
        if seed > 0.0 {
            q[node] = seed;
            frontier.push(node as u32);
            queued[node] = true;
        }
    }

    let mut operations = 0usize;
    while let Some(u) = frontier.pop() {
        let u = u as usize;
        queued[u] = false;
        let residual = q[u];
        q[u] = 0.0;
        if residual <= 0.0 {
            continue;
        }
        operations += 1;
        if operations > opts.max_operations {
            break;
        }
        let (targets, weights) = csr.neighbour_slices(u as u32);
        if targets.is_empty() {
            continue;
        }
        let total = arc_mass(weights);
        if total <= 0.0 {
            continue;
        }
        let scaled = residual * alpha / total;
        for (i, &t) in targets.iter().enumerate() {
            let contribution = scaled * f64::from(weights[i]);
            if contribution < epsilon {
                continue;
            }
            p[t as usize] += contribution;
            q[t as usize] += contribution;
            if !queued[t as usize] {
                queued[t as usize] = true;
                frontier.push(t);
            }
        }
    }

    let total: f64 = p.iter().sum();
    if total > 0.0 {
        for score in &mut p {
            *score /= total;
        }
    }

    let mut out: Vec<ScoredNode> = p
        .iter()
        .enumerate()
        .filter(|&(_, &score)| score > 0.0)
        .map(|(i, &score)| ScoredNode {
            node: i as u32,
            score: score as f32,
        })
        .collect();
    sort_scored(&mut out);
    out
}

/// Number of push operations a local push needed, and whether it stayed inside the
/// cap. The push analogue of [`ppr_iterations`].
#[must_use]
pub fn ppr_push_operations(
    csr: &Csr,
    seed_weights: &[(u32, f32)],
    opts: &WalkOptions,
) -> (usize, bool) {
    let n = csr.node_count();
    if n == 0 {
        return (0, true);
    }
    let Some(seeds) = normalise_seeds(seed_weights, n) else {
        return (0, true);
    };
    let alpha = f64::from(opts.damping.clamp(0.0, 1.0));
    let epsilon = f64::from(opts.epsilon.max(0.0));
    let mut q = vec![0f64; n];
    let mut frontier: Vec<u32> = Vec::new();
    let mut queued = vec![false; n];
    for (node, &seed) in seeds.iter().enumerate() {
        if seed > 0.0 {
            q[node] = seed;
            frontier.push(node as u32);
            queued[node] = true;
        }
    }
    let mut operations = 0usize;
    while let Some(u) = frontier.pop() {
        let u = u as usize;
        queued[u] = false;
        let residual = q[u];
        q[u] = 0.0;
        if residual <= 0.0 {
            continue;
        }
        operations += 1;
        if operations > opts.max_operations {
            return (operations, false);
        }
        let (targets, weights) = csr.neighbour_slices(u as u32);
        if targets.is_empty() {
            continue;
        }
        let total = arc_mass(weights);
        if total <= 0.0 {
            continue;
        }
        let scaled = residual * alpha / total;
        for (i, &t) in targets.iter().enumerate() {
            let contribution = scaled * f64::from(weights[i]);
            if contribution < epsilon {
                continue;
            }
            q[t as usize] += contribution;
            if !queued[t as usize] {
                queued[t as usize] = true;
                frontier.push(t);
            }
        }
    }
    (operations, true)
}

/// How many power iterations the walk needed, and whether it converged.
///
/// Not returned by [`ppr`] itself — it would change the signature for a
/// diagnostic — but the benchmark prints it, and it is how a caller tells "this
/// graph needed 40 iterations" from "this graph did not converge and the numbers
/// are a truncated iterate".
#[must_use]
pub fn ppr_iterations(csr: &Csr, seed_weights: &[(u32, f32)], opts: &WalkOptions) -> (usize, bool) {
    let (_, iterations, converged) = ppr_inner(csr, seed_weights, opts);
    (iterations, converged)
}

/// The walk itself: ranking, iteration count, convergence flag.
fn ppr_inner(
    csr: &Csr,
    seed_weights: &[(u32, f32)],
    opts: &WalkOptions,
) -> (Vec<ScoredNode>, usize, bool) {
    let n = csr.node_count();
    if n == 0 {
        return (Vec::new(), 0, true);
    }
    let Some(seeds) = normalise_seeds(seed_weights, n) else {
        return (Vec::new(), 0, true);
    };

    let damping = f64::from(opts.damping.clamp(0.0, 1.0));
    let one_minus_d = 1.0 - damping;
    let tolerance = f64::from(opts.tolerance);

    let mut rank = seeds.clone();
    let mut next = vec![0f64; n];
    let mut iterations = 0usize;
    let mut converged = false;
    for _ in 0..opts.max_iterations {
        iterations += 1;
        next.fill(0.0);
        for (v, &r) in rank.iter().enumerate() {
            if r <= 0.0 || csr.offsets[v + 1] == csr.offsets[v] {
                continue;
            }
            let (targets, weights) = csr.neighbour_slices(v as u32);
            let total = arc_mass(weights);
            if total <= 0.0 {
                continue;
            }
            let share = r * damping / total;
            for (i, &t) in targets.iter().enumerate() {
                next[t as usize] += share * f64::from(weights[i]);
            }
        }
        // Absorbing nodes return their teleport share to the seed vector, so
        // mass is conserved instead of quietly evaporating.
        for (v, &r) in rank.iter().enumerate() {
            if csr.offsets[v + 1] == csr.offsets[v] && r > 0.0 {
                for (seed_node, seed_weight) in seeds.iter().enumerate() {
                    next[seed_node] += damping * r * seed_weight;
                }
            }
        }
        let mut delta = 0.0f64;
        for v in 0..n {
            let value = one_minus_d * seeds[v] + next[v];
            delta += (value - rank[v]).abs();
            next[v] = value;
        }
        std::mem::swap(&mut rank, &mut next);
        if delta < tolerance {
            converged = true;
            break;
        }
    }

    let mut out: Vec<ScoredNode> = rank
        .iter()
        .enumerate()
        .filter(|&(_, &s)| s > 0.0)
        .map(|(i, &s)| ScoredNode {
            node: i as u32,
            score: s as f32,
        })
        .collect();
    sort_scored(&mut out);
    (out, iterations, converged)
}

/// Split one node's teleport mass over its arcs in proportion to their weights.
fn arc_mass(weights: &[f32]) -> f64 {
    weights.iter().map(|&w| f64::from(w)).sum()
}

/// Normalise seed weights over `n` nodes, dropping non-positive entries and
/// ids out of range. `None` when nothing usable is left.
fn normalise_seeds(seed_weights: &[(u32, f32)], n: usize) -> Option<Vec<f64>> {
    let mut seeds = vec![0f64; n];
    let mut total = 0f64;
    for &(node, weight) in seed_weights {
        let w = f64::from(weight);
        if !w.is_finite() || w <= 0.0 || (node as usize) >= n {
            continue;
        }
        seeds[node as usize] += w;
        total += w;
    }
    if total <= 0.0 {
        return None;
    }
    for s in &mut seeds {
        *s /= total;
    }
    Some(seeds)
}

/// Sort by score descending, then node ascending. The tiebreak is what makes
/// the output a function of the graph and not of `sort_by`'s stability.
fn sort_scored(items: &mut [ScoredNode]) {
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.node.cmp(&b.node))
    });
}

/// 1–2 hop expansion from a weighted seed set.
///
/// Depth 1 returns each seed's direct neighbours; depth 2 adds the neighbours of
/// those. Every returned entry carries the seed score it was reached from and the
/// edge weight, so the caller can score chunks without re-deriving either.
///
/// The walk is breadth-first and level-order, so `from_score` is always the score
/// of a node in the level above: a caller can turn the expansion into a
/// chunk ranking without tracking provenance separately.
///
/// Seeds are always included in the result (at depth ≥ 1), so a seed with no
/// edges is still returned — it is a real fact about the query, and dropping it
/// would make "this entity is in scope but unconnected" indistinguishable from
/// "this entity is unknown".
///
/// # Depth is clamped to 2
///
/// Deeper expansion on a real entity graph is a flood, not a signal: degree
/// distribution is power-law, so depth 3 from one hub reaches most of the graph.
/// A caller that wants more than two hops gets it from [`ppr`], which decays mass
/// instead of exploding.
#[must_use]
pub fn expand(csr: &Csr, seed_weights: &[(u32, f32)], depth: usize) -> Vec<ExpandedNode> {
    let n = csr.node_count();
    if n == 0 {
        return Vec::new();
    }
    let depth = depth.clamp(1, 2);
    let mut seed_map: HashMap<u32, f32> = HashMap::new();
    for &(node, weight) in seed_weights {
        if (node as usize) < n && weight.is_finite() && weight > 0.0 {
            let slot = seed_map.entry(node).or_insert(0.0);
            *slot += weight;
        }
    }

    let mut seen: HashSet<u32> = HashSet::new();
    let mut frontier: Vec<(u32, f32)> = Vec::new();
    let mut out: Vec<ExpandedNode> = Vec::new();
    // Deterministic level order: seeds sorted by id.
    let mut seeds_sorted: Vec<(u32, f32)> = seed_map.into_iter().collect();
    seeds_sorted.sort_unstable_by_key(|(id, _)| *id);
    for (id, score) in &seeds_sorted {
        if seen.insert(*id) {
            frontier.push((*id, *score));
        }
    }

    for _ in 0..depth {
        let mut next_level: Vec<(u32, f32)> = Vec::new();
        for &(from, from_score) in &frontier {
            let (targets, weights) = csr.neighbour_slices(from);
            for (i, &t) in targets.iter().enumerate() {
                out.push(ExpandedNode {
                    node: t,
                    from_score,
                    edge_weight: weights[i],
                });
                if seen.insert(t) {
                    next_level.push((t, from_score));
                }
            }
        }
        next_level.sort_unstable_by_key(|(id, _)| *id);
        frontier = next_level;
        if frontier.is_empty() {
            break;
        }
    }
    out
}

/// Distinct nodes reached by [`expand`], in deterministic order (score of the
/// reaching seed descending, then node id ascending). This is the shape the
/// graph retrieval arm consumes: a ranked node list to join against mentions.
#[must_use]
pub fn expansion_ranking(expanded: &[ExpandedNode]) -> Vec<ScoredNode> {
    let mut best: HashMap<u32, f32> = HashMap::new();
    for item in expanded {
        let score = item.from_score * item.edge_weight;
        best.entry(item.node)
            .and_modify(|current| {
                if score > *current {
                    *current = score;
                }
            })
            .or_insert(score);
    }
    let mut out: Vec<ScoredNode> = best
        .into_iter()
        .map(|(node, score)| ScoredNode { node, score })
        .collect();
    sort_scored(&mut out);
    out
}

/// A tiny deterministic xorshift64*, so the synthetic graph and every benchmark
/// figure are reproducible on every machine.
#[derive(Debug, Clone)]
pub struct Xorshift(pub u64);

impl Xorshift {
    /// Next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A value in `0..bound`, by modulo. Slight modulo bias, which is irrelevant
    /// for a synthetic graph and cheaper than the rejection loop.
    pub fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % u64::from(bound)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scores(nodes: &[ScoredNode]) -> Vec<(u32, f32)> {
        nodes.iter().map(|n| (n.node, n.score)).collect()
    }

    fn approx(expected: &[(u32, f32)], actual: &[(u32, f32)], eps: f32) {
        assert_eq!(
            expected.len(),
            actual.len(),
            "length differs: expected {expected:?}, got {actual:?}"
        );
        for (e, a) in expected.iter().zip(actual.iter()) {
            assert_eq!(e.0, a.0, "node order differs: {expected:?} vs {actual:?}");
            assert!(
                (e.1 - a.1).abs() < eps,
                "node {}: expected {}, got {} (eps {eps})",
                e.0,
                e.1,
                a.1
            );
        }
    }

    // Unit-weight paths, seeded at node 0, d = 0.85, undirected traversal.
    //
    // The 3-node path 0-1-2 solves in closed form, and the derivation is worth
    // writing down because it is the whole semantics in four lines:
    //
    //   r0 = (1-d)        + d * (r1 / 2)   # only node 1 reaches 0, with prob 1/2
    //   r1 = d * (r0 + r2)                  # nodes 0 and 2 have degree 1
    //   r2 = d * (r1 / 2)
    //   =>  r1 (1 - d^2) = d (1 - d)  =>  r1 = d / (1 + d) = 17/37
    //   =>  r0 = 511/1480 = 0.34527027…, r1 = 0.45945945…, r2 = 289/1480 = 0.19527027…
    //
    // Note r1 > r0: the middle of a path is the best conduit (highest in-degree
    // per out-edge) and the seed's own teleport mass is only 0.15, so a
    // one-walk-from-seed node can outrank the seed. That is the intended
    // behaviour of PageRank on a chain, and it is why the graph arm joins against
    // mentions instead of returning nodes as hits.
    #[test]
    fn ppr_on_a_three_node_path_matches_the_hand_computation() {
        let csr = Csr::from_edges(3, &[Edge::new(0, 1, 1.0), Edge::new(1, 2, 1.0)]);
        let out = ppr(&csr, &[(0, 1.0)], &WalkOptions::default());
        approx(
            &[(1, 0.459_459_45), (0, 0.345_270_27), (2, 0.195_270_27)],
            &scores(&out),
            1e-5,
        );
        let total: f32 = out.iter().map(|n| n.score).sum();
        assert!((total - 1.0).abs() < 1e-4, "mass must stay 1, got {total}");
    }

    #[test]
    fn ppr_on_a_four_node_path_matches_the_exact_solve() {
        let csr = Csr::from_edges(
            4,
            &[
                Edge::new(0, 1, 1.0),
                Edge::new(1, 2, 1.0),
                Edge::new(2, 3, 1.0),
            ],
        );
        let out = ppr(&csr, &[(0, 1.0)], &WalkOptions::default());
        assert_eq!(
            scores(&out).iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![1, 0, 2, 3],
            "every node of the path is reachable, and node 1 outranks the seed \
             because it is the best conduit on a chain"
        );
        let s: Vec<f32> = out.iter().map(|n| n.score).collect();
        // Same system as above with four nodes:
        //   r0 = (1-d) + d (r1/2), r1 = d (r0 + r2/2), r2 = d (r1/2 + r3/2), r3 = d (r2/2)
        // Solving exactly (Gaussian elimination over the rationals) gives
        //   14660/48507 = 0.30222442…, 17374/48507 = 0.35817511…,
        //   11560/48507 = 0.23831612…,  4913/48507 = 0.10128435…
        approx(
            &[
                (1, 0.358_175_1),
                (0, 0.302_224_42),
                (2, 0.238_316_12),
                (3, 0.101_284_35),
            ],
            &scores(&out),
            1e-5,
        );
        let total: f32 = s.iter().sum();
        assert!((total - 1.0).abs() < 1e-4, "mass must stay 1, got {total}");
    }

    #[test]
    fn ppr_converges_on_the_default_tolerance() {
        let csr = Csr::power_law(200, 800, 0x5eed);
        let (iterations, converged) =
            ppr_iterations(&csr, &[(7, 1.0), (19, 0.5)], &WalkOptions::default());
        assert!(converged, "power iteration must converge on 200 nodes");
        assert!(
            iterations < DEFAULT_MAX_ITERATIONS,
            "converged in {iterations} iterations"
        );
    }

    #[test]
    fn seeds_with_no_edges_keep_their_own_mass() {
        let csr = Csr::from_edges(3, &[Edge::new(0, 1, 1.0)]);
        let out = ppr(&csr, &[(2, 1.0)], &WalkOptions::default());
        assert_eq!(scores(&out), vec![(2, 1.0)]);
    }

    #[test]
    fn a_disconnected_component_keeps_its_mass_to_itself() {
        // Two disjoint triangles: 0-1-2 and 3-4-5. Seeded at 0 only.
        let csr = Csr::from_edges(
            6,
            &[
                Edge::new(0, 1, 1.0),
                Edge::new(1, 2, 1.0),
                Edge::new(3, 4, 1.0),
                Edge::new(4, 5, 1.0),
            ],
        );
        let out = ppr(&csr, &[(0, 1.0)], &WalkOptions::default());
        let mut ids: Vec<u32> = out.iter().map(|n| n.node).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1, 2], "no mass may cross components");
        let total: f32 = out.iter().map(|n| n.score).sum();
        assert!((total - 1.0).abs() < 1e-4, "mass must stay 1, got {total}");
    }

    #[test]
    fn ppr_is_deterministic_across_runs_and_orderings() {
        let edges = [
            Edge::new(0, 1, 1.0),
            Edge::new(1, 2, 0.5),
            Edge::new(2, 3, 2.0),
            Edge::new(3, 0, 1.0),
        ];
        let a = Csr::from_edges(4, &edges);
        let mut shuffled = edges;
        shuffled.swap(0, 3);
        let b = Csr::from_edges(4, &shuffled);
        let seeds = [(2u32, 1.0f32), (0, 0.25)];
        let first = ppr(&a, &seeds, &WalkOptions::default());
        let second = ppr(&b, &seeds, &WalkOptions::default());
        assert_eq!(first, second, "edge order must not change the result");
        let third = ppr(&a, &seeds, &WalkOptions::default());
        assert_eq!(first, third, "repeated runs must be identical");
    }

    #[test]
    fn ties_break_on_node_id_not_insertion_order() {
        // A star: three identical spokes, so 1, 2 and 3 tie exactly.
        let csr = Csr::from_edges(
            4,
            &[
                Edge::new(0, 1, 1.0),
                Edge::new(0, 2, 1.0),
                Edge::new(0, 3, 1.0),
            ],
        );
        let out = ppr(&csr, &[(0, 1.0)], &WalkOptions::default());
        let leaves: Vec<f32> = out
            .iter()
            .filter(|n| n.node != 0)
            .map(|n| n.score)
            .collect();
        assert_eq!(leaves.len(), 3);
        assert!(
            (leaves[0] - leaves[1]).abs() < 1e-7 && (leaves[1] - leaves[2]).abs() < 1e-7,
            "spokes must tie, got {leaves:?}"
        );
        assert_eq!(
            scores(&out).iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn zero_and_negative_weights_are_refused_or_dropped() {
        // Zero weight: no arc at all.
        let csr = Csr::from_edges(2, &[Edge::new(0, 1, 0.0)]);
        assert_eq!(csr.arc_count(), 0);
        assert!(csr.neighbours(0).is_empty());
        assert_eq!(ppr(&csr, &[(0, 1.0)], &WalkOptions::default()).len(), 1);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn an_out_of_range_edge_panics() {
        let _ = Csr::from_edges(2, &[Edge::new(0, 5, 1.0)]);
    }

    #[test]
    #[should_panic(expected = "non-negative")]
    fn a_negative_weight_panics() {
        let _ = Csr::from_edges(2, &[Edge::new(0, 1, -1.0)]);
    }

    #[test]
    fn parallel_edges_merge_with_the_largest_weight() {
        let csr = Csr::from_edges(
            2,
            &[
                Edge::new(0, 1, 1.0),
                Edge::new(1, 0, 3.0),
                Edge::new(0, 1, 2.0),
            ],
        );
        assert_eq!(csr.arc_count(), 2, "two nodes, two arcs, three edges");
        assert_eq!(csr.neighbours(0), &[(1, 3.0)]);
        assert_eq!(csr.neighbours(1), &[(0, 3.0)]);
    }

    #[test]
    fn expansion_depth_two_is_breadth_first_and_level_ordered() {
        // 0 - 1 - 2 - 3, seeded at 0.
        let csr = Csr::from_edges(
            4,
            &[
                Edge::new(0, 1, 1.0),
                Edge::new(1, 2, 1.0),
                Edge::new(2, 3, 1.0),
            ],
        );
        let one = expand(&csr, &[(0, 1.0)], 1);
        assert_eq!(
            one.iter().map(|e| e.node).collect::<Vec<_>>(),
            vec![1],
            "one hop from an endpoint is one node"
        );
        let two = expand(&csr, &[(0, 1.0)], 2);
        let ids: Vec<u32> = two.iter().map(|e| e.node).collect();
        // Node 0 comes back at level 2 through the undirected back-edge. The walk
        // does not filter the seed out: a caller wants the seed's own mentions,
        // and dropping it would make "connected to nothing" indistinguishable
        // from "unknown".
        assert_eq!(
            ids,
            vec![1, 0, 2],
            "two hops, level order: 1 then its neighbours"
        );
        assert!(two.iter().all(|e| e.from_score == 1.0));
    }

    #[test]
    fn expansion_reaches_every_neighbour_of_a_star_once() {
        let csr = Csr::from_edges(
            4,
            &[
                Edge::new(0, 1, 1.0),
                Edge::new(0, 2, 1.0),
                Edge::new(0, 3, 1.0),
            ],
        );
        let out = expand(&csr, &[(0, 1.0)], 1);
        let ids: Vec<u32> = out.iter().map(|e| e.node).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        let ranked = expansion_ranking(&out);
        assert_eq!(
            scores(&ranked),
            vec![(1, 1.0), (2, 1.0), (3, 1.0)],
            "identical spokes rank by id"
        );
    }

    #[test]
    fn expansion_depth_is_clamped_to_two() {
        let csr = Csr::from_edges(
            6,
            &[
                Edge::new(0, 1, 1.0),
                Edge::new(1, 2, 1.0),
                Edge::new(2, 3, 1.0),
                Edge::new(3, 4, 1.0),
                Edge::new(4, 5, 1.0),
            ],
        );
        let deep = expand(&csr, &[(0, 1.0)], 9);
        let ids: Vec<u32> = deep.iter().map(|e| e.node).collect();
        assert_eq!(
            ids,
            vec![1, 0, 2],
            "depth 9 must still stop at two hops (1, then its neighbours)"
        );
        assert!(!ids.contains(&3), "three hops would reach node 3");
    }

    #[test]
    fn expansion_from_an_isolated_seed_is_empty() {
        let csr = Csr::from_edges(3, &[Edge::new(0, 1, 1.0)]);
        assert!(expand(&csr, &[(2, 1.0)], 2).is_empty());
    }

    #[test]
    fn expansion_ranking_keeps_the_best_score_per_node() {
        let expanded = vec![
            ExpandedNode {
                node: 5,
                from_score: 0.2,
                edge_weight: 1.0,
            },
            ExpandedNode {
                node: 5,
                from_score: 0.9,
                edge_weight: 1.0,
            },
            ExpandedNode {
                node: 6,
                from_score: 0.4,
                edge_weight: 1.0,
            },
        ];
        let ranked = expansion_ranking(&expanded);
        assert_eq!(scores(&ranked), vec![(5, 0.9), (6, 0.4)]);
    }

    #[test]
    fn node_map_round_trips_and_is_idempotent() {
        let mut map = NodeMap::new();
        assert_eq!(map.intern("candy"), Some(0));
        assert_eq!(map.intern("candi"), Some(1));
        assert_eq!(map.intern("candy"), Some(0));
        assert_eq!(map.len(), 2);
        assert_eq!(map.id_of("candi"), Some(1));
        assert_eq!(map.key_of(1), Some("candi"));
        assert_eq!(map.key_of(9), None);
        assert_eq!(map.ids().collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn empty_inputs_are_empty_outputs() {
        let csr = Csr::default();
        assert!(ppr(&csr, &[(0, 1.0)], &WalkOptions::default()).is_empty());
        assert!(expand(&csr, &[(0, 1.0)], 2).is_empty());
        assert_eq!(csr.node_count(), 0);
        let one_node = Csr::from_edges(1, &[]);
        assert!(ppr(&one_node, &[], &WalkOptions::default()).is_empty());
    }

    #[test]
    fn csr_keeps_the_node_counting_dense() {
        let csr = Csr::from_edges(1_000, &[Edge::new(10, 900, 1.0)]);
        assert_eq!(csr.node_count(), 1_000);
        assert_eq!(csr.arc_count(), 2);
        assert!(csr.neighbours(500).is_empty(), "isolated nodes stay rows");
        assert_eq!(csr.neighbours(10), &[(900, 1.0)]);
        // offsets.len() == nodes + 1
        assert_eq!(csr.offsets.len(), 1_001);
    }

    #[test]
    fn self_loops_are_kept_and_do_not_teleport() {
        let csr = Csr::from_edges(2, &[Edge::new(0, 0, 1.0), Edge::new(0, 1, 1.0)]);
        assert_eq!(csr.neighbours(0), &[(0, 1.0), (1, 1.0)]);
        let out = ppr(&csr, &[(0, 1.0)], &WalkOptions::default());
        let total: f32 = out.iter().map(|n| n.score).sum();
        assert!((total - 1.0).abs() < 1e-4, "mass must stay 1, got {total}");
    }
}
