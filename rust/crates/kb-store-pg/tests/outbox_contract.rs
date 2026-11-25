//! Postgres contract for the scope outbox (SEC-2b).
//!
//! Gated twice, like `graph_contract.rs`: `--features integration`, so a plain
//! `cargo test --workspace` needs no database, and `KB_PG_OUTBOX_CONTRACT=1`,
//! so enabling the feature without a contract database fails loudly.
//!
//! ```sh
//! rust/scripts/stack-up.sh
//! KB_PG_OUTBOX_CONTRACT=1 cargo test -p kb-store-pg --features integration \
//!   --test outbox_contract -- --nocapture
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use kb_ports::{DocumentId, DocumentStore, KnowledgeBaseId, OutboxOp, PatchDocument, TenantId};
use kb_store_pg::{PgDocumentStore, PgSettings, claim_batch_on, insert_outbox};
use sqlx::{PgPool, Row};

const TENANT: &str = "outbox-contract-tenant";
const DOC: &str = "outbox-contract-doc";
const KB_1: &str = "outbox-kb-1";
const KB_2: &str = "outbox-kb-2";

/// The three tests share one document, so they run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn database_url() -> String {
    std::env::var("KB_PG_OUTBOX_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| {
            panic!(
                "KB_PG_OUTBOX_URL (or DATABASE_URL) must point at the contract Postgres.\n\
                 Start the stack with rust/scripts/stack-up.sh, apply the migrations, then run:\n\
                 KB_PG_OUTBOX_CONTRACT=1 cargo test -p kb-store-pg --features integration \
                 --test outbox_contract -- --nocapture"
            )
        })
}

fn require_gate() {
    if std::env::var("KB_PG_OUTBOX_CONTRACT").as_deref() != Ok("1") {
        panic!(
            "KB_PG_OUTBOX_CONTRACT=1 is required to run the Postgres outbox contract.\n\
             Start the stack with rust/scripts/stack-up.sh, apply the migrations, then run:\n\
             KB_PG_OUTBOX_CONTRACT=1 cargo test -p kb-store-pg --features integration \
             --test outbox_contract -- --nocapture"
        );
    }
}

async fn pool() -> PgPool {
    require_gate();
    let settings = PgSettings {
        database_url: database_url(),
        ..PgSettings::default()
    };
    let pool = kb_store_pg::connect_pool(&settings).await.unwrap();
    // A fresh database must be migratable from zero, exactly as
    // `kb-server migrate` does it. The tests run in parallel, so migrate once.
    static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    MIGRATED
        .get_or_init(|| async {
            let migrations =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../prisma/migrations");
            kb_store_pg::migrate::apply(&pool, &migrations)
                .await
                .unwrap();
        })
        .await;
    pool
}

async fn pending(pool: &PgPool, tenant: &str) -> i64 {
    sqlx::query(r#"SELECT count(*)::bigint AS n FROM "ScopeOutbox" WHERE "tenantId" = $1"#)
        .bind(tenant)
        .fetch_one(pool)
        .await
        .unwrap()
        .try_get::<i64, _>("n")
        .unwrap()
}

async fn reset(pool: &PgPool) {
    sqlx::query(r#"DELETE FROM "ScopeOutbox" WHERE "tenantId" = $1"#)
        .bind(TENANT)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "DocumentGroup" WHERE "documentId" = $1"#)
        .bind(DOC)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "Document" WHERE id = $1"#)
        .bind(DOC)
        .execute(pool)
        .await
        .unwrap();
}

async fn seed_document(pool: &PgPool) {
    for kb in [KB_1, KB_2] {
        sqlx::query(
            r#"INSERT INTO "KnowledgeBase" (id, "tenantId", name, "updatedAt")
               VALUES ($1, $2, $1, now()) ON CONFLICT (id) DO NOTHING"#,
        )
        .bind(kb)
        .bind(TENANT)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"INSERT INTO "Document" (id, "tenantId", title, content, "updatedAt")
           VALUES ($1, $2, 'outbox doc', '', now())"#,
    )
    .bind(DOC)
    .bind(TENANT)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "DocumentGroup" (id, "documentId", "knowledgeBaseId", "createdAt")
           VALUES ($1, $2, $3, now())"#,
    )
    .bind("outbox-contract-group")
    .bind(DOC)
    .bind(KB_1)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn a_move_writes_exactly_one_outbox_row_in_the_same_transaction() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    reset(&pool).await;
    seed_document(&pool).await;
    assert_eq!(pending(&pool, TENANT).await, 0);

    let store = PgDocumentStore::from_pool(pool.clone());
    let patch = PatchDocument {
        knowledge_base_ids: Some(vec![KnowledgeBaseId(KB_2.to_string())]),
        ..PatchDocument::default()
    };
    store
        .patch_document(
            &TenantId(TENANT.to_string()),
            &DocumentId(DOC.to_string()),
            &patch,
        )
        .await
        .unwrap();

    assert_eq!(pending(&pool, TENANT).await, 1, "one PATCH, one repair row");
    let row = sqlx::query(
        r#"SELECT "documentId", "tenantId", op FROM "ScopeOutbox" WHERE "tenantId" = $1"#,
    )
    .bind(TENANT)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.try_get::<String, _>("documentId").unwrap(), DOC);
    assert_eq!(
        row.try_get::<String, _>("op").unwrap(),
        OutboxOp::KbChanged.as_str()
    );

    let groups: Vec<String> =
        sqlx::query(r#"SELECT "knowledgeBaseId" FROM "DocumentGroup" WHERE "documentId" = $1"#)
            .bind(DOC)
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String, _>("knowledgeBaseId").unwrap())
            .collect();
    assert_eq!(groups, vec![KB_2.to_string()]);
    reset(&pool).await;
}

#[tokio::test]
async fn a_rolled_back_change_writes_no_outbox_row() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    reset(&pool).await;
    seed_document(&pool).await;

    let mut tx = pool.begin().await.unwrap();
    insert_outbox(
        &mut tx,
        &TenantId(TENANT.to_string()),
        &DocumentId(DOC.to_string()),
        OutboxOp::Deleted,
    )
    .await
    .unwrap();
    let inside: i64 =
        sqlx::query(r#"SELECT count(*)::bigint AS n FROM "ScopeOutbox" WHERE "tenantId" = $1"#)
            .bind(TENANT)
            .fetch_one(&mut *tx)
            .await
            .unwrap()
            .try_get("n")
            .unwrap();
    assert_eq!(inside, 1, "the row is visible inside the transaction");
    tx.rollback().await.unwrap();

    assert_eq!(
        pending(&pool, TENANT).await,
        0,
        "rollback must remove the row"
    );
    reset(&pool).await;
}

#[tokio::test]
async fn skip_locked_does_not_hand_one_row_to_two_claimers() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    reset(&pool).await;
    seed_document(&pool).await;
    let mut setup = pool.begin().await.unwrap();
    insert_outbox(
        &mut setup,
        &TenantId(TENANT.to_string()),
        &DocumentId(DOC.to_string()),
        OutboxOp::KbChanged,
    )
    .await
    .unwrap();
    setup.commit().await.unwrap();

    let mut first = pool.begin().await.unwrap();
    let claimed = claim_batch_on(&mut first, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].document.as_str(), DOC);
    assert_eq!(claimed[0].attempts, 1);

    let mut second = pool.begin().await.unwrap();
    let blocked = claim_batch_on(&mut second, 10).await.unwrap();
    assert!(blocked.is_empty(), "the row is locked by the first claimer");
    first.commit().await.unwrap();
    second.commit().await.unwrap();

    // The claim increments `attempts`, and the linear backoff delays the next
    // claim by 2 seconds per attempt. After that the same row is retryable.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let mut third = pool.begin().await.unwrap();
    let retry = claim_batch_on(&mut third, 10).await.unwrap();
    assert_eq!(retry.len(), 1, "a released row must be claimable again");
    assert_eq!(retry[0].attempts, 2);
    third.commit().await.unwrap();
    reset(&pool).await;
}
