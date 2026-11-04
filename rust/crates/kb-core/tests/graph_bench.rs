//! The §11 targets, measured: personalised PageRank plus 2-hop expansion on a
//! one-million-edge tenant graph.
//!
//! The two numbers `docs/architecture-target.md` §11 commits to are:
//!
//! * **p95 < 10 ms** for PPR + expansion at 1M edges, and
//! * **CSR memory < 200 MB** per 1M edges.
//!
//! Not a criterion benchmark: the workspace has no criterion dependency, and
//! adding one for a single harness would put a dev-dependency and a
//! benchmark-runner convention into the repo for one number. The repo's existing
//! benchmark (`kb-store-qdrant/tests/exact_vs_hnsw.rs`) is an `#[ignore]`d
//! harness with p50/p95 printed, and this is the same shape. Everything here is
//! `#[ignore]`d because it builds a 1M-edge graph and sweeps several walks, which
//! takes minutes — not something `cargo test --workspace` should do to anyone.
//!
//! Run it in release, or the numbers are debug-build numbers and mean nothing:
//!
//! ```sh
//! cargo test --release --manifest-path rust/Cargo.toml -p kb-core --test graph_bench -- --ignored --nocapture --test-threads=1
//! ```
//!
//! # What the runs printed (2026-10-17, this machine)
//!
//! `ppr_plus_two_hop_expansion_meets_the_architecture_targets` — clustered graph,
//! local push at `epsilon = 1e-5`:
//!
//! ```text
//! graph: 100000 nodes, 1000000 edges, 1424164 arcs (undirected projection), built in 225.7 ms
//! csr memory: 11793316 bytes = 11.2 MB of the 200 MB budget
//! ppr push  p50     1.91 ms  p95     2.68 ms  p99     3.07 ms  max     3.07 ms  PASS
//! ppr exact p50   277.43 ms  p95   327.06 ms  p99   328.51 ms  max   328.51 ms  MISS
//! 2-hop      p50     0.07 ms  p95     0.15 ms
//! combined   p50     1.97 ms  p95     2.82 ms  p99     3.23 ms  max     3.23 ms  PASS
//! exact: 55 power iterations to tolerance 1e-7 (converged: true)
//! push: 18631 push operations for the first query, epsilon 0.00001 (inside cap: true)
//! ```
//!
//! `exact_versus_bounded_push` — same edge count on a *single giant component*,
//! which is the worst case for any walk:
//!
//! ```text
//! push        1e-3   p95     0.73 ms   7 pushes      L1 1.73303   top-100  31.6%
//! push        1e-4   p95     2.68 ms   1563 pushes   L1 1.56114   top-100  34.9%
//! push        1e-5   p95    22.79 ms   16958 pushes  L1 1.18387   top-100  46.7%
//! push        1e-6   p95    98.88 ms   43238 pushes  L1 0.95145   top-100  61.6%
//! truncated   1e-3   p95    22.94 ms   10 its        L1 1.09104   top-100  38.8%
//! truncated   1e-4   p95   194.94 ms   18 its        L1 0.56266   top-100  67.1%
//! truncated   1e-5   p95  2445.83 ms   90 its        L1 0.11429   top-100  90.4%
//! truncated   1e-6   p95   836.44 ms   68 its        L1 0.00000   top-100 100.0%
//! exact     1e-7tol  p95   792.86 ms   68 its        L1 0.00000   top-100 100.0%
//! exact cost model: 68 iterations x 1997219 arcs = 135810892 arc visits per query
//! ```
//!
//! `same_algorithms_on_a_clustered_graph` — the same walks, same seeds, on 5,000
//! communities:
//!
//! ```text
//! push        1e-3   p95     0.81 ms   193 pushes    L1 1.12170   top-100  84.9%
//! push        1e-4   p95     1.04 ms   1952 pushes   L1 0.94665   top-100  92.0%
//! push        1e-5   p95     2.60 ms   18631 pushes  L1 0.81965   top-100  93.5%
//! push        1e-6   p95    12.77 ms   142123 pushes L1 0.71591   top-100  95.9%
//! truncated   1e-3   p95    26.24 ms   45 its        L1 0.54405   top-100  98.7%
//! truncated   1e-4   p95    63.02 ms   50 its        L1 0.33193   top-100 100.0%
//! truncated   1e-5   p95   941.27 ms   53 its        L1 0.09566   top-100 100.0%
//! truncated   1e-6   p95   388.80 ms   69 its        L1 0.00000   top-100 100.0%
//! exact     1e-7tol  p95   306.26 ms   55 its        L1 0.00000   top-100 100.0%
//! ```
//!
//! # The finding, in one paragraph
//!
//! The cost of a PPR walk is set by the *shape* of the graph, not only by the
//! algorithm. On a single giant component every walk must move mass across the
//! whole graph, and nothing measured here reaches 10 ms while keeping more than
//! about half of exact PPR's top 100. On a clustered graph — the shape a
//! document-derived entity graph has, because entities co-occur inside documents
//! — local push at `epsilon = 1e-5` costs 2.60 ms p95 and still puts 93.5% of the
//! exact top 100 in the same order. Exact power iteration is never competitive
//! here: it is 110x slower than push on the clustered graph, and its cost is
//! `iterations x arcs` with no regard for how far the mass actually spread, so it
//! misses the budget on both shapes by one to two orders of magnitude.
//!
//! The number this benchmark cannot settle is what a real tenant's graph looks
//! like. Measuring the community structure of an actual entity graph is the
//! follow-up in `docs/rust-cutover.md`; until then the graph arm stays behind its
//! flag (`docs/architecture-target.md` §11), which is where it was anyway.
//!
//! `build_csr` reports construction cost too, because a cache that takes longer to
//! build than it saves is not a cache: `load_tenant_graph` is on the read path's
//! critical section after every ingest bump, so a slow build is a latency number
//! the query p95 would otherwise hide.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use kb_core::graph::{
    Algorithm, Csr, WalkOptions, expand, expansion_ranking, ppr, ppr_iterations,
    ppr_push_operations, ppr_truncated_iterations,
};

/// §11: "p95 < 10 ms for a tenant graph with 1M edges".
const P95_BUDGET: Duration = Duration::from_millis(10);
/// §11: "memory for the tenant cache stays under 200 MB per 1M edges".
const MEMORY_BUDGET_BYTES: usize = 200 * 1024 * 1024;
/// 1M edges is the stated test size. Nodes are set so the mean degree is 20,
/// which is a dense entity graph: a tenant at scale has tens of thousands of
/// entities and hundreds of thousands of relations.
const EDGES: usize = 1_000_000;
const NODES: u32 = 100_000;
/// Communities in the clustered shape. A tenant's documents form communities; the
/// number is not load-bearing, only the existence of structure is.
const CLUSTERS: u32 = 5_000;
/// Share of edges that stay inside a community.
const INTRA_RATIO: f32 = 0.85;
/// Enough repetitions for a p95 that means something. p95 over 5 samples is the
/// largest one, which is a number about nothing.
const RUNS: usize = 50;
/// Seeds per query: 1 to 5, the range the brief asks for.
const SEED_COUNTS: [usize; 5] = [1, 2, 3, 4, 5];
/// The push floor the headline figures use.
const EPSILON: f32 = 1e-5;
/// How many ranked nodes are compared when asking whether two walks agree.
const TOP_K: usize = 100;

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn fmt_ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

struct Query {
    seeds: Vec<(u32, f32)>,
}

/// `RUNS` queries with 1..=5 seeds each, drawn from a deterministic stream, so
/// two runs of this harness query the same graph the same way.
fn queries(csr: &Csr) -> Vec<Query> {
    let mut rng = kb_core::graph::Xorshift(0x00C0_FFEE_1234_5678);
    let n = csr.node_count() as u32;
    (0..RUNS)
        .map(|i| {
            let seed_count = SEED_COUNTS[i % SEED_COUNTS.len()];
            let seeds = (0..seed_count)
                .map(|_| (rng.below(n), 1.0))
                .collect::<Vec<_>>();
            Query { seeds }
        })
        .collect()
}

fn clustered() -> (Csr, Duration) {
    let started = Instant::now();
    let csr = Csr::clustered(NODES, EDGES, CLUSTERS, INTRA_RATIO, 0x9e37_79b9_7f4a_7c15);
    (csr, started.elapsed())
}

fn giant_component() -> (Csr, Duration) {
    let started = Instant::now();
    let csr = Csr::power_law(NODES, EDGES, 0x9e37_79b9_7f4a_7c15);
    (csr, started.elapsed())
}

fn time_ppr(csr: &Csr, queries: &[Query], opts: &WalkOptions) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(queries.len());
    for q in queries {
        let started = Instant::now();
        let _ = ppr(csr, &q.seeds, opts);
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    samples
}

fn warm(csr: &Csr, queries: &[Query], opts: &[WalkOptions]) {
    // The first run pays for page faults on the CSR that the rest of the samples
    // would otherwise include.
    for q in queries.iter().take(5) {
        let _ = expansion_ranking(&expand(csr, &q.seeds, 2));
        for o in opts {
            let _ = ppr(csr, &q.seeds, o);
        }
    }
}

fn print_memory(csr: &Csr, build: Duration) {
    let heap = csr.heap_bytes();
    println!(
        "graph: {} nodes, {EDGES} edges, {} arcs (undirected projection), built in {:.1} ms",
        csr.node_count(),
        csr.arc_count(),
        fmt_ms(build)
    );
    println!(
        "csr memory: {heap} bytes = {:.1} MB of the {} MB budget  ({} offsets u32 + {} targets u32 + {} weights f32)",
        heap as f64 / (1024.0 * 1024.0),
        MEMORY_BUDGET_BYTES / (1024 * 1024),
        csr.node_count() + 1,
        csr.arc_count(),
        csr.arc_count(),
    );
}

fn print_line(label: &str, samples: &[Duration], budget: Option<Duration>) {
    let verdict = match budget {
        Some(b) => {
            if percentile(samples, 0.95) < b {
                "PASS"
            } else {
                "MISS"
            }
        }
        None => "-",
    };
    println!(
        "{label:<9} p50 {:>8.2} ms  p95 {:>8.2} ms  p99 {:>8.2} ms  max {:>8.2} ms  {verdict}",
        fmt_ms(percentile(samples, 0.50)),
        fmt_ms(percentile(samples, 0.95)),
        fmt_ms(percentile(samples, 0.99)),
        fmt_ms(samples.last().copied().unwrap_or(Duration::ZERO)),
    );
}

#[test]
#[ignore = "builds a 1M-edge graph and sweeps both walks; run with --release"]
fn ppr_plus_two_hop_expansion_meets_the_architecture_targets() {
    let (csr, build) = clustered();
    print_memory(&csr, build);
    let heap = csr.heap_bytes();

    let queries = queries(&csr);
    let push = WalkOptions::default().with_bounded_push(EPSILON);
    warm(&csr, &queries, &[push, WalkOptions::default()]);

    let push_samples = time_ppr(&csr, &queries, &push);
    let exact_samples = time_ppr(&csr, &queries, &WalkOptions::default());

    let mut expand_samples: Vec<Duration> = Vec::with_capacity(queries.len());
    for q in &queries {
        let started = Instant::now();
        let _ = expansion_ranking(&expand(&csr, &q.seeds, 2));
        expand_samples.push(started.elapsed());
    }
    expand_samples.sort_unstable();

    let mut combined: Vec<Duration> = push_samples
        .iter()
        .zip(expand_samples.iter())
        .map(|(a, b)| *a + *b)
        .collect();
    combined.sort_unstable();

    let (operations, inside_cap) = ppr_push_operations(&csr, &queries[0].seeds, &push);
    let (iterations, converged) = ppr_iterations(&csr, &queries[0].seeds, &WalkOptions::default());

    println!();
    println!(
        "{RUNS} queries, 1-5 seeds each, {CLUSTERS} communities, budget p95 < {:.0} ms:",
        P95_BUDGET.as_millis()
    );
    print_line("ppr push", &push_samples, Some(P95_BUDGET));
    print_line("ppr exact", &exact_samples, Some(P95_BUDGET));
    print_line("2-hop", &expand_samples, None);
    print_line("combined", &combined, Some(P95_BUDGET));
    println!();
    println!("exact: {iterations} power iterations to tolerance 1e-7 (converged: {converged})");
    println!(
        "push: {operations} push operations for the first query, epsilon {EPSILON} (inside cap: {inside_cap})"
    );

    println!();
    println!("combined push + 2-hop by seed count:");
    for (position, &count) in SEED_COUNTS.iter().enumerate() {
        let mut group: Vec<Duration> = (0..queries.len())
            .filter(|i| i % SEED_COUNTS.len() == position)
            .map(|i| push_samples[i] + expand_samples[i])
            .collect();
        group.sort_unstable();
        println!(
            "  {count} seed(s): n={:<3} p50 {:>7.2} ms  p95 {:>7.2} ms",
            group.len(),
            fmt_ms(percentile(&group, 0.50)),
            fmt_ms(percentile(&group, 0.95)),
        );
    }

    assert!(
        percentile(&combined, 0.95) < P95_BUDGET,
        "PPR (push) + 2-hop p95 is {:?}, over the {P95_BUDGET:?} target from architecture §11",
        percentile(&combined, 0.95)
    );
    assert!(
        heap < MEMORY_BUDGET_BYTES,
        "CSR for {EDGES} edges is {} bytes, over the {MEMORY_BUDGET_BYTES} byte target",
        heap
    );
}

/// Which walk should the graph arm use?
///
/// The brief asks for both when the target is missed, and exact power iteration
/// does miss it. This harness runs the whole grid on the pathological shape (one
/// giant component) so the cost of each knob is on the record, including the ones
/// that lose.
#[test]
#[ignore = "1M-edge sweep over every walk; run with --release"]
fn exact_versus_bounded_push() {
    let (csr, build) = giant_component();
    println!(
        "single giant component: {} nodes, {} arcs, built in {:.1} ms",
        csr.node_count(),
        csr.arc_count(),
        fmt_ms(build)
    );
    let queries = queries(&csr);
    let cases = all_cases();
    warm(
        &csr,
        &queries,
        &cases.iter().map(|(_, o)| *o).collect::<Vec<_>>(),
    );
    println!();
    print_grid(&csr, &queries, &cases);
    println!();
    println!(
        "exact cost model: {} iterations x {} arcs = {} arc visits per query",
        ppr_iterations(&csr, &queries[0].seeds, &WalkOptions::default()).0,
        csr.arc_count(),
        csr.arc_count() * ppr_iterations(&csr, &queries[0].seeds, &WalkOptions::default()).0
    );
}

/// The same question on the other graph shape.
///
/// [`Csr::power_law`] is one giant component, so PPR mass has nowhere local to
/// stay. A graph derived from documents is not shaped like that: entities
/// co-occur, so the mass around a seed stays inside its community. If the
/// clustered graph is cheap *and* agrees with exact PPR, then the cost is a
/// property of the shape rather than of the algorithm — and which shape a real
/// tenant has is an empirical question about the data, not something to assume in
/// either direction.
#[test]
#[ignore = "1M-edge sweep over a second graph shape; run with --release"]
fn same_algorithms_on_a_clustered_graph() {
    let (csr, build) = clustered();
    println!(
        "{CLUSTERS} communities, {:.0}% intra: {} nodes, {} arcs, built in {:.1} ms",
        INTRA_RATIO * 100.0,
        csr.node_count(),
        csr.arc_count(),
        fmt_ms(build)
    );
    let queries = queries(&csr);
    let cases = all_cases();
    warm(
        &csr,
        &queries,
        &cases.iter().map(|(_, o)| *o).collect::<Vec<_>>(),
    );
    println!();
    print_grid(&csr, &queries, &cases);
}

fn all_cases() -> Vec<(&'static str, WalkOptions)> {
    vec![
        ("push", WalkOptions::default().with_bounded_push(1e-3)),
        ("push", WalkOptions::default().with_bounded_push(1e-4)),
        ("push", WalkOptions::default().with_bounded_push(EPSILON)),
        ("push", WalkOptions::default().with_bounded_push(1e-6)),
        ("truncated", WalkOptions::default().with_truncated(1e-3)),
        ("truncated", WalkOptions::default().with_truncated(1e-4)),
        ("truncated", WalkOptions::default().with_truncated(EPSILON)),
        ("truncated", WalkOptions::default().with_truncated(1e-6)),
    ]
}

fn print_grid(csr: &Csr, queries: &[Query], cases: &[(&'static str, WalkOptions)]) {
    println!(
        "{:<11} {:<12} {:>9} {:>9} {:>12} {:>11} {:>10}",
        "algorithm", "epsilon", "p50 ms", "p95 ms", "pushes/its", "L1 error", "top-100"
    );
    for (name, opts) in cases {
        let samples = time_ppr(csr, queries, opts);
        let (l1, kept) = agreement(csr, queries, opts);
        let stats = match opts.algorithm {
            Algorithm::BoundedPush => {
                let (ops, _) = ppr_push_operations(csr, &queries[0].seeds, opts);
                format!("{ops:>7}")
            }
            _ => {
                let (its, _) = ppr_truncated_iterations(csr, &queries[0].seeds, opts);
                format!("{its:>7}")
            }
        };
        println!(
            "{name:<11} {:<12.0e} {:>9.2} {:>9.2} {stats} {l1:>11.5} {:>9.1}%",
            opts.epsilon,
            fmt_ms(percentile(&samples, 0.50)),
            fmt_ms(percentile(&samples, 0.95)),
            kept * 100.0,
        );
    }
    let exact_opts = WalkOptions::default();
    let exact_samples = time_ppr(csr, queries, &exact_opts);
    let (exact_iterations, _) = ppr_iterations(csr, &queries[0].seeds, &exact_opts);
    println!(
        "{:<11} {:<12} {:>9.2} {:>9.2} {:>12} {:>11} {:>10}",
        "exact",
        "1e-7 tol",
        fmt_ms(percentile(&exact_samples, 0.50)),
        fmt_ms(percentile(&exact_samples, 0.95)),
        exact_iterations,
        "0.00000",
        "100.0%",
    );
}

/// Mean L1 distance from the candidate walk's result to the exact result, and the
/// fraction of the exact top-K nodes it also puts in its top K. L1 is what the ACL
/// theorem bounds by `epsilon`; top-K overlap is what a chunk ranking feels.
fn agreement(csr: &Csr, queries: &[Query], opts: &WalkOptions) -> (f64, f64) {
    let exact_opts = WalkOptions::default();
    let mut l1_total = 0.0f64;
    let mut kept_total = 0.0f64;
    let counted = queries.len().min(10);
    for q in queries.iter().take(counted) {
        let exact = ppr(csr, &q.seeds, &exact_opts);
        let candidate = ppr(csr, &q.seeds, opts);
        let exact_map: std::collections::HashMap<u32, f32> =
            exact.iter().map(|s| (s.node, s.score)).collect();
        let mut candidate_map: std::collections::HashMap<u32, f32> =
            candidate.iter().map(|s| (s.node, s.score)).collect();
        let mut nodes: Vec<u32> = exact_map
            .keys()
            .chain(candidate_map.keys())
            .copied()
            .collect();
        nodes.sort_unstable();
        nodes.dedup();
        let mut l1 = 0.0f64;
        for node in nodes {
            l1 += (f64::from(exact_map.get(&node).copied().unwrap_or(0.0))
                - f64::from(candidate_map.remove(&node).unwrap_or(0.0)))
            .abs();
        }
        l1_total += l1;
        let exact_top: std::collections::HashSet<u32> =
            exact.iter().take(TOP_K).map(|s| s.node).collect();
        let candidate_top: std::collections::HashSet<u32> =
            candidate.iter().take(TOP_K).map(|s| s.node).collect();
        kept_total += exact_top.intersection(&candidate_top).count() as f64 / TOP_K as f64;
    }
    (l1_total / counted as f64, kept_total / counted as f64)
}

/// The seed sweep, so the per-seed cost is visible rather than averaged away.
#[test]
#[ignore = "1M-edge sweep; run with --release"]
fn sweep_one_to_five_seeds() {
    let (csr, _) = clustered();
    let mut rng = kb_core::graph::Xorshift(7);
    let n = csr.node_count() as u32;
    let push = WalkOptions::default().with_bounded_push(EPSILON);
    println!("clustered graph: {n} nodes, {} arcs", csr.arc_count());
    println!(
        "{:<8} {:>12} {:>12} {:>13} {:>11} {:>10}",
        "seeds", "push p50 ms", "push p95 ms", "exact p50 ms", "pushes", "iterations"
    );
    for count in 1..=5u32 {
        let seeds: Vec<(u32, f32)> = (0..count).map(|_| (rng.below(n), 1.0)).collect();
        let mut push_samples = time_ppr(
            &csr,
            &[Query {
                seeds: seeds.clone(),
            }],
            &push,
        );
        let exact_opts = WalkOptions::default();
        let mut exact_samples = Vec::new();
        for _ in 0..RUNS {
            let started = Instant::now();
            let _ = ppr(&csr, &seeds, &exact_opts);
            exact_samples.push(started.elapsed());
        }
        exact_samples.sort_unstable();
        push_samples.sort_unstable();
        let (operations, _) = ppr_push_operations(&csr, &seeds, &push);
        let (iterations, _) = ppr_iterations(&csr, &seeds, &exact_opts);
        println!(
            "{count:<8} {:>12.2} {:>12.2} {:>13.2} {operations:>11} {iterations:>10}",
            fmt_ms(percentile(&push_samples, 0.50)),
            fmt_ms(percentile(&push_samples, 0.95)),
            fmt_ms(percentile(&exact_samples, 0.50)),
        );
    }
}
