//! Subcommands of the `kb-server` binary.
//!
//! One binary, not a second `kb-cli`: the image ships one artifact, the
//! entrypoint already knows how to exec it, and an operator who has just
//! stopped the old container has exactly one new binary to reason about. The
//! commands are ports of the TypeScript scripts an operator used to run by
//! hand — `create-api-key`, `apply-schema`, `migrate` — plus `check-boundary`,
//! the Rust answer to `scripts/check-kb-boundary.ts`.
//!
//! Argument handling follows `scripts/create-api-key.ts` exactly, including
//! its `--flag value` parsing and its positional/flag split, so muscle memory
//! carries over during cutover.

use std::path::PathBuf;

use kb_ports::{CreateJobInput, JobStore};
use kb_store_pg::PgJobStore;
use kb_store_pg::api_keys::{NewApiKey, generate_api_key, insert_api_key};

use crate::config::Config;

/// Everything a command can fail with. `Usage` is a mistake by the caller;
/// `Failed` is the operation itself failing.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(
        "{0}\n\nusage: kb-server \
         <serve|create-api-key|apply-schema|migrate|reindex|eval|bench|check-boundary>"
    )]
    Usage(String),
    #[error("{0}")]
    Failed(String),
}

impl From<kb_ports::PortError> for CliError {
    fn from(err: kb_ports::PortError) -> Self {
        Self::Failed(err.to_string())
    }
}

/// Where `prisma/migrations` lives. The container sets this explicitly so the
/// entrypoint does not depend on the working directory.
const DEFAULT_MIGRATIONS_DIR: &str = "prisma/migrations";

/// Run the command named by `args[0]`.
///
/// No arguments at all means `serve`, so the process started before the
/// subcommands existed keeps working.
///
/// # Errors
///
/// Returns the command's error; `main` turns it into a non-zero exit.
pub async fn dispatch(args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        None | Some("serve") => serve().await,
        Some("create-api-key") => create_api_key(&args[1..]).await,
        Some("apply-schema") => apply_schema().await,
        Some("migrate") => migrate().await,
        Some("reindex") => reindex(&args[1..]).await,
        Some("check-boundary") => check_boundary(),
        Some("eval") => eval(&args[1..]).await,
        Some("bench") => bench(&args[1..]).await,
        Some("-h" | "--help" | "help") => {
            print_help();
            Ok(())
        }
        Some(other) => Err(CliError::Usage(format!("unknown command {other:?}"))),
    }
}

fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn print_help() {
    println!(
        "kb-server — RantAI Knowledge Base service

USAGE
    kb-server                                  Serve HTTP and run the ingest worker
    kb-server serve                            Same as above
    kb-server create-api-key <tenantId> [name] [--scopes a,b] [--kb id,id]
    kb-server apply-schema                     Create the Qdrant collections at the configured width
    kb-server migrate                          Apply prisma/migrations in order
    kb-server reindex [--tenant <id>] [--dry-run]
                                               Re-queue ingest for every live document so Qdrant
                                               is rebuilt from the originals in S3 (see below)
    kb-server eval --base-url URL --dataset DIR  Measure retrieval quality
    kb-server eval score --qrels F --run F       Score an existing run offline
    kb-server eval compare --baseline F --current F  Compare two runs against a baseline
    kb-server bench search --base-url URL --queries F  Closed-loop load test for /v1/search
    kb-server check-boundary                   Fail if an inner crate depends on an adapter

REINDEX AND COST
    SurrealDB was removed in 2026-10-25; Qdrant is the only chunk store. Data
    written by the Bun service is still in Postgres and S3, so `reindex` queues
    one ingest job per live document and the worker rebuilds the Qdrant index.
    `--dry-run` only prints the per-tenant counts.
    On paid tiers this re-embeds every chunk: it costs money unless the
    embedding disk cache (KB_CACHE_DIR) already holds those inputs.

ENVIRONMENT
    DATABASE_URL, QDRANT_GRPC_URL, KB_EMBEDDING_DIM and the rest are read from the
    environment exactly as they are for `serve`; see rust/README.md."
    );
}

async fn serve() -> Result<(), CliError> {
    let config = Config::from_env().map_err(|e| CliError::Failed(e.to_string()))?;
    // One INFO line saying what `KB_TIER` filled, if a tier is set.
    config.log_tier();
    // `run` is the same call the pre-subcommand binary made; the process only
    // returns when the listener stops.
    crate::run(config)
        .await
        .map_err(|e| CliError::Failed(e.to_string()))
}

/// `kb-server migrate` — apply `prisma/migrations/*/migration.sql`, recording
/// each in `_prisma_migrations` the way Prisma records it.
async fn migrate() -> Result<(), CliError> {
    let dir = migrations_dir();
    let settings =
        Config::pg_from_lookup(env_lookup).map_err(|e| CliError::Failed(e.to_string()))?;
    let pool = kb_store_pg::connect_pool(&settings).await?;

    let report = kb_store_pg::migrate::apply(&pool, &dir).await?;
    for name in &report.skipped {
        println!("already applied: {name}");
    }
    for name in &report.applied {
        println!("applied: {name}");
    }
    println!(
        "migrate: {} applied, {} already present ({})",
        report.applied.len(),
        report.skipped.len(),
        dir.display()
    );
    Ok(())
}

/// The directory holding the migration folders.
fn migrations_dir() -> PathBuf {
    match env_lookup("KB_MIGRATIONS_DIR") {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(DEFAULT_MIGRATIONS_DIR),
    }
}

/// `kb-server reindex [--tenant <id>] [--dry-run]`.
///
/// Qdrant is the only chunk store, and data written by Bun lives in Postgres
/// (documents) and S3 (originals). Reindex re-queues every live document so the
/// existing ingest worker rebuilds the chunks, vectors and BM25 index. It
/// deliberately reuses the upload path's job shape rather than a second ingest.
async fn reindex(args: &[String]) -> Result<(), CliError> {
    let parsed = ReindexArgs::parse(args)?;
    let settings =
        Config::pg_from_lookup(env_lookup).map_err(|e| CliError::Failed(e.to_string()))?;
    let pool = kb_store_pg::connect_pool(&settings).await?;
    let documents = kb_store_pg::reindex::list_documents(&pool, parsed.tenant.as_deref()).await?;
    let plan = kb_store_pg::reindex::plan(&documents);
    print_reindex_plan(&plan, parsed.dry_run);

    if parsed.dry_run {
        return Ok(());
    }

    let jobs = PgJobStore::from_pool(pool);
    let mut queued = 0usize;
    for document in documents.iter().filter(|doc| doc.is_queueable()) {
        let Some(filename) = document.filename() else {
            // `is_queueable` already requires an s3 key; this is belt and braces
            // because a job without a filename cannot be extracted.
            continue;
        };
        jobs.create_job(&CreateJobInput {
            tenant_id: document.tenant_id.clone(),
            user_id: None,
            filename,
            file_size: document.file_size,
            mime_type: document.mime_type.clone(),
            s3_key: document.s3_key.clone(),
            document_id: Some(document.document_id.clone()),
            params: document.job_params(),
        })
        .await?;
        queued += 1;
        if queued % 100 == 0 {
            println!("reindex: queued {queued}…");
        }
    }
    println!(
        "reindex: queued {queued} ingest job(s) across {} tenant(s); \
         the worker rebuilds Qdrant from S3 as it claims them",
        plan.len()
    );
    Ok(())
}

fn print_reindex_plan(
    plan: &std::collections::BTreeMap<String, kb_store_pg::reindex::ReindexTenantCount>,
    dry_run: bool,
) {
    println!(
        "reindex: {}",
        if dry_run {
            "dry run — no jobs will be created"
        } else {
            "queuing ingest jobs"
        }
    );
    if plan.is_empty() {
        println!("  (no live documents)");
        return;
    }
    println!(
        "  {:<32} {:>10} {:>8} {:>12} {:>12}",
        "tenant", "documents", "queued", "no original", "active job"
    );
    let mut totals = kb_store_pg::reindex::ReindexTenantCount::default();
    for (tenant, count) in plan {
        println!(
            "  {:<32} {:>10} {:>8} {:>12} {:>12}",
            tenant,
            count.documents,
            count.queued,
            count.skipped_without_original,
            count.skipped_active_job,
        );
        totals.documents += count.documents;
        totals.queued += count.queued;
        totals.skipped_without_original += count.skipped_without_original;
        totals.skipped_active_job += count.skipped_active_job;
    }
    println!(
        "  {:<32} {:>10} {:>8} {:>12} {:>12}",
        "(total)",
        totals.documents,
        totals.queued,
        totals.skipped_without_original,
        totals.skipped_active_job,
    );
}

/// `kb-server apply-schema` — create the Qdrant collections this config writes.
///
/// Qdrant creates a collection with its vector width fixed, so the width and
/// the model slug are read here through the same `KB_TIER` overlay the server
/// uses. The call is idempotent: an existing collection is left alone.
async fn apply_schema() -> Result<(), CliError> {
    let tier =
        crate::tier::Tier::from_lookup(&env_lookup).map_err(|e| CliError::Failed(e.to_string()))?;
    let lookup = crate::tier::with_preset(tier, &env_lookup);
    let model = lookup("KB_EMBEDDING_MODEL")
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "qwen/qwen3-embedding-8b".to_string());
    let (settings, collection_override) = Config::qdrant_from_lookup(&lookup, &model);
    let collection = collection_override.unwrap_or_else(|| settings.collection_name());
    let client = settings
        .client()
        .map_err(|e| CliError::Failed(e.to_string()))?;
    kb_store_qdrant::create_collection(&client, &settings, &collection).await?;
    println!(
        "[kb-schema] Qdrant collection {collection} ready (model {model}, dense {} dims)",
        settings.dense_dims.unwrap_or(0)
    );

    // The page-image arm has its own collection per page-embedding model, and
    // the server creates it on boot too; creating it here means a first boot
    // after a model change does not have to be the step that does DDL.
    if let Some(page_model) = lookup("KB_PAGE_EMBEDDING_MODEL")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        let page_dim = lookup("KB_PAGE_EMBEDDING_DIM")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|dims| *dims > 0)
            .unwrap_or(1024);
        let page_settings = kb_store_qdrant::QdrantSettings {
            model_slug: page_model.clone(),
            dense_dims: Some(page_dim),
            ..settings.clone()
        };
        let page_collection = format!("pages__{}", page_settings.collection_name());
        kb_store_qdrant::create_collection(&client, &page_settings, &page_collection).await?;
        println!(
            "[kb-schema] Qdrant page collection {page_collection} ready (model {page_model}, dense {page_dim} dims)"
        );
    }
    Ok(())
}

/// `kb-server eval …` — the retrieval-quality harness, from the `kb-eval` crate.
async fn eval(args: &[String]) -> Result<(), CliError> {
    kb_eval::run(args).await.map_err(|err| match err {
        kb_eval::EvalError::Usage(message) => CliError::Usage(message),
        kb_eval::EvalError::Failed(message) => CliError::Failed(message),
    })
}

/// `kb-server bench search …` — the closed-loop load test. Same crate as the
/// evaluation harness and the same reason it is a dependency: the load test has
/// to measure the deployed service from the outside.
async fn bench(args: &[String]) -> Result<(), CliError> {
    kb_eval::run_bench(args).await.map_err(|err| match err {
        kb_eval::bench::BenchError::Usage(message) => CliError::Usage(message),
        kb_eval::bench::BenchError::Failed(message) => CliError::Failed(message),
    })
}

/// `kb-server check-boundary` — the layering rule as an executable check.
fn check_boundary() -> Result<(), CliError> {
    let root = crate::boundary::workspace_root();
    match crate::boundary::check(&root) {
        Ok(violations) if violations.is_empty() => {
            println!(
                "✓ KB boundary clean: {} inner crates, {} adapter crates",
                crate::boundary::INNER_CRATES.len(),
                crate::boundary::ADAPTER_CRATES.len()
            );
            Ok(())
        }
        Ok(violations) => {
            eprintln!(
                "\n✗ KB boundary: {} forbidden dependency(ies)\n",
                violations.len()
            );
            for violation in &violations {
                eprintln!("  {}", violation);
                eprintln!(
                    "    {}",
                    if crate::boundary::INNER_CRATES.contains(&violation.crate_name.as_str()) {
                        "an inner crate must reach adapters through kb-ports"
                    } else {
                        "a black-box client must measure the service over HTTP only"
                    }
                );
            }
            Err(CliError::Failed("KB boundary violated".to_string()))
        }
        Err(err) => Err(CliError::Failed(err)),
    }
}

/// `kb-server create-api-key <tenantId> [name] [--scopes a,b] [--kb id,id]`.
async fn create_api_key(args: &[String]) -> Result<(), CliError> {
    let parsed = CreateKeyArgs::parse(args)?;
    let settings =
        Config::pg_from_lookup(env_lookup).map_err(|e| CliError::Failed(e.to_string()))?;
    let pool = kb_store_pg::connect_pool(&settings).await?;

    let key = generate_api_key()?;
    insert_api_key(
        &pool,
        &key,
        &NewApiKey {
            tenant_id: &parsed.tenant_id,
            name: &parsed.name,
            scopes: &parsed.scopes,
            knowledge_base_ids: &parsed.knowledge_base_ids,
        },
    )
    .await?;

    println!();
    println!("  API key created — copy it now, it will not be shown again:");
    println!();
    println!("    {}", key.plaintext);
    println!();
    println!("  id:      {}", key.id);
    println!("  tenant:  {}", parsed.tenant_id);
    println!("  scopes:  {}", join_or(&parsed.scopes, "(all)"));
    println!(
        "  KBs:     {}",
        join_or(&parsed.knowledge_base_ids, "(all in tenant)")
    );
    println!();
    Ok(())
}

fn join_or(values: &[String], empty: &str) -> String {
    if values.is_empty() {
        empty.to_string()
    } else {
        values.join(", ")
    }
}

/// Parsed `reindex` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReindexArgs {
    /// Restrict to one tenant. `None` means every tenant.
    pub tenant: Option<String>,
    /// Print the counts and create nothing.
    pub dry_run: bool,
}

impl ReindexArgs {
    /// Parse `[--tenant <id>] [--dry-run]`. No positionals: a bare argument is
    /// a usage error rather than a silently ignored tenant name.
    pub fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut tenant: Option<String> = None;
        let mut dry_run = false;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--tenant" => {
                    let value = args
                        .get(index + 1)
                        .ok_or_else(|| CliError::Usage("--tenant needs a tenant id".to_string()))?;
                    if value.starts_with("--") || value.trim().is_empty() {
                        return Err(CliError::Usage("--tenant needs a tenant id".to_string()));
                    }
                    if tenant.replace(value.clone()).is_some() {
                        return Err(CliError::Usage(
                            "--tenant may be given only once".to_string(),
                        ));
                    }
                    index += 2;
                }
                "--dry-run" => {
                    dry_run = true;
                    index += 1;
                }
                other => {
                    return Err(CliError::Usage(format!(
                        "unknown reindex argument {other:?}; expected [--tenant <id>] [--dry-run]"
                    )));
                }
            }
        }
        Ok(Self { tenant, dry_run })
    }
}

/// Parsed `create-api-key` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateKeyArgs {
    pub tenant_id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub knowledge_base_ids: Vec<String>,
}

impl CreateKeyArgs {
    /// Parse the script's argument shape: positionals are the arguments that
    /// do not begin with `--`, and a flag's value is the argument right after
    /// it. A flag at the end of the line, or followed by another flag, has no
    /// value and is read as absent — the same outcome `flag()` produces.
    pub fn parse(args: &[String]) -> Result<Self, CliError> {
        let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
        let tenant_id = positional
            .first()
            .map(|s| s.as_str())
            .ok_or_else(|| CliError::Usage("missing tenant id".to_string()))?;
        let name = positional
            .get(1)
            .map(|s| s.as_str())
            .unwrap_or("default")
            .to_string();

        Ok(Self {
            tenant_id: tenant_id.to_string(),
            name,
            scopes: flag(args, "--scopes"),
            knowledge_base_ids: flag(args, "--kb"),
        })
    }
}

/// Comma-separated value following `name`, trimmed, blanks dropped.
fn flag(args: &[String], name: &str) -> Vec<String> {
    let Some(idx) = args.iter().position(|a| a == name) else {
        return Vec::new();
    };
    let Some(value) = args.get(idx + 1) else {
        return Vec::new();
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn tenant_and_name_are_positional() {
        let parsed = CreateKeyArgs::parse(&args(&["acme", "ci-key"])).expect("parse");
        assert_eq!(parsed.tenant_id, "acme");
        assert_eq!(parsed.name, "ci-key");
        assert!(parsed.scopes.is_empty());
        assert!(parsed.knowledge_base_ids.is_empty());
    }

    #[test]
    fn name_defaults_to_default() {
        let parsed = CreateKeyArgs::parse(&args(&["acme"])).expect("parse");
        assert_eq!(parsed.name, "default");
    }

    #[test]
    fn flags_are_split_and_trimmed() {
        let parsed = CreateKeyArgs::parse(&args(&[
            "acme",
            "n",
            "--scopes",
            "kb:read, kb:write",
            "--kb",
            "kb_a,kb_b",
        ]))
        .expect("parse");
        assert_eq!(parsed.scopes, ["kb:read", "kb:write"]);
        assert_eq!(parsed.knowledge_base_ids, ["kb_a", "kb_b"]);
    }

    #[test]
    fn a_trailing_flag_reads_as_absent() {
        // Trailing flag: there is no argument after it, so it has no value and
        // the key is unrestricted — the same as the TypeScript `flag()`.
        let parsed = CreateKeyArgs::parse(&args(&["acme", "--scopes"])).expect("parse");
        assert!(parsed.scopes.is_empty());
        let parsed = CreateKeyArgs::parse(&args(&["acme", "n", "--kb"])).expect("parse");
        assert!(parsed.knowledge_base_ids.is_empty());
    }

    #[test]
    fn a_flag_reads_the_next_argument_verbatim_even_when_it_is_a_flag() {
        // `args[idx + 1]`, exactly as the TypeScript helper does: a value that
        // happens to start with `--` is taken at face value. Parity beats
        // convenience here — an operator's muscle memory is the script's
        // behaviour, not a stricter reading of it.
        let parsed =
            CreateKeyArgs::parse(&args(&["acme", "n", "--kb", "--scopes"])).expect("parse");
        assert_eq!(parsed.knowledge_base_ids, ["--scopes"]);
        assert!(parsed.scopes.is_empty());
        assert_eq!(parsed.tenant_id, "acme");
        assert_eq!(parsed.name, "n");
    }

    #[test]
    fn positionals_are_the_arguments_without_a_leading_dash_pair() {
        // The TypeScript positional filter is `!a.startsWith("--")`, so a
        // flag *value* is also a positional. Ported as-is.
        let parsed = CreateKeyArgs::parse(&args(&["--scopes", "kb:read"])).expect("parse");
        assert_eq!(parsed.tenant_id, "kb:read");
        assert_eq!(parsed.scopes, ["kb:read"]);
    }

    #[test]
    fn a_missing_tenant_is_a_usage_error() {
        let err = CreateKeyArgs::parse(&args(&[])).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)));
    }

    #[test]
    fn usage_errors_mention_the_command_shape() {
        let err = CliError::Usage("missing tenant id".to_string());
        assert!(err.to_string().contains("create-api-key") || err.to_string().contains("usage"));
    }
}
