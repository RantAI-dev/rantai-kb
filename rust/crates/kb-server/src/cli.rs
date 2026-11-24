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
use std::path::{Path, PathBuf};

use kb_ports::{CreateJobInput, JobStore};
use kb_store_pg::PgJobStore;
use kb_store_pg::api_keys::{NewApiKey, generate_api_key, insert_api_key};

use crate::config::Config;

/// Everything a command can fail with. `Usage` is a mistake by the caller;
/// `Failed` is the operation itself failing.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(
        "{}\n\nusage: {} \
         <serve|up|down|logs|ps|status|key|create-api-key|apply-schema|migrate|reindex|communities|setup|eval|bench|openapi|check-boundary>",
        .0,
        program_name()
    )]
    Usage(String),
    #[error("{0}")]
    Failed(String),
    /// A child process failed: exit with its own code rather than folding it
    /// into a generic 1 (`setup --start` runs the stack's `up`).
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
        None => serve(&[]).await,
        Some("serve") => serve(&args[1..]).await,
        Some("up") => crate::stack::up(&args[1..]).await,
        Some("down") => crate::stack::down(&args[1..]).await,
        Some("logs") => crate::stack::logs(&args[1..]).await,
        Some("ps") => crate::stack::ps(&args[1..]).await,
        Some("status") => crate::stack::status(&args[1..]).await,
        Some("key") => crate::stack::key(&args[1..]).await,
        Some("create-api-key") => create_api_key(&args[1..]).await,
        Some("apply-schema") => apply_schema().await,
        Some("migrate") => migrate().await,
        Some("reindex") => reindex(&args[1..]).await,
        Some("communities") => crate::communities::run(&args[1..]).await,
        Some("setup") => crate::setup::run(&args[1..]).await,
        Some("check-boundary") => check_boundary(),
        Some("eval") => eval(&args[1..]).await,
        Some("bench") => bench(&args[1..]).await,
        Some("openapi") => openapi(&args[1..]),
        Some("-h" | "--help" | "help") => {
            print_help();
            Ok(())
        }
        Some("-V" | "--version" | "version") => {
            println!("{}", program_version());
            Ok(())
        }
        Some(other) => Err(CliError::Usage(format!("unknown command {other:?}"))),
    }
}

/// The version of this build.
///
/// An installed bundle carries the release version in a `VERSION` file next to
/// the binary (`<home>/bin/kb-server`, `<home>/VERSION`), which is what the
/// release published — not the crate version the binary was compiled from.
#[must_use]
pub fn program_version() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(version) = version_near(bin_dir) {
                return version;
            }
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// The release version recorded next to the binary, if there is one.
///
/// The candidates are the ones the release bundle has: `../VERSION` (the
/// bundle root), `VERSION` (a binary that sits in the bundle root) and
/// `../deploy/VERSION`.
#[must_use]
pub fn version_near(bin_dir: &Path) -> Option<String> {
    for candidate in [
        bin_dir.join("../VERSION"),
        bin_dir.join("VERSION"),
        bin_dir.join("../deploy/VERSION"),
    ] {
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            let version = text.trim();
            if !version.is_empty() {
                return Some(version.to_string());
            }
        }
    }
    None
}

fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// The name the operator invoked: `rantai-kb` or `kb-server`.
///
/// The binary is installed under both names, and the help text reads better
/// when it uses the one that was typed.
#[must_use]
pub fn program_name() -> String {
    program_name_from(std::env::args().next().as_deref())
}

/// The program name from an `argv[0]`: the file name, without the path.
///
/// `rantai-kb` and `kb-server` are the same program, so the help must not
/// claim to be one of them.
#[must_use]
pub fn program_name_from(argv0: Option<&str>) -> String {
    argv0
        .and_then(|arg| {
            Path::new(arg)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "kb-server".to_string())
}

fn print_help() {
    let program = program_name();
    println!(
        "{program} — RantAI Knowledge Base service

USAGE
    {program}                                  Serve HTTP and run the ingest worker
    {program} serve                            Same as above
    {program} serve --foreground               Always serve in this process, even when a
                                               deploy directory is configured
    {program} serve --stateless                Run as a stateless document-processing
                                               microservice (zero Postgres, Qdrant, or S3;
                                               also enabled via KB_MODE=stateless)
    {program} setup [tier] [flags]             Detect the hardware, check the VRAM budget and
                                               model licences, write the deploy env file
    {program} up [tier] [--build]              Start the tier's compose stack, wait for /health
    {program} down [-v]                        Stop the stack (-v also removes the volumes)
    {program} logs [--no-follow] [service...]  The stack's logs
    {program} ps                               The stack's containers
    {program} status                           GET /health on the running stack
    {program} key <tenant> [name]              Create and print an API key
    {program} create-api-key <tenantId> [name] [--scopes a,b] [--kb id,id]
    {program} apply-schema                     Create the Qdrant collections at the configured width
    {program} migrate                          Apply prisma/migrations in order
    {program} reindex [--tenant <id>] [--dry-run]
                                               Re-queue ingest for every live document so Qdrant
                                               is rebuilt from the originals in S3 (see below)
    {program} communities build --tenant <id> [--min-pages 2] [--max-usd X] [--dry-run]
                                               Detect graph communities and write one LLM summary
                                               per community for the global arm (DEC-5, off by
                                               default). Prints a chars/4 cost estimate first;
                                               --max-usd 0 refuses any paid run. Every summary is
                                               cached under KB_CACHE_DIR
    {program} eval --base-url URL --dataset DIR  Measure retrieval quality
    {program} eval score --qrels F --run F       Score an existing run offline
    {program} eval compare --baseline F --current F  Compare two runs against a baseline
    {program} eval disclosure --base-url URL --dataset DIR --out DIR
                                                Scripted search -> structure -> content coverage gate
                                                over page-level qrels (EVAL-7)
    {program} eval calibrate-confidence --base-url URL --answerable FILE --offtopic FILE --out DIR
                                                Fit confidence bands; write calibration.json + report.md
    {program} eval classify --queries FILE [--per-query]  Print the query-class distribution of a
                                                  queries.tsv / queries.jsonl file (RET-5)
    {program} eval ocr --pdfs F --root D --out D  Read the first page of every listed PDF through
                                                 the OCR router and write <out>/<name>.md
                                                 [--html] [--concurrency N]
    {program} bench search --base-url URL --queries F  Closed-loop load test for /v1/search
    {program} openapi [--stateless]            Print the OpenAPI 3.1 document to stdout (the same
                                               document GET /openapi.json serves and the Swagger UI
                                               at GET /docs renders)
    {program} check-boundary                   Fail if an inner crate depends on an adapter
    {program} --version                         The version of this build

THE STACK
    `up`, `down`, `logs`, `ps`, `status` and `key` drive the compose stack in the
    deploy directory: $RANTAI_KB_HOME/deploy, else rust/deploy next to a source
    checkout, else ~/.rantai-kb/deploy. `setup` writes that directory's .env and
    .tier; `serve` runs the in-process server when DATABASE_URL is set (the
    container's CMD) and the stack otherwise.

STATELESS PROCESSOR MODE
    `serve --stateless` (or `KB_MODE=stateless`) starts a processor-only HTTP
    microservice (`POST /v1/process`, `POST /v1/encode`, `POST /v1/rerank`,
    `GET /v1/formats`, `GET /health`) with zero Postgres, Qdrant, or S3
    connections. Set `KB_STATELESS_API_KEY=<secret>` (or
    `KB_STATELESS_ALLOW_UNAUTHENTICATED=true` on a private network). Dense
    embeddings and reranking are optional.

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

/// Whether `KB_MODE` asks for stateless mode.
fn env_requests_stateless() -> Result<bool, CliError> {
    match env_lookup("KB_MODE").map(|v| v.trim().to_ascii_lowercase()) {
        None => Ok(false),
        Some(v) if v.is_empty() || v == "full" || v == "stateful" => Ok(false),
        Some(v) if v == "stateless" || v == "processor" => Ok(true),
        Some(other) => Err(CliError::Failed(format!(
            "KB_MODE={other:?} is not supported: expected \"full\" (default) or \"stateless\""
        ))),
    }
}

/// `kb-server serve [--foreground] [--stateless]`.
///
/// With `--stateless` or `KB_MODE=stateless`, starts the stateless
/// processor-only server with zero Postgres, Qdrant, or S3 connections.
/// Otherwise, with `DATABASE_URL` in the environment this process *is* the
/// server; without it, a configured deploy directory means "serve the stack".
async fn serve(args: &[String]) -> Result<(), CliError> {
    let mut foreground = false;
    let mut stateless = false;
    for arg in args {
        match arg.as_str() {
            "--foreground" => foreground = true,
            "--stateless" => stateless = true,
            other => {
                return Err(CliError::Usage(format!(
                    "serve: unknown argument {other:?}; usage: serve [--foreground] [--stateless]"
                )));
            }
        }
    }

    if stateless || env_requests_stateless()? {
        let config = crate::stateless::StatelessConfig::from_env()
            .map_err(|e| CliError::Failed(e.to_string()))?;
        config.log_tier();
        return crate::stateless::run_stateless(config)
            .await
            .map_err(|e| CliError::Failed(e.to_string()));
    }

    if !foreground {
        let stack = crate::stack::current();
        let mode = crate::stack::decide_serve_mode(
            env_lookup("DATABASE_URL").is_some_and(|value| !value.trim().is_empty()),
            crate::stack::is_configured(&stack),
        );
        match mode {
            crate::stack::ServeMode::Foreground => {}
            crate::stack::ServeMode::Stack => return crate::stack::serve_stack().await,
            crate::stack::ServeMode::Unconfigured => {
                return Err(CliError::Failed(format!(
                    "nothing to serve: DATABASE_URL is not set and no deploy directory is \
                     configured ({}). Run `{} setup <tier>` first, set DATABASE_URL to \
                     run the full server, or pass `--stateless` (KB_MODE=stateless) to run \
                     the processor-only microservice.",
                    stack.deploy.path.display(),
                    program_name()
                )));
            }
        }
    }

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
    if tier.is_some() {
        // The collection's name carries the model's slug, so an invalid
        // embedding stack or build would create the collection for the other
        // embedder. Fail like the server does instead of guessing.
        crate::tier::Embedders::from_lookup(&env_lookup)
            .map_err(|e| CliError::Failed(e.to_string()))?;
        crate::tier::EmbedPrecision::from_lookup(&env_lookup)
            .map_err(|e| CliError::Failed(e.to_string()))?;
    }
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

/// `kb-server openapi [--stateless]` — print the OpenAPI document to stdout.
///
/// The document is the same one `GET /openapi.json` serves (and without
/// `--stateless`, the same one `docs/openapi.json` holds), so an operator can
/// diff the contract without starting the server.
fn openapi(args: &[String]) -> Result<(), CliError> {
    let mut stateless = false;
    for arg in args {
        match arg.as_str() {
            "--stateless" => stateless = true,
            other => {
                return Err(CliError::Usage(format!(
                    "openapi: unknown argument {other:?}; usage: openapi [--stateless]"
                )));
            }
        }
    }
    let document = if stateless {
        crate::stateless::stateless_spec_json()
    } else {
        crate::openapi::spec_json()
    }
    .map_err(|err| CliError::Failed(err.to_string()))?;
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

    #[test]
    fn the_program_name_is_the_name_that_was_typed() {
        // The binary is installed as `rantai-kb` and as `kb-server`; the help
        // must use the one the operator typed.
        assert_eq!(
            program_name_from(Some("/usr/local/bin/rantai-kb")),
            "rantai-kb"
        );
        assert_eq!(program_name_from(Some("./bin/kb-server")), "kb-server");
        assert_eq!(program_name_from(Some("kb-server")), "kb-server");
    }

    #[test]
    fn an_unknown_argv0_falls_back_to_the_binary_name() {
        assert_eq!(program_name_from(None), "kb-server");
        assert_eq!(program_name_from(Some("")), "kb-server");
    }

    #[test]
    fn the_bundle_version_is_read_from_the_file_next_to_the_binary() {
        let root = std::env::temp_dir().join(format!("kb-version-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // The bundle's shape: <root>/bin/kb-server and <root>/VERSION.
        let bundle = root.join("bundle");
        let bin_dir = bundle.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        std::fs::write(bundle.join("VERSION"), "0.3.0\n").expect("version file");
        assert_eq!(version_near(&bin_dir), Some("0.3.0".to_string()));

        // A deploy directory carries it too, for a bundle whose binary sits
        // next to the deploy directory rather than above it.
        let flat = root.join("flat");
        let flat_bin = flat.join("bin");
        std::fs::create_dir_all(&flat_bin).expect("bin dir");
        std::fs::create_dir_all(flat.join("deploy")).expect("deploy dir");
        std::fs::write(flat.join("deploy/VERSION"), "0.4.0").expect("version file");
        assert_eq!(version_near(&flat_bin), Some("0.4.0".to_string()));

        // No file at all: the caller falls back to the crate version.
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).expect("empty dir");
        assert_eq!(version_near(&empty), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
