//! The `kb-server eval quant-bench` tool: measure what quantization costs and
//! what recall it buys, on a throwaway Qdrant collection.
//!
//! This is a Qdrant-level instrument, not an HTTP black-box eval: it writes its
//! own points and queries so the same vectors go into all three modes and the
//! only difference is the quantization config. Ground truth is an `exact: true`
//! float32 search on the `off` collection; every reported mode is compared to
//! those same top-20 ids.
//!
//! The tool never runs as part of `cargo test`. The big run (1M points, 500
//! queries) needs a box with enough RAM and is invoked by hand.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use qdrant_client::Payload;
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CollectionStatus, Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder,
    Distance, FieldType, Filter, KeywordIndexParamsBuilder, NamedVectors, PointStruct,
    QuantizationSearchParams, ScoredPoint, SearchParams, SearchPointsBuilder, UpsertPointsBuilder,
    Value, VectorParamsBuilder, VectorsConfigBuilder, r#match::MatchValue,
    point_id::PointIdOptions,
};

use crate::payload::KEY_TENANT_ID;
use crate::settings::{DENSE_VECTOR, QuantizationMode};

/// Default collection prefix (`{prefix}_{mode}`).
pub const DEFAULT_PREFIX: &str = "qb";
/// Default number of points to index per mode.
pub const DEFAULT_POINTS: usize = 100_000;
/// Default dense width (Nemotron at 2048 floats is ~8 GB unquantized at 1M).
pub const DEFAULT_DIM: usize = 2048;
/// Default number of queries per mode.
pub const DEFAULT_QUERIES: usize = 500;
/// Default RNG seed.
pub const DEFAULT_SEED: u64 = 42;
/// Top-k used for both the ground truth and each measured arm.
pub const TOP_K: u64 = 20;
/// Oversampling used by every quantized mode in the bench.
pub const BENCH_OVERSAMPLING: f64 = 2.0;
/// Rescoring used by every quantized mode in the bench.
pub const BENCH_RESCORE: bool = true;
/// The tenant every point and query carries (the scope filter).
pub const BENCH_TENANT: &str = "quant-bench";
/// RET-20's latency target, evaluated at 1M points.
pub const TARGET_P95_MS: f64 = 20.0;
/// RET-20's recall target, against exact float32.
pub const TARGET_RECALL: f64 = 0.97;

/// Everything the bench tool needs, already parsed and validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantBenchArgs {
    pub qdrant_url: String,
    pub prefix: String,
    pub points: usize,
    pub dim: usize,
    pub queries: usize,
    pub seed: u64,
    pub modes: Vec<QuantizationMode>,
    pub out: PathBuf,
    pub keep: bool,
}

/// What can go wrong before or during a bench run.
#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Failed(String),
}

/// One mode's measurement.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModeResult {
    pub mode: String,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub recall_at_20: f64,
    /// Qdrant client 1.19 exposes no telemetry call, so this is always the
    /// literal string the report uses when RAM was not measured.
    pub ram: String,
}

/// RET-20's pass/fail targets, restated in `results.json`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BenchTargets {
    /// The point count the targets are stated for.
    pub points: usize,
    pub p95_ms: f64,
    pub recall_at_20: f64,
}

/// The complete, serialisable output of a run.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BenchReport {
    pub qdrant_url: String,
    pub collection_prefix: String,
    pub points: usize,
    pub dim: usize,
    pub queries: usize,
    pub seed: u64,
    pub results: Vec<ModeResult>,
    pub targets: BenchTargets,
}

impl QuantBenchArgs {
    /// Parse the CLI flags. `--out` is required so a benchmark never writes
    /// into an operator's working directory by surprise.
    ///
    /// # Errors
    ///
    /// Missing flag values, unknown modes and non-positive sizes are usage
    /// errors; the caller turns them into a non-zero exit with the usage text.
    pub fn parse(args: &[String]) -> Result<Self, BenchError> {
        let mut qdrant_url = "http://127.0.0.1:16334".to_string();
        let mut prefix = DEFAULT_PREFIX.to_string();
        let mut points = DEFAULT_POINTS;
        let mut dim = DEFAULT_DIM;
        let mut queries = DEFAULT_QUERIES;
        let mut seed = DEFAULT_SEED;
        let mut mode_names = "off,scalar,binary".to_string();
        let mut out: Option<PathBuf> = None;
        let mut keep = false;

        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--keep" => {
                    keep = true;
                    index += 1;
                }
                flag @ ("--qdrant-url"
                | "--collection-prefix"
                | "--points"
                | "--dim"
                | "--queries"
                | "--seed"
                | "--modes"
                | "--out") => {
                    let value = args.get(index + 1).ok_or_else(|| {
                        BenchError::Usage(format!("quant-bench: {flag} needs a value"))
                    })?;
                    if value.starts_with("--") || value.trim().is_empty() {
                        return Err(BenchError::Usage(format!(
                            "quant-bench: {flag} needs a value"
                        )));
                    }
                    match flag {
                        "--qdrant-url" => qdrant_url = value.clone(),
                        "--collection-prefix" => prefix = value.clone(),
                        "--points" => points = parse_size(flag, value)?,
                        "--dim" => dim = parse_size(flag, value)?,
                        "--queries" => queries = parse_size(flag, value)?,
                        "--seed" => {
                            seed = value.trim().parse::<u64>().map_err(|_| {
                                BenchError::Usage(format!(
                                    "quant-bench: {flag} needs a u64, got {value:?}"
                                ))
                            })?;
                        }
                        "--modes" => mode_names = value.clone(),
                        "--out" => out = Some(PathBuf::from(value)),
                        _ => {
                            return Err(BenchError::Usage(format!(
                                "quant-bench: unknown flag {flag}"
                            )));
                        }
                    }
                    index += 2;
                }
                other => {
                    return Err(BenchError::Usage(format!(
                        "quant-bench: unknown argument {other:?}"
                    )));
                }
            }
        }

        if points == 0 || dim == 0 || queries == 0 {
            return Err(BenchError::Usage(
                "quant-bench: --points, --dim and --queries must all be positive".to_string(),
            ));
        }
        let out =
            out.ok_or_else(|| BenchError::Usage("quant-bench: --out DIR is required".to_string()))?;
        let modes = mode_names
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(QuantizationMode::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(BenchError::Usage)?;
        if modes.is_empty() {
            return Err(BenchError::Usage(
                "quant-bench: --modes must name at least one mode".to_string(),
            ));
        }

        Ok(Self {
            qdrant_url,
            prefix,
            points,
            dim,
            queries,
            seed,
            modes,
            out,
            keep,
        })
    }
}

fn parse_size(flag: &str, value: &str) -> Result<usize, BenchError> {
    value.trim().parse::<usize>().map_err(|_| {
        BenchError::Usage(format!(
            "quant-bench: {flag} needs a positive integer, got {value:?}"
        ))
    })
}

/// Run the benchmark. Creates `{prefix}_{mode}` collections, fills them with
/// identical deterministic vectors, measures latency and recall, writes the
/// report, and drops the collections unless `--keep` was given.
///
/// # Errors
///
/// Connection, collection setup, search and file-write failures are reported
/// as [`BenchError::Failed`]. A connection error never becomes an empty result.
pub async fn run(args: &QuantBenchArgs) -> Result<BenchReport, BenchError> {
    let client = Qdrant::from_url(&args.qdrant_url)
        .skip_compatibility_check()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|err| BenchError::Failed(format!("Qdrant endpoint {}: {err}", args.qdrant_url)))?;

    // `off` is the float32 reference for ground truth. Always create it, even
    // when the caller did not ask for it to be reported.
    let mut needed = args.modes.clone();
    if !needed.contains(&QuantizationMode::Off) {
        needed.insert(0, QuantizationMode::Off);
    }
    let mut collections = Vec::with_capacity(needed.len());
    for mode in &needed {
        let name = collection_name(args, *mode);
        recreate_collection(&client, &name, args, *mode).await?;
        upsert_points(&client, &name, args).await?;
        wait_green(&client, &name).await?;
        collections.push((*mode, name));
    }

    // Cleanup happens even when a measurement fails: a 1M-point bench leaves
    // 12 GB of collections behind otherwise.
    let measured = measure(&client, args, &collections).await;
    if !args.keep {
        for (_, name) in &collections {
            let _ = client.delete_collection(name).await;
        }
    }
    let report = measured?;
    write_outputs(args, &report)?;
    Ok(report)
}

/// The collection a mode uses.
fn collection_name(args: &QuantBenchArgs, mode: QuantizationMode) -> String {
    format!("{}_{}", args.prefix, mode.as_str())
}

async fn recreate_collection(
    client: &Qdrant,
    name: &str,
    args: &QuantBenchArgs,
    mode: QuantizationMode,
) -> Result<(), BenchError> {
    if client.collection_exists(name).await.map_err(failed)? {
        client.delete_collection(name).await.map_err(failed)?;
    }
    let mut vectors = VectorsConfigBuilder::default();
    let mut params = VectorParamsBuilder::new(args.dim as u64, Distance::Cosine);
    if let Some(config) = mode.quantization_config() {
        params = params.quantization_config(config);
    }
    vectors.add_named_vector_params(DENSE_VECTOR, params);
    client
        .create_collection(CreateCollectionBuilder::new(name).vectors_config(vectors))
        .await
        .map_err(failed)?;
    client
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(name, KEY_TENANT_ID, FieldType::Keyword)
                .field_index_params(KeywordIndexParamsBuilder::default().is_tenant(true).build())
                .wait(true),
        )
        .await
        .map_err(failed)?;
    Ok(())
}

/// Upsert the same deterministic vectors into every collection. The per-mode
/// RNG is seeded identically, so the only difference between arms is
/// quantization.
async fn upsert_points(
    client: &Qdrant,
    name: &str,
    args: &QuantBenchArgs,
) -> Result<(), BenchError> {
    let mut rng = args.seed;
    const BATCH: usize = 256;
    for start in (0..args.points).step_by(BATCH) {
        let end = (start + BATCH).min(args.points);
        let mut points = Vec::with_capacity(end - start);
        for id in start..end {
            let vectors = NamedVectors::default()
                .add_vector(DENSE_VECTOR, normalised_vector(&mut rng, args.dim));
            let payload: Payload =
                HashMap::from([(KEY_TENANT_ID.to_string(), Value::from(BENCH_TENANT))]).into();
            points.push(PointStruct::new(id as u64, vectors, payload));
        }
        client
            .upsert_points(UpsertPointsBuilder::new(name, points).wait(true))
            .await
            .map_err(failed)?;
    }
    Ok(())
}

/// Poll until Qdrant reports the collection green (all segments ready). A
/// quantized collection needs its quantized index built before a search means
/// anything.
async fn wait_green(client: &Qdrant, name: &str) -> Result<(), BenchError> {
    // A 1M x 2048 float32 collection can spend >600 s building HNSW on this
    // box; the poll window is a measurement-enabling wait, not a result.
    for _ in 0..3600 {
        let status = client
            .collection_info(name)
            .await
            .ok()
            .and_then(|response| response.result)
            .map(|info| info.status);
        if status == Some(CollectionStatus::Green as i32) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(1_000)).await;
    }
    Err(BenchError::Failed(format!(
        "collection {name} did not become green within 3600 s"
    )))
}

async fn measure(
    client: &Qdrant,
    args: &QuantBenchArgs,
    collections: &[(QuantizationMode, String)],
) -> Result<BenchReport, BenchError> {
    let reference = collections
        .iter()
        .find(|(mode, _)| *mode == QuantizationMode::Off)
        .map(|(_, name)| name.clone())
        .ok_or_else(|| BenchError::Failed("no float32 reference collection".to_string()))?;
    let filter = Filter::must([Condition::matches(
        KEY_TENANT_ID,
        MatchValue::Keyword(BENCH_TENANT.to_string()),
    )]);

    // A separate RNG stream for queries: changing --queries must not change
    // the stored vectors, only how many queries are asked.
    let mut query_rng = args.seed ^ 0x9E37_79B9_7F4A_7C15;
    let queries: Vec<Vec<f32>> = (0..args.queries)
        .map(|_| normalised_vector(&mut query_rng, args.dim))
        .collect();

    // Ground truth: exact float32 top-20 on the `off` collection.
    let exact_params = SearchParams {
        exact: Some(true),
        ..Default::default()
    };
    let mut truth: Vec<Vec<u64>> = Vec::with_capacity(args.queries);
    for query in &queries {
        let response = client
            .search_points(
                SearchPointsBuilder::new(&reference, query.clone(), TOP_K)
                    .vector_name(DENSE_VECTOR)
                    .filter(filter.clone())
                    .params(exact_params.clone()),
            )
            .await
            .map_err(failed)?;
        truth.push(ids(&response.result));
    }

    let mut results = Vec::new();
    for (mode, name) in collections
        .iter()
        .filter(|(mode, _)| args.modes.contains(mode))
    {
        let quantization: Option<QuantizationSearchParams> =
            mode.search_params(BENCH_OVERSAMPLING, BENCH_RESCORE);
        let mut latencies = Vec::with_capacity(args.queries);
        let mut recall_sum = 0.0;
        for (query, expected) in queries.iter().zip(&truth) {
            let mut builder = SearchPointsBuilder::new(name, query.clone(), TOP_K)
                .vector_name(DENSE_VECTOR)
                .filter(filter.clone());
            if let Some(params) = quantization {
                builder = builder.params(SearchParams {
                    quantization: Some(params),
                    ..Default::default()
                });
            }
            let started = Instant::now();
            let response = client.search_points(builder).await.map_err(failed)?;
            latencies.push(started.elapsed().as_secs_f64() * 1_000.0);
            recall_sum += recall_at_20(expected, &ids(&response.result));
        }
        latencies.sort_by(f64::total_cmp);
        results.push(ModeResult {
            mode: mode.as_str().to_string(),
            p50_ms: percentile(&latencies, 0.50),
            p95_ms: percentile(&latencies, 0.95),
            recall_at_20: recall_sum / args.queries as f64,
            // qdrant-client 1.19 has no telemetry call; see `render_report`.
            ram: "not measured".to_string(),
        });
    }

    Ok(BenchReport {
        qdrant_url: args.qdrant_url.clone(),
        collection_prefix: args.prefix.clone(),
        points: args.points,
        dim: args.dim,
        queries: args.queries,
        seed: args.seed,
        results,
        targets: BenchTargets {
            points: 1_000_000,
            p95_ms: TARGET_P95_MS,
            recall_at_20: TARGET_RECALL,
        },
    })
}

fn ids(points: &[ScoredPoint]) -> Vec<u64> {
    points
        .iter()
        .filter_map(|point| match &point.id {
            Some(id) => match &id.point_id_options {
                Some(PointIdOptions::Num(number)) => Some(*number),
                _ => None,
            },
            None => None,
        })
        .collect()
}

fn failed(err: qdrant_client::QdrantError) -> BenchError {
    BenchError::Failed(err.to_string())
}

fn write_outputs(args: &QuantBenchArgs, report: &BenchReport) -> Result<(), BenchError> {
    std::fs::create_dir_all(&args.out)
        .map_err(|err| BenchError::Failed(format!("create {}: {err}", args.out.display())))?;
    let json = serde_json::to_string_pretty(report)
        .map_err(|err| BenchError::Failed(format!("serialize report: {err}")))?;
    let results = args.out.join("results.json");
    std::fs::write(&results, format!("{json}\n"))
        .map_err(|err| BenchError::Failed(format!("write {}: {err}", results.display())))?;
    let report_path = args.out.join("report.md");
    std::fs::write(&report_path, render_report(report))
        .map_err(|err| BenchError::Failed(format!("write {}: {err}", report_path.display())))?;
    Ok(())
}

fn render_report(report: &BenchReport) -> String {
    let mut out = String::new();
    out.push_str("# Qdrant quantization bench\n\n");
    out.push_str(&format!(
        "{} points, {}-dim cosine vectors, {} queries, seed {}, prefix `{}`.\n\n",
        report.points, report.dim, report.queries, report.seed, report.collection_prefix
    ));
    out.push_str("| mode | p50 (ms) | p95 (ms) | recall@20 | RAM |\n");
    out.push_str("|---|---:|---:|---:|---|\n");
    for result in &report.results {
        out.push_str(&format!(
            "| {} | {:.2} | {:.2} | {:.4} | {} |\n",
            result.mode, result.p50_ms, result.p95_ms, result.recall_at_20, result.ram
        ));
    }
    out.push_str(&format!(
        "\nTargets at 1M points: p95 < {:.1} ms and recall@20 >= {:.2} against exact float32.\n\n",
        report.targets.p95_ms, report.targets.recall_at_20
    ));
    out.push_str(
        "RAM is **not measured**: qdrant-client 1.19 exposes no telemetry endpoint, \
         so no number is invented here. Read Qdrant's own dashboard or `/telemetry` \
         on the big-RAM box alongside this run.\n",
    );
    out
}

/// The next state of a 64-bit xorshift generator. A zero seed is mapped to a
/// fixed non-zero state so the generator cannot get stuck.
fn next_u64(state: &mut u64) -> u64 {
    if *state == 0 {
        *state = 0x9E37_79B9_7F4A_7C15;
    }
    let mut value = *state;
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    *state = value;
    value
}

/// A deterministic unit-length vector. The high 53 bits of the generator give
/// a uniform value in `[-1, 1)`; dividing by the Euclidean length projects it
/// onto the unit sphere, which is what a cosine collection expects.
fn normalised_vector(state: &mut u64, dim: usize) -> Vec<f32> {
    let mut values = Vec::with_capacity(dim);
    let mut squared = 0.0_f64;
    for _ in 0..dim {
        let unit = (next_u64(state) >> 11) as f64 / (1_u64 << 53) as f64;
        let value = unit * 2.0 - 1.0;
        squared += value * value;
        values.push(value as f32);
    }
    let norm = squared.sqrt();
    if norm > 0.0 {
        for value in &mut values {
            *value = (*value as f64 / norm) as f32;
        }
    }
    values
}

/// Recall@k as overlap with the exact top-k, divided by the exact list's size.
/// An empty ground truth cannot be scored, so it scores zero rather than a
/// misleading one.
fn recall_at_20(expected: &[u64], actual: &[u64]) -> f64 {
    if expected.is_empty() {
        return 0.0;
    }
    let found: HashSet<u64> = actual.iter().copied().collect();
    let hits = expected.iter().filter(|id| found.contains(id)).count();
    hits as f64 / expected.len() as f64
}

/// Linear-interpolated percentile of an already sorted sample (milliseconds).
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let position = fraction * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    sorted[lower] + (sorted[upper] - sorted[lower]) * (position - lower as f64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn random_vectors_are_deterministic_for_a_seed() {
        let mut first = 42;
        let mut second = 42;
        let a = normalised_vector(&mut first, 16);
        let b = normalised_vector(&mut second, 16);
        assert_eq!(a, b);
        assert_ne!(a, normalised_vector(&mut 43, 16));
    }

    #[test]
    fn random_vectors_are_unit_length_and_change_with_the_seed() {
        let vector = normalised_vector(&mut 42, 2048);
        assert_eq!(vector.len(), 2048);
        let norm: f64 = vector.iter().map(|value| f64::from(*value).powi(2)).sum();
        assert!((norm - 1.0).abs() < 1e-4, "norm was {norm}");
    }

    #[test]
    fn recall_at_20_is_overlap_over_the_exact_list_size() {
        let expected = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(
            recall_at_20(
                &expected,
                &expected.iter().rev().copied().collect::<Vec<_>>()
            ),
            1.0
        );
        assert_eq!(recall_at_20(&expected, &[]), 0.0);
        let half = vec![1, 3, 5, 7, 9, 91, 92, 93, 94, 95];
        assert_eq!(recall_at_20(&expected, &half), 0.5);
        assert_eq!(recall_at_20(&[], &expected), 0.0);
    }

    #[test]
    fn percentiles_interpolate_between_samples() {
        let samples: Vec<f64> = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&samples, 0.5), 2.5);
        assert!((percentile(&samples, 0.95) - 3.85).abs() < 1e-9);
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
    }

    #[test]
    fn quant_bench_flags_parse_with_the_documented_defaults() {
        let parsed = QuantBenchArgs::parse(&args(&["--out", "/tmp/qb-results"])).expect("parse");
        assert_eq!(parsed.qdrant_url, "http://127.0.0.1:16334");
        assert_eq!(parsed.prefix, "qb");
        assert_eq!(parsed.points, 100_000);
        assert_eq!(parsed.dim, 2048);
        assert_eq!(parsed.queries, 500);
        assert_eq!(parsed.seed, 42);
        assert_eq!(
            parsed.modes,
            vec![
                QuantizationMode::Off,
                QuantizationMode::Scalar,
                QuantizationMode::Binary
            ]
        );
        assert!(!parsed.keep);
        assert_eq!(parsed.out, PathBuf::from("/tmp/qb-results"));
    }

    #[test]
    fn quant_bench_flags_accept_explicit_values_and_keep() {
        let parsed = QuantBenchArgs::parse(&args(&[
            "--qdrant-url",
            "http://qdrant:6334",
            "--collection-prefix",
            "qb",
            "--points",
            "2000",
            "--dim",
            "8",
            "--queries",
            "50",
            "--seed",
            "7",
            "--modes",
            "off,binary",
            "--out",
            "/tmp/qb-small",
            "--keep",
        ]))
        .expect("parse");
        assert_eq!(parsed.qdrant_url, "http://qdrant:6334");
        assert_eq!(parsed.points, 2000);
        assert_eq!(parsed.dim, 8);
        assert_eq!(parsed.queries, 50);
        assert_eq!(parsed.seed, 7);
        assert_eq!(
            parsed.modes,
            vec![QuantizationMode::Off, QuantizationMode::Binary]
        );
        assert!(parsed.keep);
    }

    #[test]
    fn quant_bench_rejects_unknown_modes_and_missing_values() {
        let err =
            QuantBenchArgs::parse(&args(&["--out", "/tmp/qb", "--modes", "turbo"])).unwrap_err();
        assert!(matches!(err, BenchError::Usage(_)));
        let err = QuantBenchArgs::parse(&args(&["--out"])).unwrap_err();
        assert!(matches!(err, BenchError::Usage(_)));
        let err = QuantBenchArgs::parse(&args(&[])).unwrap_err();
        assert!(matches!(err, BenchError::Usage(_)));
    }
}
