//! Postgres contract for SEC-3 document access lists. Gated so the workspace
//! test run needs no database:
//!
//! ```sh
//! rust/scripts/stack-up.sh
//! KB_PG_ACL_CONTRACT=1 cargo test -p kb-store-pg --features integration \
//!   --test acl_contract -- --nocapture
//! ```
//!
//! The store is migrated from zero with the same files `kb-server migrate`
//! uses. A KnowledgeBase row is seeded because the documents are filed under
//! it, which exercises the `DocumentGroup` foreign keys as production does.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use kb_ports::{
    DocumentFilter, DocumentId, DocumentStore, KnowledgeBaseId, NewDocument, ScopeRequest, TenantId,
};
use kb_store_pg::{PgDocumentStore, PgSettings, connect_pool, migrate};

const TENANT: &str = "acl-contract-tenant";
const KB: &str = "acl-contract-kb";

fn require_gate() {
    if std::env::var("KB_PG_ACL_CONTRACT").as_deref() != Ok("1") {
        panic!("KB_PG_ACL_CONTRACT=1 is required to run the Postgres ACL contract");
    }
}

async fn fresh_store() -> PgDocumentStore {
    let url = std::env::var("KB_PG_ACL_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| panic!("KB_PG_ACL_URL (or DATABASE_URL) is required"));
    let settings = PgSettings {
        database_url: url,
        ..PgSettings::default()
    };
    let pool = connect_pool(&settings).await.unwrap();
    let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../prisma/migrations");
    migrate::apply(&pool, &migrations).await.unwrap();

    // DocumentGroup has no tenant column: delete it through the tenant's
    // documents before deleting the documents and the seeded knowledge base.
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
    sqlx::query(r#"DELETE FROM "KnowledgeBase" WHERE "id" = $1"#)
        .bind(KB)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "KnowledgeBase" (id, "tenantId", name, "updatedAt")
           VALUES ($1, $2, 'ACL contract', now())"#,
    )
    .bind(KB)
    .bind(TENANT)
    .execute(&pool)
    .await
    .unwrap();
    PgDocumentStore::from_pool(pool)
}

fn doc(id: &str, acl: &[&str]) -> NewDocument {
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
        doc_key: None,
        version: None,
        effective_from: None,
        effective_to: None,
        acl_principals: acl.iter().map(|p| (*p).to_string()).collect(),
    }
}

fn request(principals: &[&str]) -> ScopeRequest {
    ScopeRequest {
        tenant: TenantId(TENANT.to_string()),
        filter: DocumentFilter {
            principals: principals.iter().map(|p| (*p).to_string()).collect(),
            ..DocumentFilter::default()
        },
        force_ids: false,
    }
}

async fn scope_ids(store: &PgDocumentStore, req: &ScopeRequest) -> Vec<String> {
    let mut ids: Vec<String> = store
        .build_scope(req)
        .await
        .unwrap()
        .document_ids()
        .iter()
        .map(|id| id.0.clone())
        .collect();
    ids.sort();
    ids
}

async fn verified(store: &PgDocumentStore, req: &ScopeRequest) -> Vec<String> {
    let candidates = [
        DocumentId("acl-a".into()),
        DocumentId("acl-b".into()),
        DocumentId("acl-c".into()),
    ];
    let mut ids: Vec<String> = store
        .verify_visible(req, &candidates)
        .await
        .unwrap()
        .into_iter()
        .map(|id| id.0)
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn postgres_enforces_access_lists_default_deny_and_overlap() {
    require_gate();
    let store = fresh_store().await;
    let kb = &[KnowledgeBaseId(KB.to_string())];
    for doc in [
        doc("acl-a", &["group:hr"]),
        doc("acl-b", &[]),
        doc("acl-c", &["user:42"]),
    ] {
        store.create_document(&doc, kb).await.unwrap();
    }

    // No assertion: the un-ACL'd row stays visible, both ACL'd rows vanish.
    assert_eq!(scope_ids(&store, &request(&[])).await, ["acl-b"]);
    assert_eq!(verified(&store, &request(&[])).await, ["acl-b"]);

    // One matching principal: A joins B, C stays hidden.
    assert_eq!(
        scope_ids(&store, &request(&["group:hr"])).await,
        ["acl-a", "acl-b"]
    );
    assert_eq!(
        verified(&store, &request(&["group:hr"])).await,
        ["acl-a", "acl-b"]
    );

    // A non-matching principal changes nothing.
    assert_eq!(
        scope_ids(&store, &request(&["group:finance"])).await,
        ["acl-b"]
    );

    // Two principals intersect both ACLs; the sets are the grant, not a union
    // of everything the key could name.
    assert_eq!(
        scope_ids(&store, &request(&["group:hr", "user:42"])).await,
        ["acl-a", "acl-b", "acl-c"]
    );

    // By-id visibility uses the same predicate as the scope.
    let tenant = TenantId(TENANT.to_string());
    assert!(
        store
            .find_visible_meta_by_id(&tenant, &DocumentId("acl-a".into()), false, &[])
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .find_visible_meta_by_id(
                &tenant,
                &DocumentId("acl-a".into()),
                false,
                &["group:hr".to_string()],
            )
            .await
            .unwrap()
            .is_some()
    );

    // The document list applies the same clause to rows and to its total.
    let hidden = store
        .list_page(&tenant, 10, kb, None, None, &[])
        .await
        .unwrap();
    assert_eq!(hidden.documents.len(), 1);
    assert_eq!(hidden.documents[0].id.0, "acl-b");
    assert_eq!(hidden.total, 1);
    let entitled = store
        .list_page(&tenant, 10, kb, None, None, &["group:hr".to_string()])
        .await
        .unwrap();
    let mut ids: Vec<String> = entitled.documents.iter().map(|d| d.id.0.clone()).collect();
    ids.sort();
    assert_eq!(ids, ["acl-a", "acl-b"]);
    assert_eq!(entitled.total, 2);

    // The stored rows carry the ACL back to the API.
    let meta = store
        .find_meta_by_id(&tenant, &DocumentId("acl-a".into()), false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(meta.acl_principals, vec!["group:hr".to_string()]);
    assert_eq!(
        store
            .find_meta_by_id(&tenant, &DocumentId("acl-b".into()), false)
            .await
            .unwrap()
            .unwrap()
            .acl_principals,
        Vec::<String>::new()
    );

    // PATCH replaces the list; clearing it restores the old visibility.
    let patched = store
        .patch_document(
            &tenant,
            &DocumentId("acl-a".into()),
            &kb_ports::PatchDocument {
                acl_principals: Some(vec![]),
                ..kb_ports::PatchDocument::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(patched.acl_principals.is_empty());
    assert_eq!(scope_ids(&store, &request(&[])).await, ["acl-a", "acl-b"]);
}
