//! `kb-server eval scope-bench`: search latency versus scope size.
//!
//! The instrument seeds synthetic tenants straight into Postgres and Qdrant —
//! no ingest, no embedding provider, no paid batch — and then times
//! `POST /v1/search` through the real HTTP API. It is deliberately a two-pass
//! tool: the operator restarts the server with `KB_SCOPE_ID_LIMIT=0` for the
//! Ids pass and with `KB_SCOPE_ID_LIMIT=2000` for the predicate pass, giving
//! each pass a different `--mode-label`. Both passes append to the same
//! `--out` directory; `report.md` compares them once both exist.
//!
//! The target is `predicate p95(100k) <= 1.2 x predicate p95(1k)`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use qdrant_client::Payload;
use qdrant_client::qdrant::{Condition, DeletePointsBuilder, Filter, MatchValue, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use sqlx::{PgPool, QueryBuilder, query};

use crate::cli::CliError;
use crate::config::Config;
use kb_ports::{ChunkId, ChunkType, DocumentId, IndexUnit, KnowledgeBaseId, RetrievalIndex, TenantId};
use kb_store_pg::{NewApiKey, PgSettings, connect_pool, generate_api_key, insert_api_key};
use kb_store_qdrant::payload::{KEY_CHUNK_INDEX, KEY_CONTENT, KEY_DOCUMENT_ID, KEY_KNOWLEDGE_BASE_ID, KEY_PAGE, KEY_TENANT_ID, candidate_from, unit_payload};
use kb_store_qdrant::{QdrantIndex, QdrantSettings};

/// Default scope sizes, in documents per synthetic tenant.
pub const DEFAULT_SIZES: &[usize] = &[1_000, 10_000, 100_000];
/// Default number of timed searches per size.
pub const DEFAULT_QUERIES: usize = 200;
/// Maximum allowed ratio of predicate p95 at 100k over predicate p95 at 1k.
pub const TARGET_RATIO: f64 = 1.2;
/// Postgres/Qdrant rows written per batch, so a 100k tenant stays bounded.
const SEED_BATCH: usize = 500;

/// One invocation: a single server mode, one or more tenant sizes.
#[derive(Debug, Clone)]
pub struct ScopeBenchConfig {
    pub base_url: String,
    pub database_url: String,
    /// Accepted for operator familiarity. Keys are minted in Postgres with
    /// `create-api-key` internals; there is no key-creation HTTP route.
    pub key_admin: Option<String>,
    pub sizes: Vec<usize>,
    pub queries: usize,
    pub out_dir: PathBuf,
    pub mode_label: String,
    pub keep: bool,
}

/// One measured (size, mode) row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchRow {
    pub size: usize,
    pub mode_label: String,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub mean_scope_verify_dropped: f64,
    pub queries: usize,
    pub errors: usize,
    pub tenant_id: String,
    #[serde(default)]
    pub skipped: bool,
}

/// The accumulated two-pass results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultsFile {
    pub harness: String,
    pub timestamp: String,
    pub collection: String,
    pub dim: u32,
    pub queries: usize,
    pub target_ratio: f64,
    pub rows: Vec<BenchRow>,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Parse `--sizes 1000,10000,100000`. Pure, and tested.
pub fn parse_sizes(raw: &str) -> Result<Vec<usize>, String> {
    let sizes: Vec<usize> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<usize>().map_err(|_| format!("bad size {s:?}")))
        .collect::<Result<_, _>>()?;
    if sizes.is_empty() {
        return Err("--sizes cannot be empty".to_string());
    }
    if sizes.iter().any(|n| *n == 0) {
        return Err("--sizes must contain positive integers".to_string());
    }
    Ok(sizes)
}

/// Nearest-rank percentile of an **already sorted** slice. `q` is in 0..=1.
/// An empty slice yields 0.0; a single sample yields that sample for any q.
pub fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (q.clamp(0.0, 1.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

/// Deterministic xorshift64. Same seed, same queries in both passes, so the
/// two p95s describe the same workload.
pub struct Prng(u64);

impl Prng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// The one synthetic chunk of a document: a random unit vector plus the exact
/// payload shape `kb-store-qdrant` writes for a real chunk.
pub fn bench_unit(
    tenant: &str,
    document: &str,
    kb: &str,
    token: &str,
    dim: u32,
    random: &mut Prng,
) -> IndexUnit {
    let mut dense: Vec<f32> = (0..dim)
        .map(|_| (random.next() % 10_000) as f32 / 10_000.0)
        .collect();
    let norm = dense.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut dense {
            *value /= norm;
        }
    }
    IndexUnit {
        id: ChunkId::new_unchecked(format!("{document}_0")),
        document_id: DocumentId::new_unchecked(document),
        tenant: TenantId::new_unchecked(tenant),
        // The metadata array is what `unit_payload` writes under
        // `knowledge_base_id`: a keyword array, not a scalar.
        knowledge_base_id: Some(KnowledgeBaseId::new_unchecked(kb)),
        category: None,
        chunk_index: 0,
        page: Some(0),
        chunk_type: ChunkType::Text,
        content: token.to_string(),
        contextual_prefix: None,
        metadata: serde_json::json!({"knowledgeBaseIds": [kb], "documentTitle": token}),
        dense,
        sparse: kb_core::sparse::SparseVector::empty(),
    }
}

/// Read a payload value as a string, for the pure payload test.
fn payload_string(payload: &HashMap<String, Value>, key: &str) -> Option<String> {
    match payload.get(key)?.kind.as_ref()? {
        qdrant_client::qdrant::value::Kind::StringValue(s) => Some(s.clone()),
        _ => None,
    }
}

/// The PASS/MISS/NOT_RUN verdict and its explanation.
pub fn verdict(results: &ResultsFile) -> (String, String) {
    let predicate: Vec<&BenchRow> = results
        .rows
        .iter()
        .filter(|row| !row.skipped && row.mode_label.to_ascii_lowercase().contains("predicate"))
        .collect();
    let (Some(small_size), Some(big_size)) = (
        predicate.iter().map(|r| r.size).min(),
        predicate.iter().map(|r| r.size).max(),
    ) else {
        return (
            "NOT_RUN".to_string(),
            "no predicate-mode row was measured".to_string(),
        );
    };
    let (Some(small), Some(big)) = (
        predicate.iter().find(|r| r.size == small_size),
        predicate.iter().find(|r| r.size == big_size),
    ) else {
        return ("NOT_RUN".to_string(), "predicate rows are missing".to_string());
    };
    if small.size >= big.size || small.p95_ms <= 0.0 {
        return (
            "NOT_RUN".to_string(),
            "need two distinct predicate sizes with a positive p95".to_string(),
        );
    }
    let ratio = big.p95_ms / small.p95_ms;
    let status = if ratio <= results.target_ratio {
        "PASS"
    } else {
        "MISS"
    };
    (
        status.to_string(),
        format!(
            "predicate p95 {:.2} ms at {} / {:.2} ms at {} = {:.2}x (limit {:.2}x)",
            big.p95_ms, big.size, small.p95_ms, small.size, ratio, results.target_ratio
        ),
    )
}

/// Render `report.md` from accumulated rows.
pub fn render_report(results: &ResultsFile) -> String {
    let (status, detail) = verdict(results);
    let mut out = format!(
        "# Scope scale benchmark\n\n- target: predicate p95 at 100k <= {:.2}x predicate p95 at 1k\n- result: **{status}** — {detail}\n- collection: `{}` (dim {})\n- queries per row: {}\n\n",
        results.target_ratio, results.collection, results.dim, results.queries
    );
    out.push_str("| size | mode | p50 ms | p95 ms | mean scopeVerifyDropped | queries | errors |\n|---:|---|---:|---:|---:|---:|---:|\n");
    for row in &results.rows {
        out.push_str(&format!(
            "| {} | {} | {:.2} | {:.2} | {:.2} | {} | {} |\n",
            row.size,
            row.mode_label,
            row.p50_ms,
            row.p95_ms,
            row.mean_scope_verify_dropped,
            row.queries,
            row.errors
        ));
    }
    out.push_str("\n## How to run the two passes\n\n");
    out.push_str("1. Start the server with `KB_SCOPE_ID_LIMIT=0`, then run `kb-server eval scope-bench --mode-label ids ...`.\n");
    out.push_str("2. Restart it with `KB_SCOPE_ID_LIMIT=2000`, then run the same command with `--mode-label predicate` against the same `--out` dir.\n");
    out.push_str("3. Re-read this report: the verdict is only PASS/MISS once both passes have rows.\n");
    for note in &results.notes {
        out.push_str(&format!("\n- note: {note}\n"));
    }
    out
}

/// Parse the CLI. All arguments are `--flag value` pairs; `--keep` is a switch.
fn parse_args(args: &[String]) -> Result<ScopeBenchConfig, CliError> {
    let mut value = |name: &str| -> Option<String> {
        let idx = args.iter().position(|a| a == name)?;
        let next = args.get(idx + 1)?;
        if next.starts_with("--") || next.trim().is_empty() {
            return None;
        }
        Some(next.clone())
    };
    let base_url = value("--base-url")
        .ok_or_else(|| CliError::Usage("scope-bench: missing --base-url".to_string()))?;
    let database_url = value("--database-url")
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .ok_or_else(|| {
            CliError::Usage("scope-bench: missing --database-url (or DATABASE_URL)".to_string())
        })?;
    let sizes = parse_sizes(&value("--sizes").unwrap_or_else(|| {
        DEFAULT_SIZES
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }))
    .map_err(CliError::Usage)?;
    let queries = value("--queries")
        .map(|raw| {
            raw.parse::<usize>()
                .map_err(|_| CliError::Usage("--queries expects a number".to_string()))
        })
        .transpose()?
        .unwrap_or(DEFAULT_QUERIES);
    let out_dir = PathBuf::from(value("--out").unwrap_or_else(|| "eval-out/scope-bench".into()));
    Ok(ScopeBenchConfig {
        base_url,
        database_url,
        key_admin: value("--key-admin").or_else(|| std::env::var("KB_API_KEY").ok()),
        sizes,
        queries,
        out_dir,
        mode_label: value("--mode-label").unwrap_or_else(|| "ids".to_string()),
        keep: args.iter().any(|a| a == "--keep"),
    })
}

/// Entry point for `kb-server eval scope-bench …`.
pub async fn run_cli(args: &[String]) -> Result<(), CliError> {
    let config = parse_args(args)?;
    let out_dir = config.out_dir.clone();
    let results = run(config).await?;
    let (status, detail) = verdict(&results);
    println!("scope-bench: {status} — {detail}");
    println!("wrote {}", out_dir.join("results.json").display());
    Ok(())
}

/// Settings for the synthetic corpus derived from the server's own env.
fn bench_index_settings() -> Result<(QdrantSettings, String, u32), CliError> {
    let lookup = |key: &str| std::env::var(key).ok();
    let model = lookup("KB_EMBEDDING_MODEL").unwrap_or_else(|| "unknown".to_string());
    let (settings, collection_override) =
        Config::qdrant_from_lookup(&lookup, &model).map_err(|e| CliError::Failed(e.to_string()))?;
    let collection = collection_override.unwrap_or_else(|| settings.collection_name());
    let dim = settings
        .dense_dims
        .unwrap_or_else(|| Config::embedding_dim(&lookup));
    Ok((settings, collection, dim))
}

/// Seed one tenant, time its searches, and clean it up unless `--keep`.
async fn measure_size(
    pool: &PgPool,
    index: &QdrantIndex,
    client: &reqwest::Client,
    config: &ScopeBenchConfig,
    size: usize,
    dim: u32,
    collection: &str,
    run_id: &str,
) -> Result<BenchRow, CliError> {
    let tenant = format!("scope-bench-{run_id}-{size}");
    let kb_id = format!("bench-kb-{run_id}-{size}");
    query(
        r#"INSERT INTO "KnowledgeBase" ("id", "tenantId", "name", "createdAt", "updatedAt")
           VALUES ($1, $2, $3, now(), now())"#,
    )
    .bind(&kb_id)
    .bind(&tenant)
    .bind("scope-bench")
    .execute(pool)
    .await
    .map_err(|e| CliError::Failed(format!("pg insert kb: {e}")))?;

    let key = generate_api_key().map_err(|e| CliError::Failed(e.to_string()))?;
    let empty: Vec<String> = Vec::new();
    insert_api_key(
        pool,
        &key,
        &NewApiKey {
            tenant_id: &tenant,
            name: "scope-bench",
            scopes: &empty,
            knowledge_base_ids: &empty,
        },
    )
    .await
    .map_err(|e| CliError::Failed(e.to_string()))?;

    let mut random = Prng::new(0x5C0B ^ size as u64 ^ (run_id.len() as u64));
    let mut inserted = 0usize;
    while inserted < size {
        let end = (inserted + SEED_BATCH).min(size);
        let mut docs = QueryBuilder::new(
            r#"INSERT INTO "Document" ("id", "tenantId", "title", "content", "categories",
                "status", "externalRef", "createdAt", "updatedAt", "isLatest", "aclPrincipals")
               "#,
        );
        let mut groups = QueryBuilder::new(
            r#"INSERT INTO "DocumentGroup" ("id", "documentId", "knowledgeBaseId", "createdAt") "#,
        );
        let mut units = Vec::with_capacity(end - inserted);
        for i in inserted..end {
            let document = format!("bench-{run_id}-{size}-{i}");
            let token = format!("scopebench{run_id}{size}t{i}");
            docs.push_values(std::iter::once(&i), |mut b, _| {
                b.push_bind(&document)
                    .push_bind(&tenant)
                    .push_bind(&token)
                    .push_bind(&token)
                    .push_bind(Vec::<String>::new())
                    .push_bind("ready")
                    .push_bind(&token)
                    .push_bind(chrono::Utc::now().naive_utc())
                    .push_bind(chrono::Utc::now().naive_utc())
                    .push_bind(true)
                    .push_bind(Vec::<String>::new());
            });
            groups.push_values(std::iter::once(&i), |mut b, _| {
                b.push_bind(format!("{document}-g"))
                    .push_bind(&document)
                    .push_bind(&kb_id)
                    .push_bind(chrono::Utc::now().naive_utc());
            });
            units.push(bench_unit(&tenant, &document, &kb_id, &token, dim, &mut random));
        }
        docs.build()
            .execute(pool)
            .await
            .map_err(|e| CliError::Failed(format!("pg insert documents: {e}")))?;
        groups
            .build()
            .execute(pool)
            .await
            .map_err(|e| CliError::Failed(format!("pg insert document groups: {e}")))?;
        index
            .upsert(units)
            .await
            .map_err(|e| CliError::Failed(format!("qdrant upsert: {e}")))?;
        inserted = end;
    }

    let mut samples = Vec::with_capacity(config.queries);
    let mut dropped = 0.0f64;
    let mut errors = 0usize;
    let mut query_random = Prng::new(0xBEEF ^ size as u64 ^ (run_id.len() as u64));
    for _ in 0..config.queries {
        let token = format!(
            "scopebench{run_id}{size}t{}",
            query_random.below(size)
        );
        let body = json!({"query": token, "maxChunks": 8});
        let start = Instant::now();
        let response = client
            .post(format!("{}/v1/search", config.base_url.trim_end_matches('/')))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", key.plaintext))
            .json(&body)
            .send()
            .await;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        match response {
            Ok(response) if response.status().is_success() => {
                let parsed: Json = response.json().await.unwrap_or(Json::Null);
                let d = parsed["stats"]["scopeVerifyDropped"]
                    .as_u64()
                    .unwrap_or(0) as f64;
                dropped += d;
                samples.push(elapsed);
            }
            _ => {
                errors += 1;
                samples.push(elapsed);
            }
        }
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    if !config.keep {
        cleanup_size(pool, index, &tenant, collection).await?;
    }

    Ok(BenchRow {
        size,
        mode_label: config.mode_label.clone(),
        p50_ms: percentile(&samples, 0.50),
        p95_ms: percentile(&samples, 0.95),
        mean_scope_verify_dropped: if samples.is_empty() {
            0.0
        } else {
            dropped / samples.len() as f64
        },
        queries: config.queries,
        errors,
        tenant_id: tenant,
        skipped: false,
    })
}

/// Remove a synthetic tenant from Postgres and Qdrant.
async fn cleanup_size(
    pool: &PgPool,
    index: &QdrantIndex,
    tenant: &str,
    collection: &str,
) -> Result<(), CliError> {
    for table in ["Document", "ScopeOutbox", "ApiKey", "KnowledgeBase"] {
        let sql = format!("DELETE FROM \"{table}\" WHERE \"tenantId\" = $1");
        query(&sql)
            .bind(tenant)
            .execute(pool)
            .await
            .map_err(|e| CliError::Failed(format!("pg cleanup {table}: {e}")))?;
    }
    let filter = Filter::must([Condition::matches(
        KEY_TENANT_ID,
        MatchValue::Keyword(tenant.to_string()),
    )]);
    index
        .client()
        .delete_points(
            DeletePointsBuilder::new(collection)
                .wait(true)
                .filter(filter),
        )
        .await
        .map_err(|e| CliError::Failed(format!("qdrant cleanup: {e}")))?;
    Ok(())
}

/// Run all configured sizes and merge the rows into `--out/results.json`.
pub async fn run(config: ScopeBenchConfig) -> Result<ResultsFile, CliError> {
    let (settings, collection, dim) = bench_index_settings()?;
    let pool = connect_pool(&PgSettings {
        database_url: config.database_url.clone(),
        ..PgSettings::default()
    })
    .await?;
    let index = QdrantIndex::new_with_collection(settings, collection.clone())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| CliError::Failed(format!("http client: {e}")))?;
    let run_id = format!(
        "{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );

    let mut rows = Vec::new();
    for size in &config.sizes {
        eprintln!("scope-bench: seeding {size} documents ({})…", config.mode_label);
        let row = measure_size(
            &pool,
            &index,
            &client,
            &config,
            *size,
            dim,
            &collection,
            &run_id,
        )
        .await?;
        println!(
            "scope-bench: {} n={} p50 {:.2} ms p95 {:.2} ms dropped {:.2}",
            row.size, row.queries, row.p50_ms, row.p95_ms, row.mean_scope_verify_dropped
        );
        rows.push(row);
    }

    let mut results = ResultsFile {
        harness: crate::cli::program_version(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        collection,
        dim,
        queries: config.queries,
        target_ratio: TARGET_RATIO,
        rows,
        notes: Vec::new(),
    };
    if config.key_admin.is_some() {
        results.notes.push(
            "--key-admin was supplied; keys were still minted in Postgres with create-api-key internals"
                .to_string(),
        );
    }
    results.notes.push(
        "run pass 1 with KB_SCOPE_ID_LIMIT=0 (--mode-label ids), restart with KB_SCOPE_ID_LIMIT=2000 (--mode-label predicate), then re-read report.md"
            .to_string(),
    );
    merge_results(&config.out_dir, &mut results)?;
    std::fs::create_dir_all(&config.out_dir)
        .map_err(|e| CliError::Failed(format!("create out dir: {e}")))?;
    std::fs::write(
        config.out_dir.join("results.json"),
        serde_json::to_string_pretty(&results)
            .map_err(|e| CliError::Failed(format!("serialise results: {e}")))?,
    )
    .map_err(|e| CliError::Failed(format!("write results: {e}")))?;
    std::fs::write(config.out_dir.join("report.md"), render_report(&results))
        .map_err(|e| CliError::Failed(format!("write report: {e}")))?;
    println!("wrote {}", config.out_dir.join("results.json").display());
    println!("wrote {}", config.out_dir.join("report.md").display());
    Ok(results)
}

/// Keep rows from earlier passes; replace only this mode's rows.
fn merge_results(out_dir: &Path, results: &mut ResultsFile) -> Result<(), CliError> {
    let path = out_dir.join("results.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(previous) = serde_json::from_str::<ResultsFile>(&text) else {
        results.notes.push(format!("could not read earlier {}: rows from this pass stand alone", path.display()));
        return Ok(());
    };
    let mut rows: Vec<BenchRow> = previous
        .rows
        .into_iter()
        .filter(|row| row.mode_label != results.mode_label())
        .collect();
    rows.append(&mut results.rows);
    rows.sort_by(|a, b| {
        a.mode_label
            .cmp(&b.mode_label)
            .then_with(|| a.size.cmp(&b.size))
    });
    results.rows = rows;
    Ok(())
}

impl ResultsFile {
    fn mode_label(&self) -> String {
        self.rows
            .first()
            .map_or_else(String::new, |r| r.mode_label.clone())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_as_a_comma_list_and_reject_zero() {
        assert_eq!(
            parse_sizes("1000, 10000,100000").unwrap(),
            vec![1_000, 10_000, 100_000]
        );
        assert!(parse_sizes("0").is_err());
        assert!(parse_sizes("many").is_err());
        assert!(parse_sizes("").is_err());
    }

    #[test]
    fn percentiles_are_nearest_rank_and_handle_empty_slices() {
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&[], 0.95), 0.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
        assert_eq!(percentile(&values, 0.50), 50.0);
        assert_eq!(percentile(&values, 0.95), 95.0);
        assert_eq!(percentile(&values, 1.0), 100.0);
    }

    #[test]
    fn the_prng_is_deterministic_for_both_passes() {
        let mut a = Prng::new(42);
        let mut b = Prng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next(), b.next());
        }
        assert_ne!(Prng::new(42).below(1000), Prng::new(43).below(1000));
    }

    #[test]
    fn the_bench_payload_has_the_shape_the_scope_filter_expects() {
        let mut random = Prng::new(9);
        let unit = bench_unit(
            "tenant-a",
            "doc-1",
            "kb-1",
            "scopebench-token",
            8,
            &mut random,
        );
        assert_eq!(unit.dense.len(), 8);
        let norm = unit.dense.iter().map(|v| v * v).sum::<f32>();
        assert!((norm - 1.0).abs() < 1e-4, "not normalised: {norm}");
        let payload: Payload = unit_payload(&unit);
        let map: HashMap<String, Value> = payload.into();
        assert_eq!(payload_string(&map, KEY_TENANT_ID).as_deref(), Some("tenant-a"));
        assert_eq!(payload_string(&map, KEY_DOCUMENT_ID).as_deref(), Some("doc-1"));
        assert_eq!(payload_string(&map, KEY_CONTENT).as_deref(), Some("scopebench-token"));
        assert!(map.contains_key(KEY_KNOWLEDGE_BASE_ID), "kb array written");
        assert!(map.contains_key(KEY_CHUNK_INDEX));
        assert!(map.contains_key(KEY_PAGE));
        let back = candidate_from(&map, Default::default(), Default::default()).unwrap();
        assert_eq!(back.document_id.as_str(), "doc-1");
        assert_eq!(back.content, "scopebench-token");
    }

    fn results(p95_small: f64, p95_big: f64, label: &str) -> ResultsFile {
        ResultsFile {
            harness: "test".into(),
            timestamp: "now".into(),
            collection: "chunks__test__8".into(),
            dim: 8,
            queries: 200,
            target_ratio: TARGET_RATIO,
            rows: vec![
                BenchRow {
                    size: 1_000,
                    mode_label: label.into(),
                    p50_ms: 1.0,
                    p95_ms: p95_small,
                    mean_scope_verify_dropped: 0.0,
                    queries: 200,
                    errors: 0,
                    tenant_id: "t1".into(),
                    skipped: false,
                },
                BenchRow {
                    size: 100_000,
                    mode_label: label.into(),
                    p50_ms: 1.0,
                    p95_ms: p95_big,
                    mean_scope_verify_dropped: 0.0,
                    queries: 200,
                    errors: 0,
                    tenant_id: "t2".into(),
                    skipped: false,
                },
            ],
            notes: Vec::new(),
        }
    }

    #[test]
    fn the_target_passes_at_1_2x_and_misses_above_it() {
        assert_eq!(verdict(&results(10.0, 12.0, "predicate")).0, "PASS");
        let miss = verdict(&results(10.0, 12.1, "predicate"));
        assert_eq!(miss.0, "MISS");
        assert!(miss.1.contains("1.21x"));
        assert_eq!(verdict(&results(10.0, 12.0, "ids")).0, "NOT_RUN");
    }

    #[test]
    fn the_report_names_the_target_and_every_row() {
        let report = render_report(&results(10.0, 15.0, "predicate"));
        assert!(report.contains("**MISS**"));
        assert!(report.contains("| 100000 | predicate |"));
        assert!(report.contains("KB_SCOPE_ID_LIMIT=2000"));
        assert!(report.contains("1.50x"));
    }

    #[test]
    fn a_zero_dim_unit_is_not_silently_undefined() {
        let mut random = Prng::new(1);
        let unit = bench_unit("t", "d", "kb", "token", 0, &mut random);
        assert!(unit.dense.is_empty());
    }

    #[test]
    fn a_missing_scope_verify_stat_reads_as_zero() {
        // The wire shape may omit `stats` in Ids mode; the JSON read must not
        // turn that into a fail or a fabricated number.
        let body = json!({"chunks": []});
        assert_eq!(body["stats"]["scopeVerifyDropped"].as_u64().unwrap_or(0), 0);
    }
}
