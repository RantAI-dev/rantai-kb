//! `prisma migrate deploy` for the relational schema, without Prisma.
//!
//! The Bun service migrates with `bunx prisma migrate deploy`. During cutover
//! both binaries may run against the same Postgres, so this module speaks
//! Prisma's own bookkeeping rather than inventing a second one: every applied
//! `prisma/migrations/*/migration.sql` is recorded in `_prisma_migrations` with
//! the same columns, and the same SHA-256 hex checksum Prisma computes over
//! the file's bytes. A migration already present with that name is skipped; a
//! migration present under that name with a *different* checksum is the one
//! error Prisma also refuses (`migrate deploy` aborts on a modified migration),
//! so it is refused here too.
//!
//! Difference on failure: Prisma leaves the started row behind with a null
//! `finished_at` and the error in `logs`, and every later `migrate deploy`
//! then refuses to proceed until an operator intervenes. This runner applies
//! each migration inside one transaction, so a failure rolls the row back with
//! the statements and the next run simply tries again. That matters for the
//! container entrypoint, where a wedged migration table would turn one bad
//! deploy into a permanently unbootable service.
//!
//! `applied_steps_count` is written as 1 — one file, one applied step. Prisma
//! counts statements within a script there; nothing in `migrate deploy` or
//! `migrate status` reads the column, so the value is bookkeeping only.

use std::path::{Path, PathBuf};

use kb_ports::PortError;
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgPool};

use crate::queries::{
    CREATE_MIGRATIONS_TABLE, FINISH_MIGRATION, INSERT_MIGRATION, SELECT_APPLIED_MIGRATION,
};

/// Advisory lock id so two replicas booting at once cannot both apply the same
/// migration. Arbitrary and constant; only this program takes it.
const MIGRATE_LOCK_ID: i64 = 7_241_331_885;

/// One migration file, read and hashed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migration {
    /// Directory name, which is also `migration_name` in the table.
    pub name: String,
    /// Raw file bytes: what the checksum is taken over.
    pub sql: String,
    /// SHA-256 hex of the file, as Prisma stores it.
    pub checksum: String,
}

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Names applied by this run, in order.
    pub applied: Vec<String>,
    /// Names already recorded by an earlier run (Prisma or this binary).
    pub skipped: Vec<String>,
}

/// Read `prisma/migrations/*/migration.sql` in directory order.
///
/// Prisma orders migrations lexicographically by directory name, which is why
/// the names carry a timestamp prefix.
///
/// # Errors
///
/// Fails when the directory cannot be read, a migration folder has no
/// `migration.sql`, or one file cannot be read.
pub fn discover(dir: &Path) -> Result<Vec<Migration>, PortError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| PortError::Invalid(format!("cannot read {}: {e}", dir.display())))?;

    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| PortError::Invalid(format!("cannot read dir entry: {e}")))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| PortError::Invalid(format!("non-utf8 migration folder: {path:?}")))?
            .to_string();
        if name == "migration_lock.toml" || name.starts_with('.') {
            continue;
        }
        let sql_path = path.join("migration.sql");
        if !sql_path.is_file() {
            return Err(PortError::Invalid(format!("{name} has no migration.sql")));
        }
        files.push(sql_path);
    }
    files.sort();

    files
        .iter()
        .map(|path| {
            let sql = std::fs::read_to_string(path)
                .map_err(|e| PortError::Invalid(format!("cannot read {}: {e}", path.display())))?;
            let name = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .ok_or_else(|| PortError::Invalid(format!("no migration name: {path:?}")))?
                .to_string();
            let checksum = checksum(sql.as_bytes());
            Ok(Migration {
                name,
                sql,
                checksum,
            })
        })
        .collect()
}

/// SHA-256 of the migration file's bytes, hex encoded — Prisma's checksum.
#[must_use]
pub fn checksum(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Create `_prisma_migrations` if it is absent, then apply what is missing.
///
/// # Errors
///
/// Fails on a database error, on a migration whose checksum no longer matches
/// the recorded one, and on a migration left unfinished by a crashed run.
pub async fn apply(pool: &PgPool, dir: &Path) -> Result<MigrationReport, PortError> {
    let migrations = discover(dir)?;

    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| PortError::Unavailable(format!("pg acquire: {e}")))?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATE_LOCK_ID)
        .execute(&mut *conn)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg advisory lock: {e}")))?;

    let result = apply_locked(&mut conn, &migrations).await;

    // The lock is session-scoped; releasing it explicitly keeps a pooled
    // connection from holding it for the next borrower.
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATE_LOCK_ID)
        .execute(&mut *conn)
        .await;
    result
}

async fn apply_locked(
    conn: &mut sqlx::PgConnection,
    migrations: &[Migration],
) -> Result<MigrationReport, PortError> {
    sqlx::raw_sql(CREATE_MIGRATIONS_TABLE)
        .execute(&mut *conn)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg create _prisma_migrations: {e}")))?;

    let mut report = MigrationReport::default();
    for migration in migrations {
        if applied_checksum(&mut *conn, &migration.name)
            .await?
            .is_some_and(|recorded| recorded == migration.checksum)
        {
            report.skipped.push(migration.name.clone());
            continue;
        }
        if let Some(recorded) = applied_checksum(&mut *conn, &migration.name).await? {
            return Err(PortError::Invalid(format!(
                "migration {} was modified after it was applied (recorded checksum {recorded}, file {})",
                migration.name, migration.checksum
            )));
        }
        apply_one(&mut *conn, migration).await?;
        report.applied.push(migration.name.clone());
    }
    Ok(report)
}

/// The checksum Prisma (or an earlier run) recorded for `name`.
///
/// A row with a null `finished_at` or a `rolled_back_at` is not "applied", so
/// it reads as absent — those states mean the migration was interrupted.
async fn applied_checksum(
    conn: &mut sqlx::PgConnection,
    name: &str,
) -> Result<Option<String>, PortError> {
    // A single `Option<String>`, with the applied/unapplied decision made by the
    // WHERE clause rather than by decoding timestamps in Rust.
    //
    // That is the whole fix, and it took two attempts to find. Prisma creates
    // `_prisma_migrations` with `timestamp with time zone`; this crate's own
    // CREATE TABLE made them `TIMESTAMP(3)`, without. Decoding those columns in
    // Rust therefore cannot work for both: `DateTime<Utc>` fails on a timestamp
    // without a zone, `NaiveDateTime` fails on one with, and both were swallowed by
    // `.ok()` into "not applied". Against a Prisma-migrated database that meant
    // every run re-applied every migration and died on `relation "Document" already
    // exists` — which is the mixed fleet's exact failure mode, invisible on a fresh
    // container because there is nothing to skip yet.
    //
    // Whether a column is null is not a type question, so it is asked in SQL. What
    // comes back is one `TEXT` column whose type both engines agree on.
    let checksum: Option<String> = sqlx::query_scalar(SELECT_APPLIED_MIGRATION)
        .bind(name)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg read _prisma_migrations: {e}")))?;
    // `checksum` is NOT NULL in both this crate's DDL and Prisma's, so the
    // `Option` here can only be "no such applied row".
    Ok(checksum)
}

/// Apply one migration and record it, atomically.
async fn apply_one(conn: &mut sqlx::PgConnection, migration: &Migration) -> Result<(), PortError> {
    let mut tx = conn
        .begin()
        .await
        .map_err(|e| PortError::Unavailable(format!("pg begin: {e}")))?;

    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(INSERT_MIGRATION)
        .bind(&id)
        .bind(&migration.checksum)
        .bind(&migration.name)
        .execute(&mut *tx)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg insert migration row: {e}")))?;

    // The simple protocol: one file may hold many statements, and `raw_sql`
    // sends them as a script so Postgres aborts the whole transaction on the
    // first error.
    sqlx::raw_sql(&migration.sql)
        .execute(&mut *tx)
        .await
        .map_err(|e| PortError::Invalid(format!("migration {} failed: {e}", migration.name)))?;

    sqlx::query(FINISH_MIGRATION)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg finish migration row: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| PortError::Unavailable(format!("pg commit: {e}")))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn checksum_is_sha256_hex_of_the_file_bytes() {
        // Same vector the API-key test uses: sha256("abc").
        assert_eq!(
            checksum(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Trailing bytes change the checksum, which is what makes a modified
        // migration detectable.
        assert_ne!(checksum(b"abc"), checksum(b"abc\n"));
    }

    #[test]
    fn discover_reads_migrations_in_directory_order() {
        let dir = std::env::temp_dir().join(format!("kb-migrate-{}", std::process::id()));
        for (name, sql) in [
            ("20260101000000_second", "SELECT 2;"),
            ("20250101000000_first", "SELECT 1;"),
        ] {
            let folder = dir.join(name);
            std::fs::create_dir_all(&folder).expect("mkdir");
            std::fs::write(folder.join("migration.sql"), sql).expect("write");
        }
        std::fs::write(dir.join("migration_lock.toml"), "provider").expect("write");

        let found = discover(&dir).expect("discover");
        let names: Vec<&str> = found.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["20250101000000_first", "20260101000000_second"]);
        assert_eq!(found[1].checksum, checksum(b"SELECT 2;"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_without_migration_sql_is_an_error() {
        let dir = std::env::temp_dir().join(format!("kb-migrate-bad-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("20260101000000_empty")).expect("mkdir");
        assert!(discover(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
