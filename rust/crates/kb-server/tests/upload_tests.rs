#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `POST /v1/documents` — the multipart upload path.

mod harness;

use axum::http::StatusCode;
use kb_ports::TenantId;

use harness::*;

const TENANT: &str = "tenant-a";

#[tokio::test]
async fn upload_returns_202_with_document_and_job_ids() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![
                ("title", "Quarterly report".to_string()),
                ("categories", "finance,quarterly".to_string()),
                ("subcategory", "q3".to_string()),
                (
                    "file",
                    "report.txt\u{1}Quarterly revenue numbers".to_string(),
                ),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json_body(response).await;

    let document_id = body["id"].as_str().expect("document id").to_string();
    let job_id = body["jobId"].as_str().expect("job id").to_string();
    assert_eq!(body["status"], "processing");
    assert_eq!(body["title"], "Quarterly report");
    assert_eq!(body["knowledgeBaseIds"], serde_json::json!([]));

    // The document row exists for this tenant with the parsed metadata.
    let docs = h.documents.docs.read().unwrap();
    let stored = docs[&TenantId(TENANT.to_string())]
        .iter()
        .find(|d| d.id.0 == document_id)
        .expect("stored document");
    assert_eq!(stored.title, "Quarterly report");
    assert_eq!(stored.categories, vec!["finance", "quarterly"]);
    assert_eq!(stored.subcategory.as_deref(), Some("q3"));
    assert_eq!(stored.status, "processing");
    assert_eq!(stored.mime_type.as_deref(), Some("text/plain"));
    drop(docs);

    // The blob was written under the document's own namespace.
    let key = format!("documents/{TENANT}/{document_id}/report.txt");
    assert_eq!(
        h.blobs.objects.read().unwrap().get(&key).map(Vec::len),
        Some("Quarterly revenue numbers".len())
    );

    // The job carries the parsed parameters, not the raw form.
    let job = kb_ports::JobStore::get_job(&h.jobs, &TenantId(TENANT.to_string()), &job_id)
        .await
        .unwrap()
        .expect("job view");
    assert_eq!(job.status, "pending");
    assert_eq!(job.step.as_deref(), Some("queued"));
    assert_eq!(job.document_id.as_deref(), Some(document_id.as_str()));
    let pending = h.jobs.pending.read().unwrap();
    let record = pending.iter().find(|j| j.id == job_id).expect("job record");
    let params = record.params.clone().expect("params");
    assert_eq!(params["title"], "Quarterly report");
    assert_eq!(params["useCombined"], true);
    assert_eq!(params["figureMode"], "auto");
    assert_eq!(
        params["categories"],
        serde_json::json!(["finance", "quarterly"])
    );
    assert_eq!(params["subcategory"], "q3");
}

#[tokio::test]
async fn upload_defaults_title_to_the_filename_without_extension() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![("file", "notes.txt\u{1}hello".to_string())],
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(json_body(response).await["title"], "notes");
}

#[tokio::test]
async fn upload_reads_categories_as_a_json_array_or_csv() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![
                ("categories", r#"["a","b"]"#.to_string()),
                ("file", "notes.txt\u{1}hello".to_string()),
            ],
        )
        .await;
    let document_id = json_body(response).await["id"]
        .as_str()
        .expect("id")
        .to_string();
    let docs = h.documents.docs.read().unwrap();
    let stored = docs[&TenantId(TENANT.to_string())]
        .iter()
        .find(|d| d.id.0 == document_id)
        .expect("document");
    assert_eq!(stored.categories, vec!["a", "b"]);
}

#[tokio::test]
async fn upload_binds_only_knowledge_bases_the_key_may_use() {
    let h = Harness::new();
    h.documents.insert_api_key_with_scopes(
        &tenant_key(TENANT),
        TenantId(TENANT.to_string()),
        vec!["kb:read".into(), "kb:write".into()],
        vec![kb_ports::KnowledgeBaseId("kb-allowed".into())],
    );

    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![
                ("knowledgeBaseIds", "kb-allowed,kb-denied".to_string()),
                ("file", "notes.txt\u{1}hello".to_string()),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json_body(response).await;
    // The key's restriction wins: the denied base is dropped, not linked.
    assert_eq!(body["knowledgeBaseIds"], serde_json::json!(["kb-allowed"]));
    let document_id = body["id"].as_str().expect("id");
    let docs = h.documents.docs.read().unwrap();
    let stored = docs[&TenantId(TENANT.to_string())]
        .iter()
        .find(|d| d.id.0 == document_id)
        .expect("document");
    assert_eq!(stored.knowledge_base_ids, vec!["kb-allowed"]);
}

#[tokio::test]
async fn upload_without_a_file_is_a_400() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![("title", "no file here".to_string())],
        )
        .await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "No file provided (field: file)",
    )
    .await;
}

#[tokio::test]
async fn upload_of_an_unaccepted_extension_is_a_415() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![("file", "tool.exe\u{1}binary".to_string())],
        )
        .await;
    assert_error(
        response,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "\"tool.exe\" has an unsupported type. See GET /v1/formats for the accepted list.",
    )
    .await;
    assert!(
        h.documents
            .docs
            .read()
            .unwrap()
            .get(&TenantId(TENANT.to_string()))
            .is_none_or(Vec::is_empty)
    );
}

#[tokio::test]
async fn upload_without_kb_write_is_a_403() {
    let h = Harness::new();
    h.documents.insert_api_key_with_scopes(
        &tenant_key(TENANT),
        TenantId(TENANT.to_string()),
        vec!["kb:read".into()],
        vec![],
    );

    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![("file", "notes.txt\u{1}hello".to_string())],
        )
        .await;
    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "API key lacks scope kb:write",
    )
    .await;
}

#[tokio::test]
async fn upload_body_limit_is_derived_from_the_max_file_size() {
    // The extractor must refuse a body larger than the limit it is given, or an
    // unbounded upload would be buffered before the 413 could be produced. The
    // 413 text itself is unit-tested next to the route, where the limit is a
    // parameter, because a 50 MB test payload is not worth the memory.
    let limit = kb_server::routes::upload_body_limit();
    let max_file = kb_core::policy::MimeRegistry::kb_max_file_bytes() as usize;
    assert!(limit > max_file);
    assert!(limit - max_file <= 4 * 1024 * 1024, "slack stays bounded");
}
