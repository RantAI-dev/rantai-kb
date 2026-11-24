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
use kb_core::policy::UploadDedupe;
use kb_engine::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeEntityStore, FakeJobStore,
    FakeReranker, FakeTableStore,
};
use kb_engine::{EngineSettings, PageArm, PageArmConfig};
use kb_ports::{DocumentMeta, PageEmbedder, PageImage, PortError};
use kb_server::backends::{BackendProbe, BackendTargets};
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
    /// The page-image arm's index, when [`Harness::with_page_index`] built one.
    pub page_index: Option<FakeChunkStore>,
    /// The table store (`TBL-1`) behind every harness. Empty unless a test
    /// seeds it, so an unseeded harness answers `/v1/tables` with `[]`.
    pub tables: FakeTableStore,
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    pub fn new() -> Self {
        Self::build(BackendTargets::default(), UploadDedupe::default(), false)
    }

    /// A harness with the thin MCP endpoint mounted (`KB_MCP=on`).
    pub fn with_mcp() -> Self {
        Self::build(BackendTargets::default(), UploadDedupe::default(), true)
    }

    /// A harness whose health route probes the given backends (`#36`).
    ///
    /// The router is built from the state, so the probe has to be in place
    /// before that.
    pub fn with_backends(targets: BackendTargets) -> Self {
        Self::build(targets, UploadDedupe::default(), false)
    }

    /// A harness whose upload route refuses a file the tenant already has
    /// (`KB_UPLOAD_DEDUPE=reject`, the default) or stores it again
    /// (`KB_UPLOAD_DEDUPE=off`).
    pub fn with_upload_dedupe(upload_dedupe: UploadDedupe) -> Self {
        Self::build(BackendTargets::default(), upload_dedupe, false)
    }

    fn build(targets: BackendTargets, upload_dedupe: UploadDedupe, mcp_enabled: bool) -> Self {
        let documents = FakeDocumentStore::default();
        let chunks = FakeChunkStore::default();
        let blobs = FakeBlobStore::default();
        let jobs = FakeJobStore::default();
        let entities = FakeEntityStore::default();
        let embedder = FakeEmbedder::default();
        let events = EventBus::new();
        let tables = FakeTableStore::default();

        let mut state = AppState::from_parts(
            StateParts {
                documents: Arc::new(documents.clone()),
                index: Arc::new(chunks.clone()),
                entities: Some(Arc::new(entities.clone())),
                graph: None,
                graph_arm: None,
                blob: Arc::new(blobs.clone()),
                embedder: Arc::new(embedder.clone()),
                jobs: Arc::new(jobs.clone()),
                events: Arc::clone(&events),
                tables: Some(Arc::new(tables.clone())),
                version: "test".to_string(),
                upload_dedupe,
            },
            Arc::new(FakeReranker::default()),
            engine_settings(),
        );
        // The backends the health route probes (`#36`).
        state.backends = BackendProbe::new(targets);
        state.mcp_enabled = mcp_enabled;

        Self {
            app: kb_server::build_router(state),
            documents,
            chunks,
            blobs,
            jobs,
            entities,
            embedder,
            events,
            page_index: None,
            tables,
        }
    }

    /// A harness whose engine has a page-image arm, for the reads that need
    /// page units (the structure route). `page_index` is the fake collection
    /// those units live in; seed it with `RetrievalIndex::upsert`.
    pub fn with_page_index() -> Self {
        let documents = FakeDocumentStore::default();
        let chunks = FakeChunkStore::default();
        let blobs = FakeBlobStore::default();
        let jobs = FakeJobStore::default();
        let entities = FakeEntityStore::default();
        let embedder = FakeEmbedder::default();
        let events = EventBus::new();
        let page_index = FakeChunkStore::default();
        let tables = FakeTableStore::default();

        let mut state = AppState::from_parts(
            StateParts {
                documents: Arc::new(documents.clone()),
                index: Arc::new(chunks.clone()),
                entities: Some(Arc::new(entities.clone())),
                graph: None,
                graph_arm: None,
                blob: Arc::new(blobs.clone()),
                embedder: Arc::new(embedder.clone()),
                jobs: Arc::new(jobs.clone()),
                events: Arc::clone(&events),
                tables: Some(Arc::new(tables.clone())),
                version: "test".to_string(),
                upload_dedupe: UploadDedupe::default(),
            },
            Arc::new(FakeReranker::default()),
            engine_settings(),
        );
        state.engine.inputs.page_arm = Some(Arc::new(PageArm {
            embedder: Arc::new(FixedPageEmbedder),
            index: Arc::new(page_index.clone()),
            config: PageArmConfig::default(),
            vision: None,
        }));

        Self {
            app: kb_server::build_router(state),
            documents,
            chunks,
            blobs,
            jobs,
            entities,
            embedder,
            events,
            page_index: Some(page_index),
            tables,
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

/// A four-dimensional page embedder: the structure route never embeds a page,
/// but `PageArm` requires one to exist.
struct FixedPageEmbedder;

#[async_trait::async_trait]
impl PageEmbedder for FixedPageEmbedder {
    async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        Ok(vec![vec![0.1, 0.2, 0.3, 0.4]; pages.len()])
    }

    async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }

    fn dimension(&self) -> u32 {
        4
    }
}

fn engine_settings() -> EngineSettings {
    EngineSettings {
        candidate_pool_size: 20,
        rrf_k: 60,
        rerank_candidate_limit: 10,
        rerank_char_budget: 350,
        rerank_breadcrumbs: false,
        rerank_timeout_ms: 3000,
        rerank_final_k: 10,
        rerank_max_k: 10,
        max_chunks: 10,
        max_per_document: 0,
        min_similarity: 0.30,
        min_score: None,
        neighbour_window: 1,
        figure_limit: 3,
        expansion_enabled: false,
        expansion_paraphrases: 3,
        hybrid_bm25_enabled: true,
        rerank_enabled: false,
        adaptive_arms: false,
    }
}

/// The key a tenant fixture authenticates with.
pub fn tenant_key(tenant: &str) -> String {
    format!("{tenant}-key")
}

/// A one-line HTTP server that answers every request with `status`.
///
/// The health probe only looks at the status, so the server does nothing else.
pub async fn serve_status(status: u16) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            // Read the request head, so the client is not still writing when
            // the answer goes out.
            while let Ok(read) = socket.read(&mut buf).await {
                if read == 0 || buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                503 => "Service Unavailable",
                _ => "Status",
            };
            let answer = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            let _ = socket.write_all(answer.as_bytes()).await;
            let _ = socket.flush().await;
        }
    });
    format!("http://{addr}/v1/models")
}

/// A document row the tests can seed.
pub fn doc_meta(id: &str, title: &str) -> DocumentMeta {
    DocumentMeta {
        sha256: None,
        doc_key: None,
        version: None,
        supersedes_document_id: None,
        effective_from: None,
        effective_to: None,
        is_latest: true,
        id: kb_ports::DocumentId(id.to_string()),
        title: title.to_string(),
        categories: vec![],
        subcategory: None,
        status: "ready".to_string(),
        external_ref: None,
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
