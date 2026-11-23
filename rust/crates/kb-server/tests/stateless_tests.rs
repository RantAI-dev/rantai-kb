//! End-to-end tests for stateless processor-only mode (`--stateless` / `KB_MODE=stateless`).
//!
//! Exercises:
//! 1. Booting `StatelessConfig` + `StatelessState::from_config` with zero
//!    `DATABASE_URL`, `QDRANT_*`, `S3_*`, or `OPENROUTER_API_KEY`.
//! 2. Static API key authentication (`KB_STATELESS_API_KEY`), constant-time
//!    verification, explicit `KB_STATELESS_ALLOW_UNAUTHENTICATED=true`, and
//!    public health / formats / OpenAPI / docs endpoints.
//! 3. Real `kb-extract` document processing through `POST /v1/process` for
//!    Markdown and CSV (including structured tables, table chunks, BM25 sparse
//!    vectors, and heuristic entity extraction).
//! 4. Dense + sparse encoding parity through `POST /v1/encode` and
//!    `POST /v1/process` with a wired embedder.
//! 5. Full bidirectional agreement between `STATELESS_ROUTES` and `StatelessApiDoc`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use kb_engine::{DocumentProcessor, ProcessorSettings};
use kb_ports::{Embedder, PortError};
use kb_server::stateless::{
    STATELESS_ROUTES, StatelessApiDoc, StatelessAuth, StatelessConfig, StatelessState,
    build_stateless_router,
};
use tower::ServiceExt;
use utoipa::OpenApi;

struct FakeEmbedder {
    dim: usize,
}

#[async_trait]
impl Embedder for FakeEmbedder {
    fn dimension(&self) -> u32 {
        self.dim as u32
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![text.len() as f32; self.dim])
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        Ok(texts
            .iter()
            .map(|t| vec![(t.len() as f32) + 0.5; self.dim])
            .collect())
    }
}

fn lookup_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    let map: HashMap<&str, &str> = pairs.iter().copied().collect();
    move |key: &str| map.get(key).copied().map(str::to_string)
}

fn build_multipart(
    boundary: &str,
    file: Option<(&str, &str, &[u8])>,
    fields: &[(&str, &str)],
) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    if let Some((filename, content_type, bytes)) = file {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
                 Content-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

async fn read_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|err| panic!("invalid json ({err}): {}", String::from_utf8_lossy(&bytes)))
}

fn real_stateless_app(auth_key: &str) -> axum::Router {
    let cfg = StatelessConfig::from_lookup(lookup_from(&[
        ("KB_STATELESS_API_KEY", auth_key),
        ("KB_VERSION", "test-stateless"),
    ]))
    .expect("stateless config should load with zero DB/Qdrant/S3 env vars");
    let state = StatelessState::from_config(cfg).expect("stateless state");
    build_stateless_router(state)
}

fn app_with_embedder(auth: StatelessAuth) -> axum::Router {
    let cfg = StatelessConfig::from_lookup(lookup_from(&[(
        "KB_STATELESS_ALLOW_UNAUTHENTICATED",
        "true",
    )]))
    .expect("config");
    let extractor = kb_extract::build_production_extractor(&cfg.extract)
        .expect("production extractor")
        .shared();
    let settings = ProcessorSettings {
        embedding_model: Some("fake/embed-4d".to_string()),
        ..ProcessorSettings::default()
    };
    let processor = DocumentProcessor::new(extractor, settings)
        .with_embedder(Arc::new(FakeEmbedder { dim: 4 }));
    let state = StatelessState::from_parts(processor, auth, "test-embed".to_string());
    build_stateless_router(state)
}

// ─── Config & Boot Tests ─────────────────────────────────────────────────────

#[test]
fn stateless_config_boots_without_database_qdrant_s3_or_openrouter() {
    let cfg = StatelessConfig::from_lookup(lookup_from(&[(
        "KB_STATELESS_ALLOW_UNAUTHENTICATED",
        "true",
    )]))
    .expect("stateless config must succeed with zero storage or model env vars");

    assert_eq!(cfg.auth, StatelessAuth::Unauthenticated);
    assert!(cfg.embedder.is_none());
    assert!(cfg.egress_hosts().is_empty());
    assert_eq!(cfg.bind_addr, "0.0.0.0:8080");
}

#[test]
fn stateless_config_refuses_boot_without_auth_or_explicit_opt_out() {
    let err = StatelessConfig::from_lookup(lookup_from(&[])).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("KB_STATELESS_API_KEY"), "{msg}");
    assert!(msg.contains("KB_STATELESS_ALLOW_UNAUTHENTICATED"), "{msg}");
}

#[test]
fn stateless_config_supports_multiple_keys_and_optional_embedder() {
    let cfg = StatelessConfig::from_lookup(lookup_from(&[
        ("KB_STATELESS_API_KEY", "key-alpha, key-beta"),
        (
            "KB_EMBEDDING_BASE_URL",
            "http://127.0.0.1:8081/v1/embeddings",
        ),
        ("KB_EMBEDDING_MODEL", "bge-m3"),
        ("KB_EMBEDDING_DIM", "1024"),
    ]))
    .expect("config");

    assert!(cfg.auth.verify(Some("key-alpha")));
    assert!(cfg.auth.verify(Some("key-beta")));
    assert!(!cfg.auth.verify(Some("key-gamma")));
    assert!(!cfg.auth.verify(None));
    let emb = cfg.embedder.as_ref().expect("embedder configured");
    assert_eq!(emb.model, "bge-m3");
    assert_eq!(emb.dimension, 1024);
    assert!(cfg.egress_hosts().is_empty(), "loopback is local");

    // Explicitly disabling embeddings even when OPENROUTER_API_KEY is set for OCR.
    let no_embed = StatelessConfig::from_lookup(lookup_from(&[
        ("KB_STATELESS_ALLOW_UNAUTHENTICATED", "true"),
        ("OPENROUTER_API_KEY", "sk-or-test"),
        ("KB_STATELESS_EMBED", "false"),
    ]))
    .expect("config");
    assert!(no_embed.embedder.is_none());
}

// ─── Auth, Health, Formats, CORS, Docs ───────────────────────────────────────

#[tokio::test]
async fn public_endpoints_and_cors_answer_without_key() {
    let app = real_stateless_app("secret-123");

    for path in ["/health", "/v1/health"] {
        let res = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        assert_eq!(
            res.headers()
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("*")
        );
        let json = read_json(res).await;
        assert_eq!(json["ok"], true);
        assert_eq!(json["mode"], "stateless");
        assert_eq!(json["checks"]["extractor"], true);
        assert_eq!(json["capabilities"]["embed"], false);
        assert_eq!(json["capabilities"]["sparse"], true);
        assert_eq!(json["capabilities"]["tables"], true);
        assert_eq!(json["capabilities"]["figures"], true);
        assert_eq!(json["capabilities"]["entities"], true);
        assert_eq!(json["version"], "test-stateless");
    }

    let formats = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/formats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(formats.status(), StatusCode::OK);
    let formats_json = read_json(formats).await;
    assert!(formats_json["extensions"].as_array().unwrap().len() > 5);

    let openapi = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(openapi.status(), StatusCode::OK);
    let spec = read_json(openapi).await;
    assert_eq!(spec["info"]["title"], "rantai-kb Stateless Processor API");
    assert!(spec["paths"]["/v1/process"]["post"].is_object());
    assert!(spec["paths"]["/v1/encode"]["post"].is_object());

    let preflight = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/process")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn protected_and_unknown_routes_enforce_static_key() {
    let app = real_stateless_app("secret-123");

    // Missing key on /v1/encode -> 401
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/encode")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"texts":["hello"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Wrong key -> 401
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/encode")
                .header("authorization", "Bearer wrong-key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"texts":["hello"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Unknown path without key -> 401 (never leaks routes)
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Unknown path WITH valid X-API-Key -> 404
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/does-not-exist")
                .header("x-api-key", "secret-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Case-insensitive Bearer works on /v1/encode -> 200
    let res = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/encode")
                .header("authorization", "bearer secret-123")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"texts":["hello world"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

// ─── End-to-End Document Processing (Real kb-extract) ────────────────────────

#[tokio::test]
async fn process_markdown_end_to_end_without_embedder() {
    let app = real_stateless_app("secret-123");
    let md = b"# Annual Architecture Review\n\n\
               ## Overview\n\n\
               PT Nusantara Teknologi operates distributed Qdrant clusters across Jakarta and Singapore.\n\n\
               ## Storage\n\n\
               Each regional cluster stores dense and sparse vectors with tenant payload filtering.\n";
    let boundary = "----kbStatelessMdTest";
    let body = build_multipart(
        boundary,
        Some(("architecture.md", "text/markdown", md)),
        &[("category", "engineering"), ("entities", "true")],
    );

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/process")
                .header("x-api-key", "secret-123")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = read_json(res).await;

    assert_eq!(json["title"], "Annual Architecture Review");
    assert_eq!(json["fileSize"], md.len());
    assert_eq!(json["sha256"].as_str().unwrap().len(), 64);
    assert!(json.get("embeddingModel").is_none());

    let chunks = json["chunks"].as_array().unwrap();
    assert!(!chunks.is_empty());
    for (i, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk["chunkIndex"], i as u64);
        assert_eq!(chunk["category"], "engineering");
        assert!(chunk.get("embedding").is_none());
        let sparse = chunk["sparseVector"]
            .as_object()
            .expect("sparseVector present");
        assert!(!sparse["indices"].as_array().unwrap().is_empty());
        assert_eq!(
            sparse["indices"].as_array().unwrap().len(),
            sparse["values"].as_array().unwrap().len()
        );
    }
    assert!(json["timings"]["totalMs"].is_u64());
}

#[tokio::test]
async fn process_csv_extracts_structured_tables_and_table_chunks() {
    let app = real_stateless_app("secret-123");
    let csv = b"Region,Revenue,Units\nJakarta,125000,42\nSurabaya,98000,31\nBandung,74500,25\n";
    let boundary = "----kbStatelessCsvTest";
    let body = build_multipart(
        boundary,
        Some(("sales-q1.csv", "text/csv", csv)),
        &[("title", "Q1 Regional Sales")],
    );

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/process")
                .header("authorization", "Bearer secret-123")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = read_json(res).await;

    assert_eq!(json["title"], "Q1 Regional Sales");
    let tables = json["tables"].as_array().unwrap();
    assert_eq!(tables.len(), 1);
    assert_eq!(
        tables[0]["header"],
        serde_json::json!(["Region", "Revenue", "Units"])
    );
    assert_eq!(tables[0]["rows"].as_array().unwrap().len(), 3);
    assert!(tables[0]["manifest"].is_object());

    let chunks = json["chunks"].as_array().unwrap();
    let has_summary = chunks
        .iter()
        .any(|c| c["chunkType"] == "table_summary" && c["tableIndex"] == 0);
    let row_count = chunks
        .iter()
        .filter(|c| c["chunkType"] == "table_row" && c["tableIndex"] == 0)
        .count();
    assert!(has_summary, "expected table_summary chunk");
    assert_eq!(row_count, 3, "expected 3 table_row chunks");
}

#[tokio::test]
async fn process_validation_errors_return_accurate_statuses() {
    let app = real_stateless_app("secret-123");

    // 1. Unsupported extension -> 415
    let boundary = "----kbStatelessErr1";
    let body = build_multipart(
        boundary,
        Some(("malware.exe", "application/octet-stream", b"MZ")),
        &[],
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/process")
                .header("x-api-key", "secret-123")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // 2. embed=true without configured embedder -> 400
    let boundary = "----kbStatelessErr2";
    let body = build_multipart(
        boundary,
        Some(("note.md", "text/markdown", b"# Note\n\nSome content here.")),
        &[("embed", "true")],
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/process")
                .header("x-api-key", "secret-123")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let json = read_json(res).await;
    assert!(json["error"].as_str().unwrap().contains("embedder"));

    // 3. Invalid boolean option -> 400
    let boundary = "----kbStatelessErr3";
    let body = build_multipart(
        boundary,
        Some(("note.md", "text/markdown", b"# Note\n\nSome content here.")),
        &[("sparse", "maybe")],
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/process")
                .header("x-api-key", "secret-123")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 4. Missing file -> 400
    let boundary = "----kbStatelessErr4";
    let body = build_multipart(boundary, None, &[("title", "Orphan")]);
    let res = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/process")
                .header("x-api-key", "secret-123")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// ─── Embedder + /v1/encode Parity ────────────────────────────────────────────

#[tokio::test]
async fn process_and_encode_with_wired_embedder_match_core_sparse() {
    let app = app_with_embedder(StatelessAuth::Unauthenticated);

    // Process with default embed (true because embedder is wired)
    let boundary = "----kbStatelessEmb1";
    let body = build_multipart(
        boundary,
        Some((
            "policy.md",
            "text/markdown",
            b"# Security Policy\n\nRotate API keys every ninety days.",
        )),
        &[],
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/process")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let json = read_json(res).await;
    assert_eq!(json["embeddingModel"], "fake/embed-4d");
    let chunks = json["chunks"].as_array().unwrap();
    assert!(!chunks.is_empty());
    assert_eq!(chunks[0]["embedding"].as_array().unwrap().len(), 4);

    // Encode query: sparse matches kb_core::sparse::encode_query bit-for-bit
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/encode")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"texts":["rotate api keys"],"mode":"query","dense":true,"sparse":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let enc = read_json(res).await;
    assert_eq!(enc["mode"], "query");
    assert_eq!(enc["embeddingModel"], "fake/embed-4d");
    assert_eq!(enc["embeddings"][0].as_array().unwrap().len(), 4);

    let expected_sparse = kb_core::sparse::encode_query("rotate api keys");
    let got_indices: Vec<u32> = enc["sparseVectors"][0]["indices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    assert_eq!(got_indices, expected_sparse.indices);

    // Empty texts -> 400
    let res = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/encode")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"texts":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// ─── Stateless OpenAPI Route Agreement ───────────────────────────────────────

#[test]
fn stateless_openapi_matches_route_table_in_both_directions() {
    let document = StatelessApiDoc::openapi();
    assert_eq!(document.info.title, "rantai-kb Stateless Processor API");
    assert_eq!(document.info.version, env!("CARGO_PKG_VERSION"));

    let mut declared = 0usize;
    for (path, item) in &document.paths.paths {
        let methods = [
            (Method::GET, item.get.as_ref()),
            (Method::POST, item.post.as_ref()),
            (Method::PATCH, item.patch.as_ref()),
            (Method::DELETE, item.delete.as_ref()),
        ];
        for (method, op) in methods {
            let Some(op) = op else { continue };
            declared += 1;
            let route = STATELESS_ROUTES
                .iter()
                .find(|r| r.path == path && r.method == method)
                .unwrap_or_else(|| {
                    panic!("stateless OpenAPI has {method} {path}, which router does not serve")
                });
            let tags = op.tags.as_ref().expect("operation tags");
            assert!(
                tags.contains(&route.tag.to_string()),
                "{method} {path} tagged {tags:?}, expected {}",
                route.tag
            );
        }
    }
    assert_eq!(declared, STATELESS_ROUTES.len());
}
