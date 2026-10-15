//! Shared harness for the in-process HTTP tests.
//!
//! Every test drives the real router over `tower::ServiceExt::oneshot` with
//! fakes behind it: no network, no database, and the same code path a request
//! takes in production from auth onward.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Each integration test is its own binary and shares this module, so a helper
// used by only some of them reads as dead code in the rest.
#![allow(dead_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kb_engine::EngineSettings;
use kb_engine::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeEntityStore, FakeJobStore,
    FakeReranker,
};
use kb_ports::DocumentMeta;
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};
use tower::ServiceExt;

/// The port set behind a test app, so a test can assert on the fakes the
/// router is standing on.
pub struct Harness {
    pub app: axum::Router,
    pub documents: FakeDocumentStore,
    pub chunks: FakeChunkStore,
    pub blobs: FakeBlobStore,
    pub jobs: FakeJobStore,
    pub entities: FakeEntityStore,
    pub embedder: FakeEmbedder,
    pub events: Arc<EventBus>,
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    pub fn new() -> Self {
        let documents = FakeDocumentStore::default();
        let chunks = FakeChunkStore::default();
        let blobs = FakeBlobStore::default();
        let jobs = FakeJobStore::default();
        let entities = FakeEntityStore::default();
        let embedder = FakeEmbedder::default();
        let events = EventBus::new();

        let state = AppState::from_parts(
            StateParts {
                documents: Arc::new(documents.clone()),
                chunks: Arc::new(chunks.clone()),
                entities: Arc::new(entities.clone()),
                blob: Arc::new(blobs.clone()),
                embedder: Arc::new(embedder.clone()),
                jobs: Arc::new(jobs.clone()),
                events: Arc::clone(&events),
                version: "test".to_string(),
            },
            Arc::new(FakeReranker::default()),
            engine_settings(),
        );

        Self {
            app: kb_server::build_router(state),
            documents,
            chunks,
            blobs,
            jobs,
            entities,
            embedder,
            events,
        }
    }

    /// A tenant with a full-access key, the common case.
    pub fn with_tenant(&self, tenant: &str) -> &Self {
        self.documents
            .insert_api_key(&tenant_key(tenant), kb_ports::TenantId(tenant.to_string()));
        self
    }

    pub async fn send(&self, method: &str, uri: &str, key: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .method(method)
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    pub async fn send_json(
        &self,
        method: &str,
        uri: &str,
        key: &str,
        body: serde_json::Value,
    ) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .method(method)
                    .header("authorization", format!("Bearer {key}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    pub async fn send_multipart(
        &self,
        uri: &str,
        key: &str,
        parts: Vec<(&str, String)>,
    ) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .method("POST")
                    .header("authorization", format!("Bearer {key}"))
                    .header("content-type", multipart_content_type(&parts))
                    .body(Body::from(multipart_body(&parts)))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    pub async fn send_multipart_raw(
        &self,
        uri: &str,
        key: &str,
        content_type: String,
        body: Vec<u8>,
    ) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .method("POST")
                    .header("authorization", format!("Bearer {key}"))
                    .header("content-type", content_type)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}

fn engine_settings() -> EngineSettings {
    EngineSettings {
        candidate_pool_size: 20,
        rerank_final_k: 10,
        max_chunks: 10,
        min_similarity: 0.30,
        neighbour_window: 1,
        figure_limit: 3,
        expansion_enabled: false,
        expansion_paraphrases: 3,
        hybrid_bm25_enabled: true,
    }
}

/// The key a tenant fixture authenticates with.
pub fn tenant_key(tenant: &str) -> String {
    format!("{tenant}-key")
}

/// A document row the tests can seed.
pub fn doc_meta(id: &str, title: &str) -> DocumentMeta {
    DocumentMeta {
        id: kb_ports::DocumentId(id.to_string()),
        title: title.to_string(),
        categories: vec![],
        subcategory: None,
        status: "ready".to_string(),
        chunk_count: 0,
        file_type: Some("application/pdf".to_string()),
        file_size: Some(12),
        mime_type: Some("application/pdf".to_string()),
        s3_key: Some(format!("documents/tenant-a/{id}/{id}.pdf")),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        knowledge_base_ids: vec![],
        metadata: None,
    }
}

/// A multipart body with one file part plus text fields, built by hand so the
/// test does not depend on a client crate's boundary escaping.
pub fn multipart_body(parts: &[(&str, String)]) -> Vec<u8> {
    let boundary = "kbTestBoundary";
    let mut body = Vec::new();
    for (name, value) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        if *name == "file" {
            let file_name = value.split('\u{1}').next().unwrap_or("upload.txt");
            let rest = value.split('\u{1}').nth(1).unwrap_or("");
            body.extend_from_slice(
                format!(
                    "content-disposition: form-data; name=\"{name}\"; filename=\"{file_name}\"\r\ncontent-type: text/plain\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(rest.as_bytes());
        } else {
            body.extend_from_slice(
                format!("content-disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(value.as_bytes());
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

fn multipart_content_type(_parts: &[(&str, String)]) -> String {
    "multipart/form-data; boundary=kbTestBoundary".to_string()
}

/// Read a JSON response body.
pub async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// The `error` string of an error response.
pub async fn error_text(response: axum::response::Response) -> String {
    json_body(response)
        .await
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Assert a status and an exact error text in one step.
pub async fn assert_error(response: axum::response::Response, status: StatusCode, text: &str) {
    assert_eq!(response.status(), status, "status mismatch");
    assert_eq!(error_text(response).await, text, "error text mismatch");
}
