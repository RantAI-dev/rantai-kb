//! Postgres contract for SEC-2d's bounded scope probe. Gated so the workspace
//! test run needs no database:
//!
//! ```sh
//! KB_PG_SCOPE_LIMIT_CONTRACT=1 KB_PG_SCOPE_LIMIT_URL=postgres://... \
//!   cargo test -p kb-store-pg --features integration --test scope_limit_contract \
//!   -- --nocapture
//! ```
//!
//! The store is migrated from zero with the same files `kb-server migrate`
//! uses. No environment variable is changed: `build_scope_with_limit` takes the
//! server-side limit as an argument, which is also how the unit tests exercise
//! the same path without racing other tests over process-wide state.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use kb_ports::{
    DocumentFilter, DocumentId, DocumentStore, KnowledgeBaseId, MetadataFilter, NewDocument,
    ScopeRequest, TenantId,
};
use kb_store_pg::{PgDocumentStore, PgSettings, connect_pool, migrate};

const TENANT: &str = "scope-limit-contract-tenant";
const KB: &str = "scope-limit-contract-kb";
/// The contract seeds `LIMIT + 5` documents, so the probe must see `LIMIT + 1`.
const LIMIT: usize = 4;
const DOCUMENTS: usize = LIMIT + 5;

fn require_gate() {
    if std::env::var("KB_PG_SCOPE_LIMIT_CONTRACT").as_deref() != Ok("1") {
        panic!("KB_PG_SCOPE_LIMIT_CONTRACT=1 is required to run the Postgres scope-limit contract");
    }
}

async fn fresh_store() -> PgDocumentStore {
    let url = std::env::var("KB_PG_SCOPE_LIMIT_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| {
            panic!("KB_PG_SCOPE_LIMIT_URL (or DATABASE_URL) is required for this contract")
        });
    let pool = connect_pool(&PgSettings {
        database_url: url,
        ..PgSettings::default()
    })
    .await
    .unwrap();
    let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../prisma/migrations");
    migrate::apply(&pool, &migrations).await.unwrap();

    sqlx::query(
        r#"DELETE FROM "DocumentGroup" WHERE "documentId" IN
           (SELECT id FROM "Document" WHERE "tenantId" = $1)"#,
    )
    .bind(TENANT)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"DELETE FROM "ScopeOutbox" WHERE "tenantId" = $1"#)
        .bind(TENANT)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "Document" WHERE "tenantId" = $1"#)
        .bind(TENANT)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "KnowledgeBase" WHERE "id" = $1"#)
        .bind(KB)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "KnowledgeBase" (id, "tenantId", name, "updatedAt")
           VALUES ($1, $2, 'Scope limit contract', now())"#,
    )
    .bind(KB)
    .bind(TENANT)
    .execute(&pool)
    .await
    .unwrap();
    PgDocumentStore::from_pool(pool)
}

fn doc(id: &str, acl: &[&str], doc_key: Option<&str>) -> NewDocument {
    NewDocument {
        id: DocumentId(id.to_string()),
        tenant_id: TenantId(TENANT.to_string()),
        title: id.to_string(),
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
        version: None,
        effective_from: None,
        effective_to: None,
        acl_principals: acl.iter().map(|p| (*p).to_string()).collect(),
    }
}

fn request(filter: DocumentFilter) -> ScopeRequest {
    ScopeRequest {
        tenant: TenantId(TENANT.to_string()),
        filter,
        force_ids: false,
    }
}

#[tokio::test]
async fn a_bounded_probe_builds_a_predicate_without_reading_every_id() {
    require_gate();
    let store = fresh_store().await;
    let kb = &[KnowledgeBaseId(KB.to_string())];
    for i in 0..DOCUMENTS {
        store
            .create_document(&doc(&format!("limit-doc-{i}"), &[], None), kb)
            .await
            .unwrap();
    }

    // 0 keeps the legacy full read, even though the tenant is probe-eligible.
    let legacy = store
        .build_scope_with_limit(&request(DocumentFilter::default()), 0)
        .await
        .unwrap();
    assert!(!legacy.is_predicate());
    assert_eq!(legacy.document_ids().len(), DOCUMENTS);

    // Exactly LIMIT visible ids: the probe returns no more than the limit, so
    // those rows are the full list and no tenant check is needed.
    let exact = store
        .build_scope_with_limit(&request(DocumentFilter::default()), DOCUMENTS)
        .await
        .unwrap();
    assert!(!exact.is_predicate());
    assert_eq!(exact.document_ids().len(), DOCUMENTS);

    // LIMIT + 1 exists: predicate mode is built from the probe alone. The id
    // list is deliberately not materialised, but the verify still works.
    let predicate = store
        .build_scope_with_limit(&request(DocumentFilter::default()), LIMIT)
        .await
        .unwrap();
    assert!(predicate.is_predicate(), "expected predicate scope");
    assert!(predicate.document_ids().is_empty());
    assert_eq!(predicate.tenant().as_str(), TENANT);
    let spec = predicate.predicate().expect("a predicate spec");
    assert_eq!(spec.verify.tenant.as_str(), TENANT);
    assert!(spec.kb_ids.is_none());
    let visible = store
        .verify_visible(
            &spec.verify.request(),
            &[
                DocumentId("limit-doc-0".into()),
                DocumentId("not-seeded".into()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert!(visible.contains(&DocumentId("limit-doc-0".into())));

    // A filter the payload cannot express still pays for the full id list.
    let filtered = DocumentFilter {
        include_history: true,
        ..DocumentFilter::default()
    };
    let ids = store
        .build_scope_with_limit(&request(filtered), LIMIT)
        .await
        .unwrap();
    assert!(!ids.is_predicate());
    assert_eq!(ids.document_ids().len(), DOCUMENTS);

    // SEC-3/RET-16 hooks: once the tenant holds an ACL'd or versioned row the
    // predicate is refused and the full list is read from Postgres again.
    store
        .create_document(&doc("limit-acl", &["user:42"], None), kb)
        .await
        .unwrap();
    let acl_request = request(DocumentFilter::default());
    let ids = store
        .build_scope_with_limit(&acl_request, LIMIT)
        .await
        .unwrap();
    assert!(!ids.is_predicate(), "an ACL'd tenant forces Ids mode");
    assert_eq!(ids.document_ids().len(), DOCUMENTS);

    let entitled = ScopeRequest {
        filter: DocumentFilter {
            principals: vec!["user:42".into()],
            ..DocumentFilter::default()
        },
        ..acl_request
    };
    let ids = store
        .build_scope_with_limit(&entitled, LIMIT)
        .await
        .unwrap();
    assert!(!ids.is_predicate());
    assert_eq!(ids.document_ids().len(), DOCUMENTS + 1);

    store
        .create_document(&doc("limit-versioned", &[], Some("doc-key-1")), kb)
        .await
        .unwrap();
    let ids = store
        .build_scope_with_limit(&request(DocumentFilter::default()), LIMIT)
        .await
        .unwrap();
    assert!(!ids.is_predicate(), "a versioned tenant forces Ids mode");

    // Metadata predicates are unindexable too: even with a matching document
    // the full list is materialised (and narrowed by the filter).
    let meta = store
        .build_scope_with_limit(
            &request(DocumentFilter {
                metadata: vec![MetadataFilter {
                    key: "year".into(),
                    values: vec!["2024".into()],
                }],
                ..DocumentFilter::default()
            }),
            LIMIT,
        )
        .await
        .unwrap();
    assert!(!meta.is_predicate());
    assert!(meta.document_ids().is_empty());
}
