//! `kb-server communities build` — the operator command behind `DEC-5`.
//!
//! Graph community summaries ("global mode") are a **paid LLM batch**, so
//! nothing here is automatic: ingest never writes a summary, and the search
//! pipeline never triggers a build. An operator runs this command, reads the
//! estimate it prints, and passes `--max-usd` to allow the spend. Every response
//! is cached on disk under `KB_CACHE_DIR` keyed by `model + sha256(prompt)`, so
//! re-running a build pays only for prompts that are new (AGENTS rule 7).
//!
//! # What it reads
//!
//! The entity graph ingest already stores: `KbEntity` + `KbMention` in Postgres,
//! written only when entity extraction is on (`KB_ENTITY_EXTRACTION_ENABLED`,
//! `KB_ENTITY_EXTRACTOR=heuristic|llm`) and the graph store is Postgres
//! (`KB_GRAPH_STORE=pg`). Without those there is no graph, so the command says
//! so and exits without writing. Chunk text comes from the Qdrant chunk store,
//! because that is where the chunks live; no embedding or model is involved in
//! reading it.
//!
//! # What it writes
//!
//! One `KbCommunity` row per community, replacing the tenant's previous set in
//! one transaction. `--dry-run` writes nothing and calls no model.
//!
//! # Usage
//!
//! ```text
//! kb-server communities build --tenant <id> [--min-pages 2] [--max-usd X]
//!                             [--dry-run] [--model M] [--max-tokens N]
//!                             [--price-input USD_PER_M] [--price-output USD_PER_M]
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use kb_cache::{CacheKey, DiskCache};
use kb_core::community::{Community, DetectOptions};
use kb_inference::{CommunitySummariser, CommunitySummarySettings, LlmCommunitySummariser};
use kb_ports::{CommunityStore, DocumentId, NewCommunity, RetrievalIndex, TenantId};
use kb_store_pg::PgCommunityStore;

use crate::cli::CliError;
use crate::config::Config;

/// Cache namespace, separate from embeddings and extraction so a purge of one
/// never invalidates another.
const CACHE_NAMESPACE: &str = "community-summaries";

/// USD per 1,000,000 tokens, read from the providers' public price list on
/// 2026-11-04. Only used for the *estimate*; the invoice is the real number.
/// A `:free` model id is priced at zero without consulting this table.
const PRICES: &[(&str, (f64, f64))] = &[
    ("deepseek/deepseek-v4-flash", (0.091, 0.182)),
    ("qwen/qwen3.7-flash", (0.03, 0.13)),
    ("google/gemini-3.8-flash", (0.825, 4.125)),
    ("qwen/qwen3.5-flash-02-23", (0.065, 0.26)),
    ("google/gemini-2.5-flash", (0.30, 2.50)),
];

/// Everything the planner needs, gathered from storage.
#[derive(Debug, Clone, Default)]
pub struct BuildInput {
    /// Chunk id → entity keys mentioned in that chunk.
    pub chunk_entities: BTreeMap<String, Vec<String>>,
    /// Entity key → documents that mention it (the hub cap's denominator).
    pub entity_documents: BTreeMap<String, BTreeSet<String>>,
    /// Entity key → display name.
    pub entity_names: BTreeMap<String, String>,
    /// Chunk id → text, for the prompt excerpts.
    pub chunk_text: BTreeMap<String, String>,
}

/// One community ready to be summarised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCommunity {
    pub entity_keys: Vec<String>,
    pub chunk_ids: Vec<String>,
    pub prompt: String,
}

/// What a build would cost, before any call.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Estimate {
    pub communities: usize,
    pub cached: usize,
    /// Prompt tokens, `chars / 4` — an estimate, never a measurement.
    pub prompt_tokens: usize,
    /// Completion budget (`communities × max_tokens`), the ceiling the provider
    /// may bill.
    pub completion_tokens: usize,
    /// Estimated USD for the uncached remainder. Cached prompts are free.
    pub usd: f64,
}

/// What the summary pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SummaryRun {
    /// Communities summarised, cached or fresh.
    pub summaries: Vec<(PlannedCommunity, String)>,
    pub from_cache: usize,
    pub fresh: usize,
    /// Communities that produced no summary. An empty answer is counted here
    /// and never cached.
    pub failures: Vec<String>,
}

/// Parsed `communities build` arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildArgs {
    pub tenant: String,
    /// Communities with fewer member chunks are not summarised (the prototype's
    /// `--min-pages 2`; the unit here is a chunk).
    pub min_pages: usize,
    /// Hard ceiling in USD. `0` refuses any paid run.
    pub max_usd: f64,
    pub dry_run: bool,
    pub model: Option<String>,
    pub max_tokens: u32,
    pub chars_per_chunk: usize,
    pub max_df_fraction: f32,
    pub price_input: Option<f64>,
    pub price_output: Option<f64>,
}

impl Default for BuildArgs {
    fn default() -> Self {
        Self {
            tenant: String::new(),
            min_pages: 2,
            max_usd: 0.0,
            dry_run: false,
            model: None,
            max_tokens: 320,
            chars_per_chunk: 1200,
            max_df_fraction: 0.05,
            price_input: None,
            price_output: None,
        }
    }
}

impl BuildArgs {
    /// Parse `build --tenant <id> [flags]`.
    pub fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut parsed = Self::default();
        let mut index = 0;
        while index < args.len() {
            let flag = args[index].as_str();
            match flag {
                "--tenant" => {
                    parsed.tenant = arg_value(args, index, flag)?;
                    index += 2;
                }
                "--min-pages" => {
                    parsed.min_pages = parse_usize(flag, &arg_value(args, index, flag)?)?;
                    index += 2;
                }
                "--max-usd" => {
                    parsed.max_usd = parse_f64(flag, &arg_value(args, index, flag)?)?;
                    index += 2;
                }
                "--dry-run" => {
                    parsed.dry_run = true;
                    index += 1;
                }
                "--model" => {
                    parsed.model = Some(arg_value(args, index, flag)?);
                    index += 2;
                }
                "--max-tokens" => {
                    let tokens = parse_usize(flag, &arg_value(args, index, flag)?)?;
                    parsed.max_tokens = u32::try_from(tokens)
                        .map_err(|_| CliError::Usage("--max-tokens is out of range".to_string()))?;
                    index += 2;
                }
                "--chars-per-chunk" => {
                    parsed.chars_per_chunk = parse_usize(flag, &arg_value(args, index, flag)?)?;
                    index += 2;
                }
                "--max-df-fraction" => {
                    parsed.max_df_fraction =
                        parse_f64(flag, &arg_value(args, index, flag)?)? as f32;
                    index += 2;
                }
                "--price-input" => {
                    parsed.price_input = Some(parse_f64(flag, &arg_value(args, index, flag)?)?);
                    index += 2;
                }
                "--price-output" => {
                    parsed.price_output = Some(parse_f64(flag, &arg_value(args, index, flag)?)?);
                    index += 2;
                }
                other => {
                    return Err(CliError::Usage(format!(
                        "unknown communities build argument {other:?}; expected \
                         [--tenant <id>] [--min-pages N] [--max-usd X] [--dry-run] \
                         [--model M] [--max-tokens N] [--chars-per-chunk N] \
                         [--max-df-fraction F] [--price-input X] [--price-output X]"
                    )));
                }
            }
        }
        if parsed.tenant.trim().is_empty() {
            return Err(CliError::Usage(
                "communities build needs --tenant <id>".to_string(),
            ));
        }
        if parsed.max_usd < 0.0 {
            return Err(CliError::Usage(
                "--max-usd must not be negative".to_string(),
            ));
        }
        Ok(parsed)
    }
}

fn arg_value(args: &[String], index: usize, flag: &str) -> Result<String, CliError> {
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("{flag} needs a value")))
}

fn parse_usize(flag: &str, value: &str) -> Result<usize, CliError> {
    value.trim().parse::<usize>().map_err(|_| {
        CliError::Usage(format!(
            "{flag} needs a non-negative integer, got {value:?}"
        ))
    })
}

fn parse_f64(flag: &str, value: &str) -> Result<f64, CliError> {
    value
        .trim()
        .parse::<f64>()
        .map_err(|_| CliError::Usage(format!("{flag} needs a number, got {value:?}")))
}

/// Turn the gathered graph into summarisable communities.
///
/// Pure: no I/O, no model. The prompt is built here so the cache key is a
/// function of the input and two builds of the same graph pay once.
#[must_use]
pub fn plan(
    input: &BuildInput,
    options: DetectOptions,
    chars_per_chunk: usize,
) -> (Vec<PlannedCommunity>, Vec<String>) {
    let (communities, hubs) =
        kb_core::community::detect(&input.chunk_entities, &input.entity_documents, options);
    let plans = communities
        .into_iter()
        .map(
            |Community {
                 entity_keys,
                 chunk_ids,
             }| {
                let names: Vec<String> = entity_keys
                    .iter()
                    .map(|key| {
                        input
                            .entity_names
                            .get(key)
                            .cloned()
                            .unwrap_or_else(|| key.clone())
                    })
                    .collect();
                let excerpts: Vec<(String, String)> = chunk_ids
                    .iter()
                    .map(|chunk| {
                        (
                            chunk.clone(),
                            input.chunk_text.get(chunk).cloned().unwrap_or_default(),
                        )
                    })
                    .collect();
                PlannedCommunity {
                    prompt: kb_core::community::summary_prompt(&names, &excerpts, chars_per_chunk),
                    entity_keys,
                    chunk_ids,
                }
            },
        )
        .collect();
    (plans, hubs)
}

/// The estimate for one model and price pair.
///
/// Reads the cache, so a second build reports the truth: cached prompts are
/// free and the ceiling compares against what would actually be spent.
pub async fn estimate(
    plans: &[PlannedCommunity],
    model: &str,
    cache: &DiskCache,
    prices: (f64, f64),
    max_tokens: u32,
) -> Estimate {
    let mut estimate = Estimate {
        communities: plans.len(),
        ..Estimate::default()
    };
    for plan in plans {
        let key = CacheKey::from_parts(&[model.as_bytes(), plan.prompt.as_bytes()]);
        let cached = cache
            .get(CACHE_NAMESPACE, &key)
            .await
            .is_some_and(|bytes| !bytes.is_empty());
        // `chars / 4`, the prototype's estimate and the README's stated method.
        let prompt_tokens = (plan.prompt.len() / 4).max(1);
        estimate.prompt_tokens += prompt_tokens;
        estimate.completion_tokens += max_tokens as usize;
        if cached {
            estimate.cached += 1;
            continue;
        }
        #[allow(clippy::cast_precision_loss)]
        let cost = prompt_tokens as f64 / 1_000_000.0 * prices.0
            + f64::from(max_tokens) / 1_000_000.0 * prices.1;
        estimate.usd += cost;
    }
    estimate
}

/// Summarise every planned community, cache-first.
///
/// The order of operations is the whole safety argument:
///
/// 1. estimate (reads the cache);
/// 2. `--dry-run` stops here, before any call and before any write;
/// 3. a real run refuses to start when the uncached estimate exceeds
///    `max_usd`;
/// 4. only then does the first HTTP request happen.
///
/// An empty answer is a failure and is never cached, so the next run retries it
/// instead of serving a hole.
///
/// This is the composed helper the tests drive. The CLI calls [`estimate`],
/// prints it, and then [`summarise_after_estimate`], so the operator sees the
/// number *before* the first request rather than after the batch.
pub async fn summarise(
    plans: &[PlannedCommunity],
    summariser: &dyn CommunitySummariser,
    cache: &DiskCache,
    prices: (f64, f64),
    max_tokens: u32,
    max_usd: f64,
    dry_run: bool,
) -> Result<(Estimate, SummaryRun), CliError> {
    let estimate = estimate(plans, summariser.model(), cache, prices, max_tokens).await;
    if dry_run {
        return Ok((estimate, SummaryRun::default()));
    }
    let run = summarise_after_estimate(plans, summariser, cache, max_usd, estimate.usd).await?;
    Ok((estimate, run))
}

/// [`summarise`] after the estimate has been printed.
///
/// Re-checks the ceiling rather than trusting the caller: this is the last
/// function before the first HTTP request, and the refusal has to be here for
/// "aborting before any call" to be true no matter who calls it.
pub async fn summarise_after_estimate(
    plans: &[PlannedCommunity],
    summariser: &dyn CommunitySummariser,
    cache: &DiskCache,
    max_usd: f64,
    estimated_usd: f64,
) -> Result<SummaryRun, CliError> {
    if estimated_usd > max_usd + 1e-9 {
        return Err(CliError::Failed(format!(
            "estimated ${estimated_usd:.4} exceeds --max-usd ${max_usd:.4}; aborting before any \
             call"
        )));
    }

    let mut run = SummaryRun::default();
    for plan in plans {
        let key = CacheKey::from_parts(&[summariser.model().as_bytes(), plan.prompt.as_bytes()]);
        if let Some(bytes) = cache.get(CACHE_NAMESPACE, &key).await {
            if let Ok(text) = String::from_utf8(bytes) {
                if !text.trim().is_empty() {
                    run.from_cache += 1;
                    run.summaries.push((plan.clone(), text));
                    continue;
                }
            }
        }
        match summariser.summarise(&plan.prompt).await {
            Ok(Some(text)) if !text.trim().is_empty() => {
                cache.put(CACHE_NAMESPACE, &key, text.as_bytes()).await;
                run.fresh += 1;
                run.summaries.push((plan.clone(), text));
            }
            Ok(_) => {
                // An empty completion is a reasoning budget spent on thinking,
                // not a summary. Never cache it.
                run.failures.push(plan.chunk_ids.join(","));
            }
            Err(error) => {
                tracing::warn!(error = %error, "community summary failed");
                run.failures.push(plan.chunk_ids.join(","));
            }
        }
    }
    Ok(run)
}

/// The price pair for a model: flags, then env, then the built-in table.
///
/// `None` means "unknown", and an unknown price cannot be estimated. A real run
/// refuses; a dry run is allowed only with `--max-usd 0`, where it can still
/// print token counts and say the cost is unknown.
#[must_use]
pub fn resolve_prices(
    model: &str,
    price_input: Option<f64>,
    price_output: Option<f64>,
    env_input: Option<f64>,
    env_output: Option<f64>,
) -> Option<(f64, f64)> {
    if let (Some(input), Some(output)) = (price_input, price_output) {
        return Some((input, output));
    }
    if model.ends_with(":free") {
        return Some((0.0, 0.0));
    }
    if let (Some(input), Some(output)) = (env_input, env_output) {
        return Some((input, output));
    }
    PRICES
        .iter()
        .find(|(name, _)| *name == model)
        .map(|(_, prices)| *prices)
}

/// The cache directory for summaries: `KB_CACHE_DIR`, then the legacy
/// `KB_STACK_CACHE_DIR`. `None` means a real build must refuse: paying for a
/// summary that cannot be reused is the exact waste AGENTS rule 7 forbids.
#[must_use]
pub fn cache_dir_from_lookup<F>(lookup: F) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<String>,
{
    lookup("KB_CACHE_DIR")
        .or_else(|| lookup("KB_STACK_CACHE_DIR"))
        .map(|dir| dir.trim().to_string())
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// `kb-server communities …`.
pub async fn run(args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        Some("build") => run_build(&args[1..]).await,
        other => Err(CliError::Usage(format!(
            "unknown communities command {other:?}; expected `communities build --tenant <id>`"
        ))),
    }
}

async fn run_build(args: &[String]) -> Result<(), CliError> {
    let parsed = BuildArgs::parse(args)?;
    let tenant = TenantId::new_unchecked(parsed.tenant.clone());
    let settings = Config::pg_from_lookup(|key| std::env::var(key).ok())
        .map_err(|e| CliError::Failed(e.to_string()))?;
    let pool = kb_store_pg::connect_pool(&settings).await?;
    let store = PgCommunityStore::from_pool(pool);
    let mentions = store.tenant_mentions(&tenant).await?;
    if mentions.is_empty() {
        println!(
            "communities: no entity mentions for tenant {}. Entity extraction must be on \
             (KB_ENTITY_EXTRACTION_ENABLED, KB_ENTITY_EXTRACTOR=heuristic|llm) and the graph \
             store must be Postgres (KB_GRAPH_STORE=pg); nothing was written.",
            parsed.tenant
        );
        return Ok(());
    }

    let mut input = BuildInput::default();
    for mention in &mentions {
        input
            .chunk_entities
            .entry(mention.chunk_id.clone())
            .or_default()
            .push(mention.entity_key.clone());
        input
            .entity_documents
            .entry(mention.entity_key.clone())
            .or_default()
            .insert(mention.document_id.clone());
        input
            .entity_names
            .entry(mention.entity_key.clone())
            .or_insert_with(|| mention.entity_name.clone());
    }
    for entities in input.chunk_entities.values_mut() {
        entities.sort();
        entities.dedup();
    }

    // Chunk text, from the only chunk store. Reading it costs nothing; it is
    // what makes the prompt (and therefore the estimate) real.
    let documents: BTreeSet<String> = mentions
        .iter()
        .map(|mention| mention.document_id.clone())
        .collect();
    let index = build_index()?;
    let mut missing = 0usize;
    for document in &documents {
        let document_id = DocumentId::new_unchecked(document.clone());
        match index.ordered_chunks(&tenant, &document_id).await {
            Ok(chunks) => {
                for chunk in chunks {
                    input.chunk_text.insert(chunk.unit_id.0, chunk.content);
                }
            }
            Err(error) => {
                missing += 1;
                tracing::warn!(document = %document, error = %error, "chunk text unavailable");
            }
        }
    }
    if missing > 0 && !parsed.dry_run {
        return Err(CliError::Failed(format!(
            "chunk text is missing for {missing} document(s); the prompt and its cost estimate \
             would be understated. Fix the Qdrant connection or run --dry-run to inspect counts."
        )));
    }
    if missing > 0 {
        println!(
            "warning: chunk text is missing for {missing} document(s); the token estimate \
             below is an UNDERESTIMATE"
        );
    }

    let options = DetectOptions {
        resolution: 1.0,
        max_df_fraction: parsed.max_df_fraction,
        min_chunks: parsed.min_pages,
    };
    let (plans, hubs) = plan(&input, options, parsed.chars_per_chunk);
    println!(
        "communities: {} mentions, {} chunks with entities, {} documents, {} hub entities \
         dropped (more than {:.1}% document frequency), {} communities with >= {} chunks",
        mentions.len(),
        input.chunk_entities.len(),
        documents.len(),
        hubs.len(),
        parsed.max_df_fraction * 100.0,
        plans.len(),
        parsed.min_pages,
    );
    if !hubs.is_empty() {
        println!(
            "  hub entities (first 5): {}",
            hubs.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        );
    }
    if plans.is_empty() {
        println!("communities: nothing to summarise; nothing was written.");
        return Ok(());
    }

    let mut summary_settings =
        CommunitySummarySettings::from_lookup(|key| std::env::var(key).ok(), parsed.max_tokens);
    if let Some(model) = parsed.model.as_ref() {
        summary_settings.model.clone_from(model);
    }
    let prices = resolve_prices(
        &summary_settings.model,
        parsed.price_input,
        parsed.price_output,
        std::env::var("KB_COMMUNITY_PRICE_INPUT")
            .ok()
            .and_then(|v| v.trim().parse().ok()),
        std::env::var("KB_COMMUNITY_PRICE_OUTPUT")
            .ok()
            .and_then(|v| v.trim().parse().ok()),
    );
    println!(
        "communities: model {} at {} (reasoning off, temperature 0)",
        summary_settings.model, summary_settings.base_url
    );

    let cache_dir = cache_dir_from_lookup(|key| std::env::var(key).ok());
    if cache_dir.is_none() && !parsed.dry_run {
        return Err(CliError::Failed(
            "KB_CACHE_DIR is not set: every summary must be cached so no prompt is paid for \
             twice (AGENTS rule 7). Set KB_CACHE_DIR or run --dry-run."
                .to_string(),
        ));
    }
    let cache = DiskCache::new(
        cache_dir.unwrap_or_else(|| std::env::temp_dir().join("kb-community-no-cache")),
    );

    let price_known = prices.is_some();
    let prices = match prices {
        Some(prices) => prices,
        // A dry run is the only thing an unknown price can be used for, and only
        // with `--max-usd 0`: a ceiling the operator did not set is not a ceiling.
        None if parsed.dry_run && parsed.max_usd == 0.0 => {
            println!(
                "communities: no price for {}; token counts are printed below, but the cost \
                 is unknown. Pass --price-input/--price-output (USD per 1M tokens) to price \
                 a paid run.",
                summary_settings.model
            );
            (0.0, 0.0)
        }
        None => {
            return Err(CliError::Failed(format!(
                "no price for {}; pass --price-input and --price-output in USD per 1,000,000 \
                 tokens (or use a :free model id)",
                summary_settings.model
            )));
        }
    };

    if summary_settings.api_key.trim().is_empty() && !parsed.dry_run {
        return Err(CliError::Failed(
            "no API key: set ENTITY_EXTRACTION_LLM_API_KEY (or OPENROUTER_API_KEY) for the \
             chat model; a build cannot run without one. --dry-run makes no call and needs \
             no key."
                .to_string(),
        ));
    }
    let summariser = LlmCommunitySummariser::new(summary_settings)?;
    // The estimate is computed and printed *before* the first request: the
    // operator reads the number, then the ceiling check runs, then the calls
    // start. Printing it after the batch would make the estimate useless.
    let estimate = estimate(
        &plans,
        summariser.model(),
        &cache,
        prices,
        parsed.max_tokens,
    )
    .await;
    println!(
        "communities: {} prompts, {} cached, ~{} prompt tokens (chars/4), {} completion tokens, \
         estimated ${:.4} uncached",
        estimate.communities,
        estimate.cached,
        estimate.prompt_tokens,
        estimate.completion_tokens,
        estimate.usd
    );
    println!(
        "  token counts are ESTIMATES (chars/4); prices are the configured/table values; the \
         provider invoice is the only measured number"
    );
    if !price_known {
        println!("  cost unknown: no price for this model; only --dry-run is possible");
    }
    if parsed.dry_run {
        println!("dry run: nothing written, no request made");
        return Ok(());
    }
    let run =
        summarise_after_estimate(&plans, &summariser, &cache, parsed.max_usd, estimate.usd).await?;

    if run.summaries.is_empty() {
        return Err(CliError::Failed(format!(
            "every summary failed ({} failure(s)); nothing was written. An empty answer is not \
             cached, so re-running retries it.",
            run.failures.len()
        )));
    }
    let communities: Vec<NewCommunity> = run
        .summaries
        .iter()
        .map(|(plan, summary)| NewCommunity {
            summary: summary.clone(),
            entity_keys: plan.entity_keys.clone(),
            document_ids: plan
                .chunk_ids
                .iter()
                .filter_map(|chunk| chunk_document(chunk))
                .collect::<BTreeSet<String>>()
                .into_iter()
                .map(DocumentId::new_unchecked)
                .collect(),
            chunk_ids: plan
                .chunk_ids
                .iter()
                .cloned()
                .map(kb_ports::ChunkId::new_unchecked)
                .collect(),
            model: summariser.model().to_string(),
        })
        .collect();
    let version = store.replace_communities(&tenant, communities).await?;
    println!(
        "communities: wrote {} summaries as version {} for tenant {} ({} fresh call(s), {} \
         cached, {} failure(s) not written)",
        run.summaries.len(),
        version,
        parsed.tenant,
        run.fresh,
        run.from_cache,
        run.failures.len()
    );
    Ok(())
}

/// The document id ingest embedded in a chunk id (`{documentId}_{index}`).
fn chunk_document(chunk: &str) -> Option<String> {
    kb_ports::ChunkId::new_unchecked(chunk)
        .document_part()
        .map(str::to_string)
}

/// Build the Qdrant index the build command reads chunk text from.
fn build_index() -> Result<Arc<dyn RetrievalIndex>, CliError> {
    let lookup = |key: &str| std::env::var(key).ok();
    let tier =
        crate::tier::Tier::from_lookup(&lookup).map_err(|e| CliError::Failed(e.to_string()))?;
    let lookup = crate::tier::with_preset(tier, &lookup);
    let model = lookup("KB_EMBEDDING_MODEL")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "qwen/qwen3-embedding-8b".to_string());
    let (settings, collection_override) =
        Config::qdrant_from_lookup(&lookup, &model).map_err(|e| CliError::Failed(e.to_string()))?;
    let collection = collection_override.unwrap_or_else(|| settings.collection_name());
    let index = kb_store_qdrant::QdrantIndex::new_with_collection(settings, collection)?;
    Ok(Arc::new(index))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kb_ports::PortError;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A summariser that records every prompt and can be told what to answer.
    struct FakeSummariser {
        model: String,
        calls: AtomicUsize,
        prompts: Mutex<Vec<String>>,
        answer: Mutex<Option<String>>,
        fail: Mutex<bool>,
    }

    impl FakeSummariser {
        fn new(answer: Option<&str>) -> Self {
            Self {
                model: "test/model".to_string(),
                calls: AtomicUsize::new(0),
                prompts: Mutex::new(Vec::new()),
                answer: Mutex::new(answer.map(str::to_string)),
                fail: Mutex::new(false),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl CommunitySummariser for FakeSummariser {
        fn model(&self) -> &str {
            &self.model
        }

        async fn summarise(&self, prompt: &str) -> Result<Option<String>, PortError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.prompts.lock().unwrap().push(prompt.to_string());
            if *self.fail.lock().unwrap() {
                return Err(PortError::Unavailable("provider down".into()));
            }
            Ok(self.answer.lock().unwrap().clone())
        }
    }

    fn plans() -> Vec<PlannedCommunity> {
        vec![
            PlannedCommunity {
                entity_keys: vec!["alpha".into()],
                chunk_ids: vec!["doc-a_0".into()],
                prompt: "summarise alpha".repeat(10),
            },
            PlannedCommunity {
                entity_keys: vec!["beta".into()],
                chunk_ids: vec!["doc-a_1".into()],
                prompt: "summarise beta".repeat(10),
            },
        ]
    }

    fn cache(name: &str) -> DiskCache {
        let dir =
            std::env::temp_dir().join(format!("kb-community-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        DiskCache::new(dir)
    }

    #[tokio::test]
    async fn a_dry_run_makes_no_call_and_writes_nothing() {
        let cache = cache("dry-run");
        let summariser = FakeSummariser::new(Some("a summary"));
        let (estimate, run) =
            summarise(&plans(), &summariser, &cache, (1.0, 2.0), 320, 100.0, true)
                .await
                .expect("dry run");
        assert_eq!(summariser.calls(), 0, "a dry run must not call the model");
        assert_eq!(run.summaries.len(), 0);
        assert!(estimate.usd > 0.0, "it still estimates the cost");
        let prompt = plans()[0].prompt.as_bytes().to_vec();
        assert!(
            cache
                .get(
                    CACHE_NAMESPACE,
                    &CacheKey::from_parts(&[b"test/model", prompt.as_slice()])
                )
                .await
                .is_none(),
            "a dry run writes no cache entry"
        );
    }

    #[tokio::test]
    async fn the_budget_refusal_happens_before_the_first_call() {
        let cache = cache("budget");
        let summariser = FakeSummariser::new(Some("a summary"));
        let error = summarise(
            &plans(),
            &summariser,
            &cache,
            (100.0, 100.0),
            320,
            0.000001,
            false,
        )
        .await
        .expect_err("over the ceiling");
        assert!(error.to_string().contains("exceeds --max-usd"));
        assert_eq!(summariser.calls(), 0);
    }

    #[tokio::test]
    async fn a_fresh_summary_is_cached_and_the_second_run_costs_nothing() {
        let cache = cache("cache-hit");
        let summariser = FakeSummariser::new(Some("a summary"));
        let (first_estimate, first) =
            summarise(&plans(), &summariser, &cache, (1.0, 2.0), 320, 100.0, false)
                .await
                .expect("first run");
        assert_eq!(first.fresh, 2);
        assert_eq!(first.from_cache, 0);
        assert_eq!(summariser.calls(), 2);

        let (second_estimate, second) =
            summarise(&plans(), &summariser, &cache, (1.0, 2.0), 320, 0.0, false)
                .await
                .expect("second run");
        assert_eq!(second.from_cache, 2);
        assert_eq!(second.fresh, 0);
        assert_eq!(summariser.calls(), 2, "cached prompts make no call");
        assert_eq!(second_estimate.usd, 0.0);
        assert!(first_estimate.usd > 0.0);
    }

    #[tokio::test]
    async fn an_empty_answer_is_a_failure_and_is_never_cached() {
        let cache = cache("empty");
        let summariser = FakeSummariser::new(None);
        let (_, run) = summarise(&plans(), &summariser, &cache, (1.0, 2.0), 320, 100.0, false)
            .await
            .expect("run");
        assert_eq!(run.summaries.len(), 0);
        assert_eq!(run.failures.len(), 2);
        // A second run retries rather than serving a cached hole.
        let (_, again) = summarise(&plans(), &summariser, &cache, (1.0, 2.0), 320, 100.0, false)
            .await
            .expect("second run");
        assert_eq!(again.failures.len(), 2);
        assert_eq!(summariser.calls(), 4);
    }

    #[test]
    fn prices_come_from_flags_then_free_then_env_then_the_table() {
        assert_eq!(
            resolve_prices("test/model", Some(1.0), Some(2.0), None, None),
            Some((1.0, 2.0))
        );
        assert_eq!(
            resolve_prices("test/model:free", None, None, None, None),
            Some((0.0, 0.0))
        );
        assert_eq!(
            resolve_prices("test/model", None, None, Some(3.0), Some(4.0)),
            Some((3.0, 4.0))
        );
        assert_eq!(
            resolve_prices("deepseek/deepseek-v4-flash", None, None, None, None),
            Some((0.091, 0.182))
        );
        assert_eq!(
            resolve_prices("unknown/model", None, None, None, None),
            None
        );
    }

    #[test]
    fn planning_uses_the_hub_cap_and_builds_a_deterministic_prompt() {
        let mut input = BuildInput::default();
        for (chunk, entities) in [
            ("doc-a_0", vec!["alpha", "hub"]),
            ("doc-a_1", vec!["alpha", "beta", "hub"]),
            ("doc-a_2", vec!["beta", "hub"]),
        ] {
            input.chunk_entities.insert(
                chunk.to_string(),
                entities.iter().map(|e| (*e).to_string()).collect(),
            );
            input
                .chunk_text
                .insert(chunk.to_string(), format!("text of {chunk}"));
        }
        for entity in ["alpha", "beta", "hub"] {
            input
                .entity_names
                .insert(entity.to_string(), entity.to_string());
        }
        for (index, entity) in ["alpha", "alpha", "beta", "beta", "hub", "hub", "hub", "hub"]
            .iter()
            .enumerate()
        {
            input
                .entity_documents
                .entry((*entity).to_string())
                .or_default()
                .insert(format!("doc-{index}"));
        }
        let (plans, hubs) = plan(
            &input,
            DetectOptions {
                min_chunks: 1,
                ..DetectOptions::default()
            },
            40,
        );
        assert_eq!(hubs, vec!["hub".to_string()]);
        assert!(!plans.is_empty());
        for plan in &plans {
            assert!(plan.prompt.contains("Chunk excerpts:"));
            assert!(!plan.prompt.contains("hub"));
        }
        let (again, _) = plan(
            &input,
            DetectOptions {
                min_chunks: 1,
                ..DetectOptions::default()
            },
            40,
        );
        assert_eq!(plans, again, "the plan must be deterministic");
    }

    #[test]
    fn arguments_require_a_tenant_and_reject_unknown_flags() {
        assert!(BuildArgs::parse(&[]).is_err());
        assert!(BuildArgs::parse(&["--dry-run".to_string()]).is_err());
        assert!(BuildArgs::parse(&["--tenant".to_string()]).is_err());
        assert!(
            BuildArgs::parse(&[
                "--tenant".to_string(),
                "t1".to_string(),
                "--wat".to_string()
            ])
            .is_err()
        );
        let parsed = BuildArgs::parse(&[
            "--tenant".to_string(),
            "t1".to_string(),
            "--min-pages".to_string(),
            "3".to_string(),
            "--max-usd".to_string(),
            "0.5".to_string(),
            "--dry-run".to_string(),
        ])
        .expect("parse");
        assert_eq!(parsed.tenant, "t1");
        assert_eq!(parsed.min_pages, 3);
        assert!((parsed.max_usd - 0.5).abs() < f64::EPSILON);
        assert!(parsed.dry_run);
    }

    #[test]
    fn a_chunk_id_yields_its_document_for_the_community_members() {
        assert_eq!(chunk_document("doc-a_7"), Some("doc-a".to_string()));
        assert_eq!(chunk_document("not-a-chunk"), None);
    }
}
