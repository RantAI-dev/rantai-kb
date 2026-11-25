//! `kb-server scope backfill` — give old chunks their KB payload (SEC-2b).
//!
//! Predicate mode needs `knowledge_base_id` on every point, but chunks ingested
//! before SEC-2a carry none. This command walks the live documents in Postgres
//! and overwrites that one payload key from the authoritative group links. It
//! needs no re-embed: vectors, BM25, text and page images are untouched.
//!
//! Idempotent by construction: setting the same list twice is the same state,
//! and a document no longer in any KB gets an empty list (which matches no
//! predicate). Soft-deleted documents are not visited — their points are the
//! outbox's `deleted` rows' job.

use std::sync::Arc;

use kb_ports::{DocumentStore, KnowledgeBaseId, RetrievalIndex, TenantId};
use kb_store_pg::PgDocumentStore;
use kb_store_qdrant::QdrantIndex;

use crate::cli::CliError;
use crate::config::Config;

/// Documents fetched per `list_page` call.
const PAGE_SIZE: u32 = 200;

/// Parsed `scope backfill [--tenant T] [--dry-run]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillArgs {
    pub tenant: Option<String>,
    pub dry_run: bool,
}

impl BackfillArgs {
    /// Parse the subcommand's flags. A bare argument is a usage error rather
    /// than a silently ignored tenant.
    pub fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut tenant = None;
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
                        "scope backfill: unknown argument {other:?}; expected [--tenant <id>] [--dry-run]"
                    )));
                }
            }
        }
        Ok(Self { tenant, dry_run })
    }
}

/// Run `scope backfill`.
pub async fn run(args: &[String]) -> Result<(), CliError> {
    if args.first().map(String::as_str) != Some("backfill") {
        return Err(CliError::Usage(
            "scope: expected `backfill [--tenant <id>] [--dry-run]`".to_string(),
        ));
    }
    let parsed = BackfillArgs::parse(&args[1..])?;

    let lookup = |key: &str| std::env::var(key).ok();
    let pg = Config::pg_from_lookup(|key| std::env::var(key).ok())
        .map_err(|e| CliError::Failed(e.to_string()))?;
    let pool = kb_store_pg::connect_pool(&pg).await?;
    let documents: Arc<dyn DocumentStore> = Arc::new(PgDocumentStore::from_pool(pool.clone()));
    let tenants = match parsed.tenant.as_deref() {
        Some(tenant) => vec![tenant.to_string()],
        None => kb_store_pg::scope::list_tenants(&pool).await?,
    };

    // Qdrant settings resolve through the same KB_TIER overlay `apply-schema`
    // uses, so the collection this command writes is the one the server reads.
    let tier =
        crate::tier::Tier::from_lookup(&lookup).map_err(|e| CliError::Failed(e.to_string()))?;
    let overlaid = crate::tier::with_preset(tier, &lookup);
    let model = overlaid("KB_EMBEDDING_MODEL")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "qwen/qwen3-embedding-8b".to_string());
    let (settings, collection_override) = Config::qdrant_from_lookup(&overlaid, &model);
    let collection = collection_override.unwrap_or_else(|| settings.collection_name());
    let index: Arc<dyn RetrievalIndex> = Arc::new(
        QdrantIndex::new_with_collection(settings, collection.clone())
            .map_err(|e| CliError::Failed(e.to_string()))?,
    );

    println!(
        "scope backfill: {} tenant(s), collection {collection}{}",
        tenants.len(),
        if parsed.dry_run {
            " (dry run — no payloads will be written)"
        } else {
            ""
        }
    );

    let mut scanned = 0usize;
    let mut updated = 0usize;
    let mut failed = 0usize;
    for tenant_id in &tenants {
        let tenant = TenantId::new_unchecked(tenant_id.clone());
        let mut cursor = None;
        loop {
            let page = documents
                .list_page(&tenant, PAGE_SIZE, &[], None, cursor.as_ref())
                .await?;
            for doc in &page.documents {
                scanned += 1;
                let kb_ids: Vec<KnowledgeBaseId> = doc
                    .knowledge_base_ids
                    .iter()
                    .map(|id| KnowledgeBaseId::new_unchecked(id.clone()))
                    .collect();
                if parsed.dry_run {
                    updated += 1;
                    continue;
                }
                match index
                    .set_document_kb_payload(&tenant, &doc.id, &kb_ids)
                    .await
                {
                    Ok(()) => updated += 1,
                    Err(err) => {
                        failed += 1;
                        tracing::warn!(
                            tenant = %tenant_id,
                            document = %doc.id.as_str(),
                            error = %err.to_string(),
                            "backfill payload write failed"
                        );
                    }
                }
            }
            match page.next_cursor {
                Some(token) => {
                    cursor = kb_core::pagination::decode(&token);
                    if cursor.is_none() {
                        return Err(CliError::Failed(format!(
                            "tenant {tenant_id}: malformed pagination cursor"
                        )));
                    }
                }
                None => break,
            }
        }
        println!("  {tenant_id}: scanned so far {scanned}");
    }

    println!(
        "scope backfill: scanned {scanned}, {} {updated}, failed {failed}",
        if parsed.dry_run { "would set" } else { "set" }
    );
    if failed > 0 {
        return Err(CliError::Failed(format!(
            "{failed} payload write(s) failed; rerun the command (it is idempotent)"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_tenant_and_dry_run_parse() {
        let parsed = BackfillArgs::parse(&args(&["--tenant", "acme", "--dry-run"])).unwrap();
        assert_eq!(parsed.tenant.as_deref(), Some("acme"));
        assert!(parsed.dry_run);
    }

    #[test]
    fn a_bare_argument_is_a_usage_error() {
        let err = BackfillArgs::parse(&args(&["acme"])).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)));
    }

    #[test]
    fn the_subcommand_name_is_required() {
        let err = BackfillArgs::parse(&args(&["--dry-run"]));
        assert!(err.is_ok(), "the subcommand is consumed by dispatch");
    }
}
