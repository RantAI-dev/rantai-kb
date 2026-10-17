//! What `exact: true` buys, and what it costs, at the corpus sizes this service
//! actually serves.
//!
//! The question this answers is narrow and decided by measurement, not taste:
//! the tiny CI corpus is 19 chunks and the real one is ~1.5k, so if an exact scan
//! of the real corpus is within noise of HNSW there is no reason to approximate
//! at that size at all — and if it is not, the crossover point is worth knowing
//! before the corpus grows by three orders of magnitude.
//!
//! Reported per size: latency of both modes (p50/p95 over 50 queries) and
//! `recall@20` of HNSW against the exact ranking, which is the ground truth by
//! definition.
//!
//! A benchmark, not a pass/fail check, so it is `#[ignore]`d rather than gated on
//! an environment variable: it builds 66,500 points and takes about half a
//! minute, which is not something `--features integration` should do to anyone.
//!
//! ```sh
//! KB_STACK_QDRANT=1 rust/scripts/stack-up.sh
//! cargo test -p kb-store-qdrant --features integration \
//!   --test exact_vs_hnsw -- --ignored --nocapture
//! rust/scripts/stack-down.sh
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Instant;

use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, NamedVectors, PointId, PointStruct, Query,
    QueryPointsBuilder, SearchParams, UpsertPointsBuilder, Value, Vector, VectorParamsBuilder,
    VectorsConfigBuilder,
};
use std::collections::HashMap;

use kb_store_qdrant::QdrantSettings;

/// The width of every real embedding this service has run: Qwen3-Embedding-0.6B.
const DIM: usize = 1024;
/// 1,477 chunks is the miracl-id-50 corpus; the others are one and two orders of
/// magnitude above it, so the curve has somewhere to go.
const SIZES: [usize; 3] = [1_500, 15_000, 50_000];
const QUERIES: usize = 50;
const K: u64 = 20;
/// One tenant, one scope: the filter a real search carries.
const TENANT: &str = "tenant-a";

struct Rng(u64);

impl Rng {
    /// xorshift64*, so the corpus is the same on every machine and every run and
    /// two sizes are comparable.
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish in [0, 1).
    fn next_unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }
}

fn collection_name(size: usize) -> String {
    format!("exact_vs_hnsw_{size}_{}", std::process::id())
}

fn vector(rng: &mut Rng) -> Vec<f32> {
    (0..DIM).map(|_| rng.next_unit()).collect()
}

async fn build(client: &Qdrant, name: &str, size: usize) {
    let _ = client.delete_collection(name).await;
    let mut vectors = VectorsConfigBuilder::default();
    vectors.add_named_vector_params(
        "dense",
        VectorParamsBuilder::new(DIM as u64, Distance::Cosine).build(),
    );
    client
        .create_collection(CreateCollectionBuilder::new(name).vectors_config(vectors))
        .await
        .expect("create collection");

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ size as u64);
    for batch in 0..size.div_ceil(1000) {
        let start = batch * 1000;
        let end = (start + 1000).min(size);
        let points: Vec<PointStruct> = (start..end)
            .map(|i| {
                let mut payload = HashMap::new();
                payload.insert("tenant_id".to_string(), Value::from(TENANT));
                let named = NamedVectors::default()
                    .add_vector("dense", Vector::new_dense(vector(&mut rng)));
                PointStruct::new(PointId::from(i as u64), named, payload)
            })
            .collect();
        client
            .upsert_points(UpsertPointsBuilder::new(name, points).wait(true).build())
            .await
            .expect("upsert batch");
    }
}

/// The point ids one mode returns for `query`, and how long it took.
async fn run(client: &Qdrant, name: &str, query: &[f32], exact: bool) -> (Vec<u64>, f64) {
    let mut builder = QueryPointsBuilder::new(name)
        .using("dense")
        .query(Query::new_nearest(query.to_vec()))
        .limit(K)
        .with_payload(false);
    if exact {
        builder = builder.params(SearchParams {
            exact: Some(true),
            ..Default::default()
        });
    }
    let started = Instant::now();
    let response = client.query(builder).await.expect("query");
    let ids = response
        .result
        .iter()
        .filter_map(|p| p.id.as_ref())
        .filter_map(|id| match id.point_id_options {
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => Some(n),
            _ => None,
        })
        .collect();
    (ids, started.elapsed().as_secs_f64() * 1000.0)
}

fn p50(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn p95(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() * 95 / 100]
}

#[tokio::test]
#[ignore = "benchmark: builds 66,500 points. Run with --ignored."]
async fn exact_and_hnsw_at_real_corpus_sizes() {
    let mut settings = QdrantSettings::from_env();
    settings.dense_dims = Some(DIM as u32);
    let client = settings.client().expect("client");

    println!("\n| points | mode   | p50 ms | p95 ms | recall@{K} vs exact |");
    println!("|---:|---|---:|---:|---:|");
    for size in SIZES {
        let name = collection_name(size);
        build(&client, &name, size).await;

        let mut rng = Rng(0x5EED_1234_ABCD_0001 ^ size as u64);
        let queries: Vec<Vec<f32>> = (0..QUERIES).map(|_| vector(&mut rng)).collect();

        let mut exact_ms = Vec::with_capacity(QUERIES);
        let mut hnsw_ms = Vec::with_capacity(QUERIES);
        let mut recall_hits = 0usize;
        let mut recall_total = 0usize;
        for query in &queries {
            let (truth, e) = run(&client, &name, query, true).await;
            let (approx, h) = run(&client, &name, query, false).await;
            exact_ms.push(e);
            hnsw_ms.push(h);
            recall_total += truth.len();
            recall_hits += truth.iter().filter(|id| approx.contains(id)).count();
        }
        let recall = recall_hits as f64 / recall_total as f64;
        let (exact_p50, exact_p95) = (p50(&mut exact_ms), p95(&mut exact_ms));
        let (hnsw_p50, hnsw_p95) = (p50(&mut hnsw_ms), p95(&mut hnsw_ms));
        println!("| {size} | exact  | {exact_p50:.2} | {exact_p95:.2} | 1.000 |");
        println!("| {size} | hnsw   | {hnsw_p50:.2} | {hnsw_p95:.2} | {recall:.3} |");
        let _ = client.delete_collection(&name).await;
    }
}

#[tokio::test]
#[ignore = "scale benchmark: filtered top-50 at scale up to 1M chunks. Run with --ignored."]
async fn vector_store_filtered_top50_scale() {
    use kb_store_qdrant::{
        DENSE_VECTOR, create_collection, drop_collection, payload::KEY_TENANT_ID,
    };
    use qdrant_client::qdrant::{Condition, Filter, r#match::MatchValue};

    let mut settings = QdrantSettings::from_env();
    settings.dense_dims = Some(DIM as u32);
    let client = settings.client().expect("client");

    let scale_sizes: [usize; 4] = [50_000, 100_000, 250_000, 500_000];
    println!("\n=== Vector Store Filtered Top-50 Scale Check ===");
    println!("| Points | Filter | Top-K | p50 ms | p95 ms | Queries |");
    println!("|---:|---|---:|---:|---:|---:|");

    let collection = format!("scale_test_{}", std::process::id());
    let _ = drop_collection(&client, &collection).await;
    create_collection(&client, &settings, &collection)
        .await
        .expect("create collection");

    let mut current_size: usize = 0;
    let mut rng = Rng(0xCAFE_BABE_DEAD_BEEF);
    let queries: Vec<Vec<f32>> = (0..50).map(|_| vector(&mut rng)).collect();

    for &target_size in &scale_sizes {
        let to_add: usize = target_size - current_size;
        let batch_size: usize = 1000;
        for batch in 0..to_add.div_ceil(batch_size) {
            let start = current_size + batch * batch_size;
            let end = (start + batch_size).min(target_size);
            let points: Vec<PointStruct> = (start..end)
                .map(|i| {
                    let tenant = if i % 5 == 0 {
                        "tenant-target"
                    } else {
                        "tenant-other"
                    };
                    let mut payload = HashMap::new();
                    payload.insert(KEY_TENANT_ID.to_string(), Value::from(tenant));
                    let named = NamedVectors::default()
                        .add_vector(DENSE_VECTOR, Vector::new_dense(vector(&mut rng)));
                    PointStruct::new(PointId::from(i as u64), named, payload)
                })
                .collect();
            client
                .upsert_points(
                    UpsertPointsBuilder::new(&collection, points)
                        .wait(true)
                        .build(),
                )
                .await
                .expect("upsert batch");
        }
        current_size = target_size;

        // Run 50 warm filtered top-50 queries
        let filter = Filter::must([Condition::matches(
            KEY_TENANT_ID,
            MatchValue::Keyword("tenant-target".to_string()),
        )]);

        let mut latencies = Vec::with_capacity(queries.len());
        for q in &queries {
            let builder = QueryPointsBuilder::new(&collection)
                .using(DENSE_VECTOR)
                .query(Query::new_nearest(q.clone()))
                .filter(filter.clone())
                .limit(50)
                .with_payload(false);
            let started = Instant::now();
            let _res = client.query(builder).await.expect("query");
            latencies.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let med = p50(&mut latencies);
        let p95_val = p95(&mut latencies);
        println!("| {current_size} | tenant=tenant-target | 50 | {med:.2} | {p95_val:.2} | 50 |");
    }

    let _ = drop_collection(&client, &collection).await;
}
