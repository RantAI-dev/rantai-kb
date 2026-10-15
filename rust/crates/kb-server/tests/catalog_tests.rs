//! Job routes and the knowledge-base / category CRUD.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_ports::TenantId;

use harness::*;

const TENANT: &str = "tenant-a";
const OTHER: &str = "tenant-b";

fn failed_job(h: &Harness, id: &str, tenant: &TenantId, document_id: Option<&str>) {
    let mut view = kb_ports::JobView {
        id: id.to_string(),
        status: "failed".to_string(),
        step: Some("embedding".to_string()),
        progress: 40,
        step_current: Some(1),
        step_total: Some(3),
        eta_seconds: Some(12),
        attempt: 2,
        error: Some("extractor crashed".to_string()),
        document_id: document_id.map(str::to_string),
    };
    if let Some(doc) = document_id {
        h.documents.insert_doc(tenant.clone(), doc_meta(doc, "Doc"));
    }
    view.attempt = 2;
    h.jobs
        .job_views
        .write()
        .unwrap()
        .insert((tenant.clone(), id.to_string()), view);
}

#[tokio::test]
async fn get_job_returns_the_camel_case_progress_fields() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    failed_job(&h, "job-1", &TenantId(TENANT.to_string()), Some("doc-1"));

    let response = h.send("GET", "/v1/jobs/job-1", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["id"], "job-1");
    assert_eq!(body["status"], "failed");
    assert_eq!(body["step"], "embedding");
    assert_eq!(body["progress"], 40);
    assert_eq!(body["stepCurrent"], 1);
    assert_eq!(body["stepTotal"], 3);
    assert_eq!(body["etaSeconds"], 12);
    assert_eq!(body["attempt"], 2);
    assert_eq!(body["error"], "extractor crashed");
    assert_eq!(body["documentId"], "doc-1");
}

#[tokio::test]
async fn get_job_for_another_tenant_is_a_404() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    failed_job(&h, "job-1", &TenantId(OTHER.to_string()), None);

    let response = h.send("GET", "/v1/jobs/job-1", &tenant_key(TENANT)).await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn retry_requeues_a_failed_job_and_resets_its_document() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let tenant = TenantId(TENANT.to_string());
    failed_job(&h, "job-1", &tenant, Some("doc-1"));

    let response = h
        .send("POST", "/v1/jobs/job-1/retry", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await,
        serde_json::json!({"jobId": "job-1"})
    );

    let job = kb_ports::JobStore::get_job(&h.jobs, &tenant, "job-1")
        .await
        .unwrap()
        .expect("job");
    assert_eq!(job.status, "pending");
    assert_eq!(job.step.as_deref(), Some("queued"));
    assert_eq!(job.progress, 0);
    assert_eq!(job.step_current, None);
    assert_eq!(job.step_total, None);
    assert_eq!(job.eta_seconds, None);
    assert_eq!(job.error, None);
    assert_eq!(job.attempt, 3, "the attempt counter increments");

    let docs = h.documents.docs.read().unwrap();
    assert_eq!(docs[&tenant][0].status, "processing");
}

#[tokio::test]
async fn retry_of_a_job_that_is_not_failed_is_a_409() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let tenant = TenantId(TENANT.to_string());
    h.jobs.job_views.write().unwrap().insert(
        (tenant.clone(), "job-1".to_string()),
        kb_ports::JobView {
            id: "job-1".into(),
            status: "processing".into(),
            step: Some("chunking".into()),
            progress: 10,
            step_current: None,
            step_total: None,
            eta_seconds: None,
            attempt: 1,
            error: None,
            document_id: None,
        },
    );

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

#[tokio::test]
async fn retry_of_an_unknown_job_is_a_404_and_of_a_read_only_key_a_403() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let response = h
        .send("POST", "/v1/jobs/job-nope/retry", &tenant_key(TENANT))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;

    h.documents.insert_api_key_with_scopes(
        "reader",
        TenantId(TENANT.to_string()),
        vec!["kb:read".into()],
        vec![],
    );
    failed_job(&h, "job-2", &TenantId(TENANT.to_string()), None);
    let response = h.send("POST", "/v1/jobs/job-2/retry", "reader").await;
    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "API key lacks scope kb:write",
    )
    .await;
}

#[tokio::test]
async fn retry_across_tenants_is_a_404_and_leaves_the_job_alone() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    failed_job(&h, "job-1", &TenantId(OTHER.to_string()), None);

    let response = h
        .send("POST", "/v1/jobs/job-1/retry", &tenant_key(TENANT))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
    let views = h.jobs.job_views.read().unwrap();
    let view = &views[&(TenantId(OTHER.to_string()), "job-1".to_string())];
    assert_eq!(view.status, "failed");
    assert_eq!(view.attempt, 2);
}

#[tokio::test]
async fn create_knowledge_base_is_a_201_with_the_stored_row() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send_json(
            "POST",
            "/v1/knowledge-bases",
            &tenant_key(TENANT),
            serde_json::json!({"name": "Finance", "description": "money", "color": "#123456"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert!(body["id"].as_str().is_some());
    assert_eq!(body["tenantId"], TENANT);
    assert_eq!(body["name"], "Finance");
    assert_eq!(body["description"], "money");
    assert_eq!(body["color"], "#123456");
    assert!(body["createdAt"].as_str().is_some());
}

#[tokio::test]
async fn create_knowledge_base_without_a_name_is_a_400() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send_json(
            "POST",
            "/v1/knowledge-bases",
            &tenant_key(TENANT),
            serde_json::json!({"description": "nameless"}),
        )
        .await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Body must include 'name'",
    )
    .await;
}

#[tokio::test]
async fn patch_knowledge_base_updates_only_the_fields_sent() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let kb_id = seed_kb(&h, "kb-1", "Before", Some("keep"), Some("#fff"));

    let response = h
        .send_json(
            "PATCH",
            &format!("/v1/knowledge-bases/{kb_id}"),
            &tenant_key(TENANT),
            serde_json::json!({"name": "After"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["knowledgeBase"]["name"], "After");
    assert_eq!(body["knowledgeBase"]["description"], "keep");
    assert_eq!(body["knowledgeBase"]["color"], "#fff");
}

#[tokio::test]
async fn delete_knowledge_base_removes_the_base_and_leaves_documents() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let kb_id = seed_kb(&h, "kb-1", "Doomed", None, None);
    let mut doc = doc_meta("doc-1", "Doc");
    doc.knowledge_base_ids = vec![kb_id.clone()];
    h.documents.insert_doc(TenantId(TENANT.to_string()), doc);

    let response = h
        .send(
            "DELETE",
            &format!("/v1/knowledge-bases/{kb_id}"),
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await, serde_json::json!({"ok": true}));
    assert!(
        h.documents
            .kb_records
            .read()
            .unwrap()
            .get(&TenantId(TENANT.to_string()))
            .is_none_or(Vec::is_empty)
    );
    // The document itself is untouched: a knowledge base never deletes one.
    assert_eq!(
        h.documents.docs.read().unwrap()[&TenantId(TENANT.to_string())].len(),
        1
    );
}

#[tokio::test]
async fn knowledge_base_mutations_across_tenants_are_404s_that_change_nothing() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents.kb_records.write().unwrap().insert(
        TenantId(OTHER.to_string()),
        vec![kb_ports::KnowledgeBaseRecord {
            id: kb_ports::KnowledgeBaseId("kb-1".into()),
            tenant_id: TenantId(OTHER.to_string()),
            name: "Theirs".into(),
            description: None,
            color: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }],
    );

    let patch = h
        .send_json(
            "PATCH",
            "/v1/knowledge-bases/kb-1",
            &tenant_key(TENANT),
            serde_json::json!({"name": "Hijacked"}),
        )
        .await;
    assert_error(patch, StatusCode::NOT_FOUND, "Not found").await;
    let delete = h
        .send("DELETE", "/v1/knowledge-bases/kb-1", &tenant_key(TENANT))
        .await;
    assert_error(delete, StatusCode::NOT_FOUND, "Not found").await;
    let records = h.documents.kb_records.read().unwrap();
    assert_eq!(records[&TenantId(OTHER.to_string())][0].name, "Theirs");
}

#[tokio::test]
async fn knowledge_base_mutations_without_kb_write_are_403s() {
    let h = Harness::new();
    h.documents.insert_api_key_with_scopes(
        &tenant_key(TENANT),
        TenantId(TENANT.to_string()),
        vec!["kb:read".into()],
        vec![],
    );
    let response = h
        .send_json(
            "POST",
            "/v1/knowledge-bases",
            &tenant_key(TENANT),
            serde_json::json!({"name": "nope"}),
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
async fn categories_round_trip_through_the_bun_envelopes() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let created = h
        .send_json(
            "POST",
            "/v1/categories",
            &tenant_key(TENANT),
            serde_json::json!({"name": "policy", "label": "Policy", "color": "#abc"}),
        )
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let body = json_body(created).await;
    let id = body["category"]["id"].as_str().expect("id").to_string();
    assert_eq!(body["category"]["name"], "policy");
    assert_eq!(body["category"]["label"], "Policy");
    assert_eq!(body["category"]["color"], "#abc");

    let listed = h.send("GET", "/v1/categories", &tenant_key(TENANT)).await;
    let listed = json_body(listed).await;
    assert_eq!(listed["categories"][0]["id"], id);
    assert_eq!(listed["categories"][0]["label"], "Policy");

    let patched = h
        .send_json(
            "PATCH",
            &format!("/v1/categories/{id}"),
            &tenant_key(TENANT),
            serde_json::json!({"label": "Policies"}),
        )
        .await;
    assert_eq!(json_body(patched).await["category"]["label"], "Policies");

    let deleted = h
        .send(
            "DELETE",
            &format!("/v1/categories/{id}"),
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(json_body(deleted).await, serde_json::json!({"ok": true}));
    assert!(
        h.documents
            .categories
            .read()
            .unwrap()
            .get(&TenantId(TENANT.to_string()))
            .is_none_or(Vec::is_empty)
    );
}

#[tokio::test]
async fn category_requires_a_name_and_a_label() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let response = h
        .send_json(
            "POST",
            "/v1/categories",
            &tenant_key(TENANT),
            serde_json::json!({"name": "policy"}),
        )
        .await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Body must include 'name' and 'label'",
    )
    .await;
}

#[tokio::test]
async fn category_reads_need_kb_read_and_writes_need_kb_write() {
    let h = Harness::new();
    h.documents.insert_api_key_with_scopes(
        &tenant_key(TENANT),
        TenantId(TENANT.to_string()),
        vec!["kb:write".into()],
        vec![],
    );
    let response = h.send("GET", "/v1/categories", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn category_mutations_across_tenants_are_404s_that_change_nothing() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents.categories.write().unwrap().insert(
        TenantId(OTHER.to_string()),
        vec![kb_ports::CategoryRecord {
            id: "cat-1".into(),
            name: "theirs".into(),
            label: "Theirs".into(),
            color: None,
        }],
    );

    let patch = h
        .send_json(
            "PATCH",
            "/v1/categories/cat-1",
            &tenant_key(TENANT),
            serde_json::json!({"label": "Hijacked"}),
        )
        .await;
    assert_error(patch, StatusCode::NOT_FOUND, "Not found").await;
    let delete = h
        .send("DELETE", "/v1/categories/cat-1", &tenant_key(TENANT))
        .await;
    assert_error(delete, StatusCode::NOT_FOUND, "Not found").await;
    let categories = h.documents.categories.read().unwrap();
    assert_eq!(categories[&TenantId(OTHER.to_string())][0].label, "Theirs");
}

fn seed_kb(
    h: &Harness,
    id: &str,
    name: &str,
    description: Option<&str>,
    color: Option<&str>,
) -> String {
    h.documents.kb_records.write().unwrap().insert(
        TenantId(TENANT.to_string()),
        vec![kb_ports::KnowledgeBaseRecord {
            id: kb_ports::KnowledgeBaseId(id.to_string()),
            tenant_id: TenantId(TENANT.to_string()),
            name: name.to_string(),
            description: description.map(str::to_string),
            color: color.map(str::to_string),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        }],
    );
    id.to_string()
}
