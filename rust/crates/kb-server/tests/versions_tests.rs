//! `RET-16`: versioned documents and point-in-time search.
//!
//! The router runs over the in-process fakes, so the filter under test is the
//! one in `FakeDocumentStore::build_scope` — which deliberately mirrors
//! `scope_sql` in `kb-store-pg/src/store.rs`. Every test seeds a hit for each
//! version, so a post-filtering implementation would still pass unless the
//! scope really narrowed before the arms ran.
//!
//! The two stores' filters must stay in sync. Both live here:
//! * SQL: `rust/crates/kb-store-pg/src/store.rs`, `scope_sql` (unit-tested there);
//! * fake: `rust/crates/kb-engine/src/fakes.rs`, `FakeDocumentStore::build_scope`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_engine::fakes::make_fake_hit;
use kb_ports::TenantId;

use harness::{Harness, json_body, tenant_key};

const TENANT: &str = "tenant-versions";

struct Fixture {
    h: Harness,
    v1: String,
    v2: String,
    plain: String,
}

impl Fixture {
    async fn new() -> Self {
        let h = Harness::new();
        h.with_tenant(TENANT);
        let v1 = upload(&h, "cuti", "2024", "2024-01-01", "cuti tahunan 12 hari").await;
        let v2 = upload(&h, "cuti", "2025", "2025-01-01", "cuti tahunan 14 hari").await;
        let plain = upload(&h, "", "", "", "cuti tanpa versi").await;
        for (id, text) in [
            (&v1, "cuti tahunan 12 hari"),
            (&v2, "cuti tahunan 14 hari"),
            (&plain, "cuti tanpa versi"),
        ] {
            h.chunks.vector_hits.write().unwrap().push(make_fake_hit(
                &format!("{id}_0"),
                id,
                "Kebijakan Cuti",
                text,
                0,
                0.9,
            ));
        }
        Self { h, v1, v2, plain }
    }

    async fn search(&self, body: serde_json::Value) -> serde_json::Value {
        let response = self
            .h
            .send_json("POST", "/v1/search", &tenant_key(TENANT), body)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await
    }

    async fn search_status(&self, body: serde_json::Value) -> StatusCode {
        self.h
            .send_json("POST", "/v1/search", &tenant_key(TENANT), body)
            .await
            .status()
    }
}

async fn upload(h: &Harness, doc_key: &str, version: &str, from: &str, text: &str) -> String {
    let mut parts = vec![
        ("title", "Kebijakan Cuti".to_string()),
        ("file", format!("cuti-{version}.txt\u{1}{text}")),
    ];
    if !doc_key.is_empty() {
        parts.push(("docKey", doc_key.to_string()));
    }
    if !version.is_empty() {
        parts.push(("version", version.to_string()));
    }
    if !from.is_empty() {
        parts.push(("effectiveFrom", from.to_string()));
    }
    let response = h
        .send_multipart("/v1/documents", &tenant_key(TENANT), parts)
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    json_body(response).await["id"]
        .as_str()
        .expect("document id")
        .to_string()
}

fn contents(body: &serde_json::Value) -> Vec<String> {
    body["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["content"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn uploading_a_new_version_retires_the_previous_one() {
    let f = Fixture::new().await;
    let docs = f.h.documents.docs.read().unwrap();
    let list = &docs[&TenantId(TENANT.to_string())];
    let old = list.iter().find(|d| d.id.0 == f.v1).unwrap();
    let new = list.iter().find(|d| d.id.0 == f.v2).unwrap();
    assert_eq!(old.doc_key.as_deref(), Some("cuti"));
    assert!(!old.is_latest);
    assert_eq!(old.effective_to.as_deref(), Some("2025-01-01T00:00:00Z"));
    assert!(new.is_latest);
    assert_eq!(new.supersedes_document_id.as_deref(), Some(f.v1.as_str()));
    assert_eq!(new.effective_from.as_deref(), Some("2025-01-01T00:00:00Z"));
}

#[tokio::test]
async fn default_search_returns_only_the_latest_version() {
    let f = Fixture::new().await;
    let body = f.search(serde_json::json!({"query": "cuti tahunan"})).await;
    assert_eq!(
        contents(&body),
        vec!["cuti tahunan 14 hari", "cuti tanpa versi"]
    );
    assert!(body["multiVersionWarning"].is_null());
    let v2 = body["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["documentId"] == f.v2)
        .unwrap();
    assert_eq!(v2["docKey"], "cuti");
    assert_eq!(v2["version"], "2025");
    assert_eq!(v2["effectiveFrom"], "2025-01-01T00:00:00Z");
    let source = body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["documentId"] == f.v2)
        .unwrap();
    assert_eq!(source["docKey"], "cuti");
    assert_eq!(source["version"], "2025");
}

#[tokio::test]
async fn as_of_returns_exactly_the_version_in_force() {
    let f = Fixture::new().await;
    let body = f
        .search(serde_json::json!({"query": "cuti tahunan", "asOf": "2024-06-01"}))
        .await;
    assert_eq!(
        contents(&body),
        vec!["cuti tahunan 12 hari", "cuti tanpa versi"]
    );
    let chunk = body["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["documentId"] == f.v1)
        .unwrap();
    assert_eq!(chunk["version"], "2024");
    assert_eq!(chunk["effectiveTo"], "2025-01-01T00:00:00Z");

    // The boundary is half-open: v2 takes effect exactly at its effectiveFrom.
    let body = f
        .search(serde_json::json!({"query": "cuti tahunan", "asOf": "2025-01-01"}))
        .await;
    assert!(contents(&body).contains(&"cuti tahunan 14 hari".to_string()));
    assert!(!contents(&body).contains(&"cuti tahunan 12 hari".to_string()));
}

#[tokio::test]
async fn include_history_returns_both_versions_and_a_warning() {
    let f = Fixture::new().await;
    let body = f
        .search(serde_json::json!({"query": "cuti tahunan", "includeHistory": true}))
        .await;
    let all = contents(&body);
    assert!(all.contains(&"cuti tahunan 12 hari".to_string()), "{all:?}");
    assert!(all.contains(&"cuti tahunan 14 hari".to_string()), "{all:?}");
    assert_eq!(
        body["multiVersionWarning"],
        serde_json::json!([{
            "docKey": "cuti",
            "documentIds": [f.v1, f.v2],
            "versions": ["2024", "2025"],
        }])
    );
}

#[tokio::test]
async fn as_of_never_widens_a_kb_limited_key() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    // A writer key with the whole tenant, and a reader key limited to a KB the
    // versioned documents are NOT in.
    h.documents
        .insert_api_key("writer-key", TenantId(TENANT.to_string()));
    h.documents.kb_records.write().unwrap().insert(
        TenantId(TENANT.to_string()),
        vec!["kb-a", "kb-b"]
            .into_iter()
            .map(|id| kb_ports::KnowledgeBaseRecord {
                id: kb_ports::KnowledgeBaseId(id.to_string()),
                tenant_id: TenantId(TENANT.to_string()),
                name: id.to_string(),
                description: None,
                color: None,
                created_at: String::new(),
                updated_at: String::new(),
            })
            .collect(),
    );
    h.documents.insert_api_key_with_scopes(
        &tenant_key(TENANT),
        TenantId(TENANT.to_string()),
        vec!["kb:read".into(), "kb:write".into()],
        vec![kb_ports::KnowledgeBaseId("kb-b".into())],
    );
    let response = h
        .send_multipart(
            "/v1/documents",
            "writer-key",
            vec![
                ("title", "Kebijakan Cuti".into()),
                ("docKey", "cuti".into()),
                ("version", "2024".into()),
                ("effectiveFrom", "2024-01-01".into()),
                ("knowledgeBaseIds", "kb-a".into()),
                ("file", "cuti.txt\u{1}cuti tahunan 12 hari".into()),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let id = json_body(response).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "c_0",
        &id,
        "Kebijakan Cuti",
        "cuti tahunan 12 hari",
        0,
        0.9,
    ));

    for body in [
        serde_json::json!({"query": "cuti", "asOf": "2024-06-01"}),
        serde_json::json!({"query": "cuti", "includeHistory": true}),
    ] {
        let response = h
            .send_json("POST", "/v1/search", &tenant_key(TENANT), body)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert!(
            body["chunks"].as_array().unwrap().is_empty(),
            "asOf/history cannot reach a document outside the key's KB: {body}"
        );
    }
}

#[tokio::test]
async fn a_document_without_doc_key_is_visible_in_every_mode() {
    let f = Fixture::new().await;
    for body in [
        serde_json::json!({"query": "cuti"}),
        serde_json::json!({"query": "cuti", "asOf": "2020-01-01"}),
        serde_json::json!({"query": "cuti", "includeHistory": true}),
    ] {
        let response = f.search(body).await;
        let ids: Vec<&str> = response["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["documentId"].as_str())
            .collect();
        assert!(ids.contains(&f.plain.as_str()), "{ids:?}");
    }
}

#[tokio::test]
async fn a_bad_as_of_is_a_400() {
    let f = Fixture::new().await;
    for value in ["not-a-date", "2024-13-01", "2024-02-30", ""] {
        assert_eq!(
            f.search_status(serde_json::json!({"query": "cuti", "asOf": value}))
                .await,
            StatusCode::BAD_REQUEST,
            "asOf={value}"
        );
    }
    assert_eq!(
        f.search_status(serde_json::json!({"query": "cuti", "asOf": 123}))
            .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn patch_sets_version_fields_and_a_bad_date_is_a_400() {
    let f = Fixture::new().await;
    let response =
        f.h.send_json(
            "PATCH",
            &format!("/v1/documents/{}", f.plain),
            &tenant_key(TENANT),
            serde_json::json!({"docKey": "cuti", "version": "2026", "effectiveFrom": "2026-01-01"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["document"]["docKey"], "cuti");
    assert_eq!(body["document"]["version"], "2026");
    assert_eq!(body["document"]["effectiveFrom"], "2026-01-01T00:00:00Z");
    assert_eq!(body["document"]["isLatest"], true);

    let response =
        f.h.send_json(
            "PATCH",
            &format!("/v1/documents/{}", f.plain),
            &tenant_key(TENANT),
            serde_json::json!({"effectiveTo": "kemarin"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_bad_upload_date_is_a_400_and_leaves_no_document() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let response = h
        .send_multipart(
            "/v1/documents",
            &tenant_key(TENANT),
            vec![
                ("file", "cuti.txt\u{1}cuti tahunan 12 hari".into()),
                ("docKey", "cuti".into()),
                ("effectiveFrom", "kapan-saja".into()),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        h.documents
            .docs
            .read()
            .unwrap()
            .get(&TenantId(TENANT.to_string()))
            .is_none_or(Vec::is_empty)
    );
}
