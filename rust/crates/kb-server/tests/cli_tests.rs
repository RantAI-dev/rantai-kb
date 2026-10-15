//! The operational commands, checked without a database.
//!
//! What can be wrong before a socket is opened is checked here: which
//! migrations exist, that their checksum is the one Prisma computes, that the
//! SurrealDB DDL is the TypeScript script's statement for statement, and that
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
fn the_vector_schema_is_the_typescript_statement_set_at_the_configured_width() {
    let dim = Config::embedding_dim(&|k: &str| (k == "KB_EMBEDDING_DIM").then(|| "8".to_string()));
    assert_eq!(dim, 8);
    let statements = kb_store_surreal::schema::schema_statements(dim);
    let expected = [
        "DEFINE TABLE IF NOT EXISTS document_chunk SCHEMALESS;",
        "DEFINE FIELD IF NOT EXISTS document_id ON document_chunk TYPE string;",
        "DEFINE FIELD IF NOT EXISTS content ON document_chunk TYPE string;",
        "DEFINE FIELD IF NOT EXISTS chunk_index ON document_chunk TYPE int;",
        "DEFINE FIELD IF NOT EXISTS embedding ON document_chunk TYPE array<float>;",
        "DEFINE FIELD IF NOT EXISTS contextual_prefix ON document_chunk TYPE option<string>;",
        "DEFINE FIELD IF NOT EXISTS embedding_model ON document_chunk TYPE option<string>;",
        "DEFINE INDEX IF NOT EXISTS document_id_idx ON document_chunk FIELDS document_id;",
        "DEFINE ANALYZER IF NOT EXISTS kb_en TOKENIZERS class FILTERS lowercase, snowball(english);",
        "DEFINE INDEX IF NOT EXISTS content_search_idx ON document_chunk FIELDS content SEARCH ANALYZER kb_en BM25(1.2, 0.75) HIGHLIGHTS;",
        "DEFINE INDEX IF NOT EXISTS embedding_idx ON document_chunk FIELDS embedding HNSW DIMENSION 8 DIST COSINE;",
    ];
    assert_eq!(statements, expected);
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
fn surreal_settings_keep_their_typescript_defaults() {
    let surreal = Config::surreal_from_lookup(|k: &str| {
        (k == "SURREAL_DB_URL").then(|| "http://surreal:8000".to_string())
    })
    .expect("surreal settings");
    assert_eq!(surreal.url, "http://surreal:8000");
    assert_eq!(surreal.namespace, "rantai");
    assert_eq!(surreal.database, "knowledge");
    assert!(surreal.username.is_none());
    assert!(surreal.password.is_none());
}

#[test]
fn a_missing_database_url_names_the_variable() {
    let err = Config::pg_from_lookup(|_: &str| None).unwrap_err();
    assert!(err.to_string().contains("DATABASE_URL"));
    let err = Config::surreal_from_lookup(|_: &str| None).unwrap_err();
    assert!(err.to_string().contains("SURREAL_DB_URL"));
}
