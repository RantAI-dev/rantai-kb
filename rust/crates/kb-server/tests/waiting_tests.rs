//! The `waiting_for_ocr` state, as a client sees it (`#36`).
//!
//! A job whose backend was not there parks in `waiting_for_ocr`, and the
//! document it ingests says so too. Both are plain strings in the database,
//! so the routes hand them over as they are: a client that polls the job
//! sees that the job is waiting, not that it failed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_ports::{DocumentStore, TenantId};

use harness::*;

const TENANT: &str = "tenant-a";

/// A job that is waiting for a backend, with the reason it is waiting.
fn waiting_job(h: &Harness, id: &str, tenant: &TenantId, document_id: Option<&str>) {
    let view = kb_ports::JobView {
        id: id.to_string(),
        status: "waiting_for_ocr".to_string(),
        step: Some("extracting".to_string()),
        progress: 20,
        step_current: Some(1),
        step_total: Some(3),
        eta_seconds: None,
        attempt: 2,
        error: Some("the OCR provider is unreachable".to_string()),
        document_id: document_id.map(str::to_string),
    };
    h.jobs
        .job_views
        .write()
        .unwrap()
        .insert((tenant.clone(), id.to_string()), view);
    if let Some(doc) = document_id {
        h.documents.insert_doc(tenant.clone(), doc_meta(doc, "Doc"));
    }
}

#[tokio::test]
async fn a_waiting_job_says_it_is_waiting() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    waiting_job(&h, "job-1", &TenantId(TENANT.to_string()), Some("doc-1"));

    let response = h.send("GET", "/v1/jobs/job-1", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["status"], "waiting_for_ocr");
    // The reason is told, so an operator knows what the job is waiting for.
    assert_eq!(body["error"], "the OCR provider is unreachable");
    assert_eq!(body["documentId"], "doc-1");
}

#[tokio::test]
async fn a_waiting_document_says_so_in_the_list_and_the_detail() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let tenant = TenantId(TENANT.to_string());
    waiting_job(&h, "job-1", &tenant, Some("doc-1"));
    // The engine marks the document as waiting while its job waits.
    h.documents
        .set_status(&kb_ports::DocumentId::new_unchecked("doc-1"), "waiting_for_ocr")
        .await
        .unwrap();

    let response = h.send("GET", "/v1/documents", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let documents = body["documents"].as_array().expect("documents array");
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0]["status"], "waiting_for_ocr");

    let response = h
        .send("GET", "/v1/documents/doc-1", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["document"]["status"], "waiting_for_ocr");
}

#[tokio::test]
async fn a_waiting_job_cannot_be_retried_by_hand() {
    // The job is not broken: it is waiting for a backend. The worker resumes
    // it by itself, so a retry would only make the operator's press of the
    // button a second way to do the same thing.
    let h = Harness::new();
    h.with_tenant(TENANT);
    waiting_job(&h, "job-1", &TenantId(TENANT.to_string()), Some("doc-1"));

    let response = h
        .send("POST", "/v1/jobs/job-1/retry", &tenant_key(TENANT))
        .await;
    assert_error(
        response,
        StatusCode::CONFLICT,
        "Only a failed job can be retried",
    )
    .await;
}
