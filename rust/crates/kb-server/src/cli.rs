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

use std::collections::HashMap;
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
         <serve|create-api-key|apply-schema|migrate|reindex|communities|setup|eval|bench|openapi|check-boundary>"
    )]
    Usage(String),
    #[error("{0}")]
    Failed(String),
    /// A child process failed: exit with its own code rather than folding it
    /// into a generic 1 (`setup --start` runs `kb-dev.sh up`).
    #[error("exited with status {0}")]
    Exit(u8),
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
        Some("communities") => crate::communities::run(&args[1..]).await,
        Some("setup") => crate::setup::run(&args[1..]).await,
        Some("check-boundary") => check_boundary(),
        Some("eval") => eval(&args[1..]).await,
        Some("bench") => bench(&args[1..]).await,
        Some("openapi") => openapi(),
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
    kb-server communities build --tenant <id> [--min-pages 2] [--max-usd X] [--dry-run]
                                               Detect graph communities and write one LLM summary
                                               per community for the global arm (DEC-5, off by
                                               default). Prints a chars/4 cost estimate first;
                                               --max-usd 0 refuses any paid run. Every summary is
                                               cached under KB_CACHE_DIR
    kb-server setup [--tier T] [--detect-only] [--check]
                                               Detect the hardware, check the VRAM budget and
                                               model licences, write rust/deploy/.env (see
                                               `kb-server setup --help`)
    kb-server eval --base-url URL --dataset DIR  Measure retrieval quality
    kb-server eval score --qrels F --run F       Score an existing run offline
    kb-server eval compare --baseline F --current F  Compare two runs against a baseline
    kb-server eval classify --queries FILE [--per-query]  Print the query-class distribution of a
                                                 queries.tsv / queries.jsonl file (RET-5)
    kb-server eval ocr --pdfs F --root D --out D  Read the first page of every listed PDF through
                                                 the OCR router and write <out>/<name>.md
                                                 [--html] [--concurrency N]
    kb-server bench search --base-url URL --queries F  Closed-loop load test for /v1/search
    kb-server openapi                           Print the OpenAPI 3.1 document to stdout (the same
                                               document GET /openapi.json serves and the Swagger UI
                                               at GET /docs renders)
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
    // Classify is local and pure, so it stays here rather than in the black-box
    // `kb-eval` crate: that crate is forbidden from linking `kb-core` (the
    // boundary check enforces it), and duplicating the rules would let the
    // reported distribution drift from what the pipeline actually does.
    if args.first().map(String::as_str) == Some("classify") {
        return classify_queries(&args[1..]);
    }
    // `eval ocr` reads pages through the pipeline's own OCR router, so it
    // needs `kb-extract` — a crate the black-box `kb-eval` may not link.
    if args.first().map(String::as_str) == Some("ocr") {
        return crate::eval_ocr::run(&args[1..]).await;
    }
    kb_eval::run(args).await.map_err(|err| match err {
        kb_eval::EvalError::Usage(message) => CliError::Usage(message),
        kb_eval::EvalError::Failed(message) => CliError::Failed(message),
    })
}

/// `kb-server eval classify --queries <file>` — the class distribution of a
/// query file, using the same classifier the pipeline runs.
///
/// A MIRACL `queries.tsv` is `query-id<TAB>text`; a BEIR `queries.jsonl` is one
/// `{"_id": "...", "text": "..."}` per line. A file with neither shape is read
/// as one query per line, so a hand-written list still works.
fn classify_queries(args: &[String]) -> Result<(), CliError> {
    let path = args
        .iter()
        .position(|arg| arg == "--queries")
        .and_then(|index| args.get(index + 1))
        .ok_or_else(|| {
            CliError::Usage(
                "usage: kb-server eval classify --queries <queries.tsv|queries.jsonl>".to_string(),
            )
        })?;
    let text = std::fs::read_to_string(path)
        .map_err(|err| CliError::Failed(format!("read {path}: {err}")))?;
    let queries = parse_query_file(path, &text)?;
    if queries.is_empty() {
        return Err(CliError::Failed(format!("{path}: no queries found")));
    }

    // `--per-query` prints `<id>\t<class>` per query instead of the counts, so
    // offline experiments (EVAL-2's gated tree fusion) can gate on exactly the
    // classifier the pipeline runs.
    if args.iter().any(|arg| arg == "--per-query") {
        for (id, query) in &queries {
            println!("{id}\t{}", kb_core::classify_query(query).class.as_str());
        }
        return Ok(());
    }
    let mut counts: HashMap<kb_core::QueryClass, usize> = HashMap::new();
    for (_id, query) in &queries {
        let class = kb_core::classify_query(query).class;
        *counts.entry(class).or_insert(0) += 1;
    }
    let total = queries.len();
    println!("{path}: {total} queries");
    for class in kb_core::QueryClass::ALL {
        let count = counts.get(&class).copied().unwrap_or(0);
        println!(
            "  {:<12} {:>6}  {:>5.1}%",
            class.as_str(),
            count,
            (count as f64 / total as f64) * 100.0
        );
    }
    Ok(())
}

/// Parse `<id, text>` pairs from a queries file, in the formats the eval
/// datasets use. A malformed JSONL line names its number rather than being
/// skipped: a count over half a file is worse than a failed command.
fn parse_query_file(path: &str, text: &str) -> Result<Vec<(String, String)>, CliError> {
    let lower = path.to_lowercase();
    let mut queries = Vec::new();
    if lower.ends_with(".tsv") {
        for (line_number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let mut fields = line.split('\t');
            let id = fields.next().unwrap_or_default().trim();
            if line_number == 0 && matches!(id, "query-id" | "_id" | "id") {
                continue;
            }
            let query = fields.collect::<Vec<_>>().join("\t");
            let query = query.trim();
            if !query.is_empty() {
                queries.push((id.to_string(), query.to_string()));
            }
        }
    } else if lower.ends_with(".jsonl") || lower.ends_with(".json") {
        for (line_number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line)
                .map_err(|err| CliError::Failed(format!("{path}:{}: {err}", line_number + 1)))?;
            let id = value
                .get("_id")
                .or_else(|| value.get("id"))
                .or_else(|| value.get("queryId"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let query = value
                .get("text")
                .or_else(|| value.get("query"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if !query.trim().is_empty() {
                queries.push((id.to_string(), query.trim().to_string()));
            }
        }
    } else {
        for (line_number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((id, query)) = line.split_once('\t') {
                if !query.trim().is_empty() {
                    queries.push((id.trim().to_string(), query.trim().to_string()));
                }
            } else {
                queries.push((format!("q{}", line_number + 1), line.to_string()));
            }
        }
    }
    Ok(queries)
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

/// `kb-server openapi` — print the OpenAPI document to stdout.
///
/// The document is the same one `GET /openapi.json` serves and the same one
/// `docs/openapi.json` holds, so an operator can diff the contract without
/// starting the server.
fn openapi() -> Result<(), CliError> {
    let document = crate::openapi::spec_json().map_err(|err| CliError::Failed(err.to_string()))?;
    let mut stdout = std::io::stdout().lock();
    write_document(&mut stdout, &document)
}

/// Write the document to a writer.
///
/// The bytes are written rather than printed, because a reader that stops
/// early (`kb-server openapi | head`) closes the pipe, and `println!` turns
/// that into a panic and a backtrace on the operator's terminal. A closed
/// pipe is the reader's decision, not an error of this command.
///
/// The trailing newline keeps `kb-server openapi > docs/openapi.json` a
/// byte-for-byte copy of the committed document.
fn write_document(writer: &mut impl std::io::Write, document: &str) -> Result<(), CliError> {
    let written = writer
        .write_all(document.as_bytes())
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush());
    match written {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(CliError::Failed(err.to_string())),
    }
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
    fn a_tsv_query_file_skips_its_header() {
        let parsed = parse_query_file(
            "queries.tsv",
            "query-id\ttext\nq1\tApa ibu kota?\nq2\tSiapa penulisnya?\n",
        )
        .expect("parse");
        assert_eq!(
            parsed,
            vec![
                ("q1".to_string(), "Apa ibu kota?".to_string()),
                ("q2".to_string(), "Siapa penulisnya?".to_string()),
            ]
        );
    }

    #[test]
    fn a_jsonl_query_file_reads_id_and_text() {
        let parsed = parse_query_file(
            "queries.jsonl",
            r#"{"_id":"q1","text":"chapter 2"}
{"_id":"q2","text":"main themes"}
"#,
        )
        .expect("parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "q1");
        assert_eq!(parsed[1].1, "main themes");
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

    /// A writer that always fails with the given error kind.
    struct Failing(std::io::ErrorKind);

    impl std::io::Write for Failing {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(self.0, "closed"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_openapi_document_is_written_with_a_trailing_newline() {
        // `kb-server openapi > docs/openapi.json` has to be a byte-for-byte
        // copy of the committed document, newline included.
        let mut out: Vec<u8> = Vec::new();
        write_document(&mut out, "{\"openapi\":\"3.1.0\"}").expect("write");
        assert_eq!(
            String::from_utf8(out).expect("utf-8"),
            "{\"openapi\":\"3.1.0\"}\n"
        );
    }

    #[test]
    fn a_reader_that_stops_early_is_not_an_error() {
        // `kb-server openapi | head` closes the pipe. The command must stop
        // quietly: a backtrace on the operator's terminal would read as a
        // crash of the service.
        let mut out = Failing(std::io::ErrorKind::BrokenPipe);
        assert!(write_document(&mut out, "{}").is_ok());
    }

    #[test]
    fn a_broken_stdout_that_is_not_a_pipe_is_still_an_error() {
        let mut out = Failing(std::io::ErrorKind::PermissionDenied);
        let err = write_document(&mut out, "{}").unwrap_err();
        assert!(matches!(err, CliError::Failed(_)), "{err:?}");
    }
}
