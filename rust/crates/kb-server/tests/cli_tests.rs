//! The operational commands, checked without a database.
//!
//! What can be wrong before a socket is opened is checked here: which
//! migrations exist, that their checksum is the one Prisma computes, and that
//! the CLI argument shape still matches `scripts/create-api-key.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use kb_server::config::Config;

fn repo_root() -> PathBuf {
    kb_server::boundary::workspace_root()
        .parent()
        .expect("rust/ has a parent")
        .to_path_buf()
}

#[test]
fn the_real_migration_folder_is_discovered_in_order() {
    let dir = repo_root().join("prisma/migrations");
    let found = kb_store_pg::migrate::discover(&dir).expect("discover migrations");
    let names: Vec<&str> = found.iter().map(|m| m.name.as_str()).collect();
    assert!(
        names.contains(&"20260819110721_init"),
        "the init migration must be found, got {names:?}"
    );
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(
        names, sorted,
        "migrations must be applied in directory order"
    );
}

#[test]
fn a_migration_checksum_is_the_sha256_of_the_file_on_disk() {
    let dir = repo_root().join("prisma/migrations");
    let found = kb_store_pg::migrate::discover(&dir).expect("discover migrations");
    for migration in &found {
        let path = dir.join(&migration.name).join("migration.sql");
        let bytes = std::fs::read(&path).expect("read migration");
        assert_eq!(
            migration.checksum,
            kb_store_pg::migrate::checksum(&bytes),
            "checksum for {} does not match the file Prisma hashes",
            migration.name
        );
        assert_eq!(migration.checksum.len(), 64);
        assert!(migration.checksum.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(migration.sql.contains("CREATE TABLE"));
    }
}

#[test]
fn the_qdrant_collection_name_is_the_model_and_the_width() {
    let (settings, override_name) = Config::qdrant_from_lookup(
        &|k: &str| (k == "KB_EMBEDDING_DIM").then(|| "8".to_string()),
        "stub",
    );
    assert_eq!(settings.dense_dims, Some(8));
    assert_eq!(settings.url, "http://127.0.0.1:16334");
    assert_eq!(settings.collection_name(), "chunks__stub__8");
    assert_eq!(override_name, None);

    let (settings, override_name) = Config::qdrant_from_lookup(
        &|k: &str| {
            (k == "KB_EMBEDDING_DIM")
                .then(|| "1024".to_string())
                .or_else(|| (k == "QDRANT_COLLECTION").then(|| "operator_chunks".to_string()))
        },
        "Qwen/Qwen3-Embedding-0.6B",
    );
    assert_eq!(override_name.as_deref(), Some("operator_chunks"));
    assert_eq!(
        settings.collection_name(),
        "chunks__qwen-qwen3-embedding-0-6b__1024"
    );
}

#[test]
fn an_unset_embedding_dimension_is_4096() {
    assert_eq!(Config::embedding_dim(&|_| None), 4096);
    let lookup = |k: &str| (k == "KB_EMBEDDING_DIM").then(|| "  1536 ".to_string());
    assert_eq!(Config::embedding_dim(&lookup), 1536);
}

#[test]
fn the_cli_only_needs_a_database_url_for_database_commands() {
    // `create-api-key` and `migrate` must not demand an inference provider.
    let pg = Config::pg_from_lookup(|k: &str| {
        (k == "DATABASE_URL").then(|| "postgres://kb:kb@localhost:5432/kb".to_string())
    })
    .expect("pg settings from DATABASE_URL alone");
    assert_eq!(pg.database_url, "postgres://kb:kb@localhost:5432/kb");
}

#[test]
fn a_missing_database_url_names_the_variable() {
    let err = Config::pg_from_lookup(|_: &str| None).unwrap_err();
    assert!(err.to_string().contains("DATABASE_URL"));
}

fn reindex_args(list: &[&str]) -> Vec<String> {
    list.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn reindex_parses_a_tenant_and_a_dry_run() {
    let parsed = kb_server::ReindexArgs::parse(&reindex_args(&[])).expect("parse");
    assert_eq!(parsed.tenant, None);
    assert!(!parsed.dry_run);

    let parsed =
        kb_server::ReindexArgs::parse(&reindex_args(&["--tenant", "acme"])).expect("parse");
    assert_eq!(parsed.tenant.as_deref(), Some("acme"));
    assert!(!parsed.dry_run);

    let parsed = kb_server::ReindexArgs::parse(&reindex_args(&["--dry-run", "--tenant", "acme"]))
        .expect("parse");
    assert_eq!(parsed.tenant.as_deref(), Some("acme"));
    assert!(parsed.dry_run);
}

#[test]
fn reindex_rejects_a_missing_tenant_value_or_an_unknown_flag() {
    for args in [
        vec!["--tenant"],
        vec!["--tenant", "--dry-run"],
        vec!["--tenant", ""],
        vec!["acme"],
        vec!["--dry_run"],
        vec!["--tenant", "a", "--tenant", "b"],
    ] {
        let err = kb_server::ReindexArgs::parse(&reindex_args(&args)).unwrap_err();
        assert!(matches!(err, kb_server::CliError::Usage(_)), "{args:?}");
    }
}

#[test]
fn reindex_counts_one_tenant_and_skips_what_cannot_be_rebuilt() {
    use kb_ports::{DocumentId, TenantId};
    use kb_store_pg::reindex::{ReindexDocument, plan};

    let document = |id: &str, s3_key: Option<&str>, active_job: bool| ReindexDocument {
        tenant_id: TenantId::new_unchecked("acme"),
        document_id: DocumentId::new_unchecked(id),
        title: id.to_string(),
        categories: Vec::new(),
        subcategory: None,
        metadata: None,
        s3_key: s3_key.map(str::to_string),
        file_size: None,
        mime_type: None,
        active_job,
    };
    let documents = vec![
        document("d1", Some("documents/acme/d1/one.pdf"), false),
        document("d2", None, false),
        document("d3", Some("documents/acme/d3/three.pdf"), true),
    ];
    let counts = plan(&documents);
    let acme = &counts["acme"];
    assert_eq!(acme.documents, 3);
    assert_eq!(acme.queued, 1);
    assert_eq!(acme.skipped_without_original, 1);
    assert_eq!(acme.skipped_active_job, 1);

    // Exactly the queueable row names the file the worker will read.
    let queueable: Vec<&str> = documents
        .iter()
        .filter(|doc| doc.is_queueable())
        .filter_map(|doc| doc.filename().as_deref().map(|_| doc.document_id.as_str()))
        .collect();
    assert_eq!(queueable, vec!["d1"]);
}
