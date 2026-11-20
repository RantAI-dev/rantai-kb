//! `POST /v1/documents` — the content-hash dedupe (`KB_UPLOAD_DEDUPE`).
//!
//! The same bytes twice cost a whole ingest — extraction, embedding,
//! indexing — and produce a second copy of the same answer. The dedupe
//! refuses the second upload and names the document that already holds the
//! file. It is scoped to the tenant: another tenant's identical file is not
//! the caller's business and must not even be mentioned.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_core::policy::UploadDedupe;
use kb_ports::{DocumentStore, TenantId};
use sha2::{Digest, Sha256};

use harness::*;

const TENANT: &str = "tenant-a";
const OTHER: &str = "tenant-b";

/// The bytes of a file, as the multipart body carries them.
fn file_part(name: &str, content: &str) -> String {
    format!("{name}\u{1}{content}")
}

/// The SHA-256 of a file's bytes, in the hex form the store keeps.
fn sha256_of(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

/// Upload a file and answer with the response body.
async fn upload(h: &Harness, tenant: &str, name: &str, content: &str) -> serde_json::Value {
    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(tenant),
            vec![("file", file_part(name, content))],
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "the upload was refused"
    );
    json_body(response).await
}

/// A knowledge base the tenant owns, so an upload can be filed under it.
fn seed_kbs(h: &Harness, tenant: &str, ids: &[&str]) {
    h.documents.kb_records.write().unwrap().insert(
        TenantId(tenant.to_string()),
        ids.iter()
            .map(|id| kb_ports::KnowledgeBaseRecord {
                id: kb_ports::KnowledgeBaseId((*id).into()),
                tenant_id: TenantId(tenant.to_string()),
                name: (*id).into(),
                description: None,
                color: None,
                created_at: "2026-01-01T00:00:00.000Z".into(),
                updated_at: "2026-01-01T00:00:00.000Z".into(),
            })
            .collect(),
    );
}

#[tokio::test]
async fn the_same_bytes_twice_are_refused_with_the_existing_document() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let first = upload(&h, TENANT, "report.txt", "the same bytes").await;
    let first_id = first["id"].as_str().expect("id").to_string();

    // The same bytes under a different name: the content is what counts.
    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![("file", file_part("copy.txt", "the same bytes"))],
        )
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = json_body(response).await;

    assert_eq!(
        body["existingDocumentId"],
        first_id.as_str(),
        "the refusal names the document that already holds the file"
    );
    let message = body["error"].as_str().unwrap_or_default().to_string();
    assert!(
        message.contains("KB_UPLOAD_DEDUPE"),
        "the message names the toggle: {message}"
    );

    // The refused upload stored nothing: one document, one blob.
    let docs = h.documents.docs.read().unwrap();
    assert_eq!(docs[&TenantId(TENANT.to_string())].len(), 1);
    assert_eq!(h.blobs.objects.read().unwrap().len(), 1);
}

#[tokio::test]
async fn a_different_file_is_not_refused() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    upload(&h, TENANT, "one.txt", "first file").await;
    let second = upload(&h, TENANT, "two.txt", "a different file").await;
    assert!(second["id"].is_string());
}

#[tokio::test]
async fn another_tenants_file_never_refuses_an_upload() {
    // The dedupe is the store's scope, not the caller's: the tenant is part
    // of the statement that looks for the duplicate.
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.with_tenant(OTHER);

    let theirs = upload(&h, OTHER, "theirs.txt", "the same bytes").await;
    let their_id = theirs["id"].as_str().expect("id").to_string();

    // The same bytes, the other tenant.
    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![("file", file_part("mine.txt", "the same bytes"))],
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "another tenant's file is not this tenant's duplicate"
    );
    let body = json_body(response).await;
    assert!(
        body.get("existingDocumentId").is_none(),
        "the refusal never names another tenant's document: {body}"
    );
    assert_ne!(body["id"], their_id.as_str());
}

#[tokio::test]
async fn a_tenant_list_never_shows_another_tenants_document() {
    // The same scope rule as the refusal: another tenant's identical file is
    // invisible, not refused.
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.with_tenant(OTHER);
    upload(&h, OTHER, "theirs.txt", "the same bytes").await;
    upload(&h, TENANT, "mine.txt", "the same bytes").await;

    let body = json_body(h.send("GET", "/v1/documents", &tenant_key(TENANT)).await).await;
    let documents = body["documents"].as_array().expect("documents");
    assert_eq!(documents.len(), 1, "{body}");
    assert_eq!(documents[0]["title"], "mine");
}

#[tokio::test]
async fn a_deleted_document_does_not_block_an_upload() {
    // A document the tenant deleted is not holding the file any more.
    let h = Harness::new();
    h.with_tenant(TENANT);

    let first = upload(&h, TENANT, "report.txt", "the same bytes").await;
    let first_id = first["id"].as_str().expect("id").to_string();

    let response = h
        .send(
            "DELETE",
            &format!("/v1/documents/{first_id}"),
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let second = upload(&h, TENANT, "report.txt", "the same bytes").await;
    assert_ne!(second["id"], first_id.as_str());
}

#[tokio::test]
async fn a_failed_document_does_not_block_an_upload() {
    // A document that failed to ingest is exactly the one a caller wants to
    // try again.
    let h = Harness::new();
    h.with_tenant(TENANT);

    let first = upload(&h, TENANT, "report.txt", "the same bytes").await;
    let first_id = first["id"].as_str().expect("id").to_string();
    h.documents
        .set_status(&kb_ports::DocumentId(first_id.clone()), "failed")
        .await
        .unwrap();

    let second = upload(&h, TENANT, "report.txt", "the same bytes").await;
    assert_ne!(second["id"], first_id.as_str());
}

#[tokio::test]
async fn the_dedupe_off_stores_the_file_again() {
    let h = Harness::with_upload_dedupe(UploadDedupe::Off);
    h.with_tenant(TENANT);

    let first = upload(&h, TENANT, "report.txt", "the same bytes").await;
    let second = upload(&h, TENANT, "report.txt", "the same bytes").await;
    assert_ne!(first["id"], second["id"], "two documents, as asked for");

    let docs = h.documents.docs.read().unwrap();
    assert_eq!(docs[&TenantId(TENANT.to_string())].len(), 2);
}

#[tokio::test]
async fn the_same_file_in_another_knowledge_base_is_not_a_duplicate() {
    // The duplicate has to be the same *document*, filed where this upload is
    // filed: the same bytes in two different knowledge bases are two
    // documents.
    let h = Harness::new();
    h.with_tenant(TENANT);
    seed_kbs(&h, TENANT, &["kb-one", "kb-two"]);

    let first = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![
                ("knowledgeBaseIds", "kb-one".to_string()),
                ("file", file_part("report.txt", "the same bytes")),
            ],
        )
        .await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first_id = json_body(first).await["id"]
        .as_str()
        .expect("id")
        .to_string();

    let second = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![
                ("knowledgeBaseIds", "kb-two".to_string()),
                ("file", file_part("report.txt", "the same bytes")),
            ],
        )
        .await;
    assert_eq!(
        second.status(),
        StatusCode::ACCEPTED,
        "another knowledge base is another document"
    );
    assert_ne!(json_body(second).await["id"], first_id.as_str());
}

#[tokio::test]
async fn the_same_file_in_the_same_knowledge_base_is_a_duplicate() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    seed_kbs(&h, TENANT, &["kb-one"]);

    let first = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![
                ("knowledgeBaseIds", "kb-one".to_string()),
                ("file", file_part("report.txt", "the same bytes")),
            ],
        )
        .await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first_id = json_body(first).await["id"]
        .as_str()
        .expect("id")
        .to_string();

    let second = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![
                ("knowledgeBaseIds", "kb-one".to_string()),
                ("file", file_part("report.txt", "the same bytes")),
            ],
        )
        .await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(second).await["existingDocumentId"],
        first_id.as_str()
    );
}

#[tokio::test]
async fn the_document_carries_the_hash_of_its_bytes() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    upload(&h, TENANT, "report.txt", "the same bytes").await;

    let body = json_body(h.send("GET", "/v1/documents", &tenant_key(TENANT)).await).await;
    let documents = body["documents"].as_array().expect("documents");
    assert_eq!(documents[0]["sha256"], sha256_of("the same bytes").as_str());

    let id = documents[0]["id"].as_str().expect("id").to_string();
    let body = json_body(
        h.send("GET", &format!("/v1/documents/{id}"), &tenant_key(TENANT))
            .await,
    )
    .await;
    assert_eq!(
        body["document"]["sha256"],
        sha256_of("the same bytes").as_str()
    );
}
