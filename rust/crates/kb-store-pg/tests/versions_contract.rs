//! Postgres contract for RET-16 version scope and superseding, in the same
//! style as `graph_contract.rs`. Gated twice so `cargo test --workspace` needs
//! no database:
//!
//! ```sh
//! rust/scripts/stack-up.sh
//! KB_PG_VERSIONS_CONTRACT=1 cargo test -p kb-store-pg --features integration \
//!   --test versions_contract -- --nocapture
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use kb_ports::{DocumentFilter, DocumentId, DocumentStore, NewDocument, ScopeRequest, TenantId};
use kb_store_pg::{PgDocumentStore, PgSettings, connect_pool, migrate};

const TENANT: &str = "versions-contract-tenant";

fn require_gate() {
    if std::env::var("KB_PG_VERSIONS_CONTRACT").as_deref() != Ok("1") {
        panic!("KB_PG_VERSIONS_CONTRACT=1 is required to run the Postgres version contract");
    }
}

async fn fresh_store() -> PgDocumentStore {
    let url = std::env::var("KB_PG_VERSIONS_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| panic!("KB_PG_VERSIONS_URL (or DATABASE_URL) is required"));
    let settings = PgSettings {
        database_url: url,
        ..PgSettings::default()
    };
    let pool = connect_pool(&settings).await.unwrap();
    // `kb-server migrate` is the production path; run the same files here so
    // this contract also proves a fresh database can be migrated from zero.
    let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../prisma/migrations");
    migrate::apply(&pool, &migrations).await.unwrap();
    // `DocumentGroup` has no tenant column; its rows hang off the tenant's
    // documents, so delete them through `documentId` first.
    sqlx::query(
        r#"DELETE FROM "DocumentGroup" WHERE "documentId" IN
           (SELECT id FROM "Document" WHERE "tenantId" = $1)"#,
    )
    .bind(TENANT)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"DELETE FROM "Document" WHERE "tenantId" = $1"#)
        .bind(TENANT)
        .execute(&pool)
        .await
        .unwrap();
    PgDocumentStore::from_pool(pool)
}

fn doc(id: &str, doc_key: Option<&str>, version: Option<&str>, from: Option<&str>) -> NewDocument {
    NewDocument {
        id: DocumentId(id.to_string()),
        tenant_id: TenantId(TENANT.to_string()),
        title: "Kebijakan Cuti".to_string(),
        content: String::new(),
        categories: vec![],
        subcategory: None,
        s3_key: None,
        file_size: None,
        mime_type: None,
        status: "ready".to_string(),
        external_ref: None,
        metadata: None,
        sha256: None,
        doc_key: doc_key.map(str::to_string),
        version: version.map(str::to_string),
        effective_from: from.map(str::to_string),
        effective_to: None,
        acl_principals: Vec::new(),
    }
}

async fn scope_ids(store: &PgDocumentStore, filter: DocumentFilter) -> Vec<String> {
    let mut ids: Vec<String> = store
        .build_scope(&ScopeRequest {
            tenant: TenantId(TENANT.to_string()),
            filter,
            force_ids: false,
        })
        .await
        .unwrap()
        .document_ids()
        .iter()
        .map(|id| id.0.clone())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn postgres_supersedes_and_scopes_versions() {
    require_gate();
    let store = fresh_store().await;
    store
        .create_document(
            &doc(
                "v1",
                Some("cuti"),
                Some("2024"),
                Some("2024-01-01T00:00:00Z"),
            ),
            &[],
        )
        .await
        .unwrap();
    store
        .create_document(
            &doc(
                "v2",
                Some("cuti"),
                Some("2025"),
                Some("2025-01-01T00:00:00Z"),
            ),
            &[],
        )
        .await
        .unwrap();
    store
        .create_document(&doc("plain", None, None, None), &[])
        .await
        .unwrap();

    let old = store
        .find_meta_by_id(
            &TenantId(TENANT.to_string()),
            &DocumentId("v1".into()),
            false,
        )
        .await
        .unwrap()
        .unwrap();
    let new = store
        .find_meta_by_id(
            &TenantId(TENANT.to_string()),
            &DocumentId("v2".into()),
            false,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!old.is_latest, "v1 must be retired");
    // Postgres renders timestamps with milliseconds, like every other
    // timestamp the API returns.
    assert_eq!(
        old.effective_to.as_deref(),
        Some("2025-01-01T00:00:00.000Z")
    );
    assert_eq!(new.supersedes_document_id.as_deref(), Some("v1"));

    assert_eq!(
        scope_ids(&store, DocumentFilter::default()).await,
        ["plain", "v2"]
    );
    assert_eq!(
        scope_ids(
            &store,
            DocumentFilter {
                as_of: Some("2024-06-01T00:00:00Z".into()),
                ..DocumentFilter::default()
            },
        )
        .await,
        ["plain", "v1"]
    );
    assert_eq!(
        scope_ids(
            &store,
            DocumentFilter {
                include_history: true,
                ..DocumentFilter::default()
            },
        )
        .await,
        ["plain", "v1", "v2"]
    );

    // SEC-2a × RET-16: the predicate-mode verify runs the same SQL with an id
    // restriction. With `asOf` it must number its parameters correctly and
    // keep only the version in force — proven here against real Postgres.
    let candidates = [
        DocumentId("v1".into()),
        DocumentId("v2".into()),
        DocumentId("plain".into()),
    ];
    let verified = |filter: DocumentFilter| {
        let store = &store;
        let candidates = &candidates;
        async move {
            let mut ids: Vec<String> = store
                .verify_visible(
                    &ScopeRequest {
                        tenant: TenantId(TENANT.to_string()),
                        filter,
                        force_ids: false,
                    },
                    candidates,
                )
                .await
                .unwrap()
                .into_iter()
                .map(|id| id.0)
                .collect();
            ids.sort();
            ids
        }
    };
    assert_eq!(
        verified(DocumentFilter {
            as_of: Some("2024-06-01T00:00:00Z".into()),
            ..DocumentFilter::default()
        })
        .await,
        ["plain", "v1"]
    );
    assert_eq!(verified(DocumentFilter::default()).await, ["plain", "v2"]);
}
