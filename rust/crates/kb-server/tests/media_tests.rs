//! Document sub-resources: `/raw`, `/assets` and `/intelligence`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_ports::{BlobStore, DocumentId, EntityView, RelationView, TenantId};

use harness::*;

const TENANT: &str = "tenant-a";
const OTHER: &str = "tenant-b";

#[tokio::test]
async fn raw_streams_the_stored_bytes_with_the_documents_mime_type() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Doc"));
    h.blobs
        .upload(
            "documents/tenant-a/doc-1/doc-1.pdf",
            b"%PDF-1.7 bytes".to_vec(),
            "application/pdf",
        )
        .await
        .unwrap();

    let response = h
        .send("GET", "/v1/documents/doc-1/raw", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/pdf"
    );
    assert_eq!(response.headers().get("content-length").unwrap(), "14");
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "*"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), b"%PDF-1.7 bytes");
}

#[tokio::test]
async fn raw_for_a_document_without_a_stored_file_is_a_404() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let mut doc = doc_meta("doc-1", "Doc");
    doc.s3_key = None;
    h.documents.insert_doc(TenantId(TENANT.to_string()), doc);

    let response = h
        .send("GET", "/v1/documents/doc-1/raw", &tenant_key(TENANT))
        .await;
    assert_error(
        response,
        StatusCode::NOT_FOUND,
        "Document has no stored file",
    )
    .await;
}

#[tokio::test]
async fn raw_across_tenants_is_a_404_and_downloads_nothing() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let mut doc = doc_meta("doc-1", "Theirs");
    doc.s3_key = Some("documents/tenant-b/doc-1/doc-1.pdf".to_string());
    h.documents.insert_doc(TenantId(OTHER.to_string()), doc);
    h.blobs
        .upload(
            "documents/tenant-b/doc-1/doc-1.pdf",
            b"secret".to_vec(),
            "application/pdf",
        )
        .await
        .unwrap();

    let response = h
        .send("GET", "/v1/documents/doc-1/raw", &tenant_key(TENANT))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
    assert!(
        h.blobs
            .objects
            .read()
            .unwrap()
            .contains_key("documents/tenant-b/doc-1/doc-1.pdf")
    );
}

#[tokio::test]
async fn raw_without_kb_read_is_a_403() {
    let h = Harness::new();
    h.documents.insert_api_key_with_scopes(
        &tenant_key(TENANT),
        TenantId(TENANT.to_string()),
        vec!["kb:write".into()],
        vec![],
    );
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Doc"));

    // The `Authed` extractor itself refuses a key without kb:read.
    let response = h
        .send("GET", "/v1/documents/doc-1/raw", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn asset_is_served_only_inside_its_own_document_namespace() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Doc"));
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-2", "Other"));
    h.blobs
        .upload(
            "documents/tenant-a/doc-1/assets/fig-1.png",
            b"\x89PNG".to_vec(),
            "image/png",
        )
        .await
        .unwrap();
    h.blobs
        .upload(
            "documents/tenant-a/doc-2/assets/fig-1.png",
            b"\x89PNG-other".to_vec(),
            "image/png",
        )
        .await
        .unwrap();

    let response = h
        .send(
            "GET",
            "/v1/documents/doc-1/assets?key=documents%2Ftenant-a%2Fdoc-1%2Fassets%2Ffig-1.png",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").unwrap(), "image/png");
    let bytes = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), b"\x89PNG");
}

#[tokio::test]
async fn asset_rejects_traversal_other_documents_and_other_tenants() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Doc"));
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-2", "Other"));
    h.blobs
        .upload(
            "documents/tenant-a/doc-2/assets/fig-1.png",
            b"other".to_vec(),
            "image/png",
        )
        .await
        .unwrap();

    let rejects = [
        // Another document of the same tenant.
        "documents%2Ftenant-a%2Fdoc-2%2Fassets%2Ffig-1.png",
        // A traversal that starts with this document's prefix as a string.
        "documents%2Ftenant-a%2Fdoc-1%2Fassets%2F..%2F..%2Fdoc-2%2Fassets%2Ffig-1.png",
        // Another tenant's namespace.
        "documents%2Ftenant-b%2Fdoc-1%2Fassets%2Ffig-1.png",
        // A backslash that some object stores normalise.
        "documents%2Ftenant-a%2Fdoc-1%2Fassets%2F..%5Cdoc-2.png",
    ];
    for key in rejects {
        let response = h
            .send(
                "GET",
                &format!("/v1/documents/doc-1/assets?key={key}"),
                &tenant_key(TENANT),
            )
            .await;
        assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
    }

    // A missing key is the same 404, not a 400: the response must not tell a
    // caller that it guessed the shape right.
    let response = h
        .send("GET", "/v1/documents/doc-1/assets", &tenant_key(TENANT))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn intelligence_returns_entities_relations_and_stats() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Doc"));
    h.entities.intelligences.write().unwrap().insert(
        DocumentId("doc-1".into()),
        kb_ports::DocumentIntelligenceData {
            entities: vec![
                EntityView {
                    id: "entity:acme".into(),
                    name: "Acme".into(),
                    r#type: "organization".into(),
                    confidence: 0.9,
                    metadata: serde_json::json!({"source": "pattern"}),
                },
                EntityView {
                    id: "entity:acme".into(),
                    name: "Acme".into(),
                    r#type: "organization".into(),
                    confidence: 0.5,
                    metadata: serde_json::json!({}),
                },
                EntityView {
                    id: "entity:acme2".into(),
                    name: "Acme UK".into(),
                    r#type: "organization".into(),
                    confidence: 0.4,
                    metadata: serde_json::json!({}),
                },
            ],
            relations: vec![RelationView {
                id: "rel-1".into(),
                from_id: "entity:acme".into(),
                out: "entity:jane".into(),
                relation_type: "WORKS_FOR".into(),
                confidence: 0.7,
                metadata: serde_json::json!({"context": "Jane works at Acme"}),
            }],
        },
    );

    let response = h
        .send(
            "GET",
            "/v1/documents/doc-1/intelligence",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["stats"]["totalEntities"], 3);
    assert_eq!(body["stats"]["totalRelations"], 1);
    assert_eq!(body["stats"]["entityTypes"], 1);
    assert_eq!(body["stats"]["relationTypes"], 1);
    assert_eq!(body["entities"][0]["type"], "organization");
    assert_eq!(body["entities"][0]["confidence"], 0.9);
    assert_eq!(body["relations"][0]["relation_type"], "WORKS_FOR");
    assert_eq!(body["relations"][0]["in"], "entity:acme");
    assert_eq!(body["relations"][0]["out"], "entity:jane");
    assert_eq!(
        body["relations"][0]["metadata"]["context"],
        "Jane works at Acme"
    );
}

#[tokio::test]
async fn intelligence_status_follows_the_document_status() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let mut doc = doc_meta("doc-1", "Doc");
    doc.status = "processing".to_string();
    h.documents.insert_doc(TenantId(TENANT.to_string()), doc);

    let response = h
        .send(
            "GET",
            "/v1/documents/doc-1/intelligence",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(json_body(response).await["status"], "processing");
}

#[tokio::test]
async fn intelligence_across_tenants_is_a_404_and_queries_no_graph() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(OTHER.to_string()), doc_meta("doc-1", "Theirs"));

    let response = h
        .send(
            "GET",
            "/v1/documents/doc-1/intelligence",
            &tenant_key(TENANT),
        )
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}
