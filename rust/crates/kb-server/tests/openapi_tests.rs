//! The OpenAPI document must describe the API that actually runs.
//!
//! Four things are checked here, and each of them is a way the published
//! contract could lie:
//!
//! 1. every route the router serves is in the document, and every operation in
//!    the document is a route the router serves (the route table and the
//!    document are checked against each other, in both directions);
//! 2. `/openapi.json` and `/docs` answer without a key — the contract has to
//!    be readable before the caller has one;
//! 3. a `/v1` route still answers 401 without a key — publishing the document
//!    must not have opened the API;
//! 4. `docs/openapi.json` is the document the binary prints, byte for byte.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use kb_server::openapi::{ALL_ROUTES, ApiDoc};
use tower::ServiceExt;
use utoipa::OpenApi;

// ─── The document and the router agree ───────────────────────────────────────

#[test]
fn every_route_of_the_table_is_in_the_document() {
    let document = ApiDoc::openapi();
    for route in ALL_ROUTES {
        let path = openapi_path(route.path);
        let item = document
            .paths
            .get_path_item(&path)
            .unwrap_or_else(|| panic!("{path} is missing from the document"));
        let operation = operation(item, route.method.clone())
            .unwrap_or_else(|| panic!("{path} {} is missing from the document", route.method));
        let tags = operation.tags.as_ref().expect("the operation has a tag");
        assert!(
            tags.contains(&route.tag.to_string()),
            "{path} is filed under {tags:?}, not under {}",
            route.tag
        );
    }
}

#[test]
fn every_operation_of_the_document_is_a_route_of_the_table() {
    let document = ApiDoc::openapi();
    let mut declared = 0usize;
    for (path, item) in document.paths.paths.iter() {
        for (method, operation) in operations(item) {
            declared += 1;
            let route = ALL_ROUTES
                .iter()
                .find(|route| openapi_path(route.path) == *path && route.method == method);
            let route = route.unwrap_or_else(|| {
                panic!("the document has {path} {method}, which the router does not serve")
            });
            let tags = operation.tags.as_ref().expect("the operation has a tag");
            assert_eq!(
                tags.first().map(String::as_str),
                Some(route.tag),
                "{path} {method} is filed under {tags:?}, not under {}",
                route.tag
            );
        }
    }
    assert_eq!(
        declared,
        ALL_ROUTES.len(),
        "the document has {declared} operations, the router serves {}",
        ALL_ROUTES.len()
    );
}

#[test]
fn the_document_names_itself_and_its_version() {
    let document = ApiDoc::openapi();
    assert_eq!(document.info.title, "rantai-kb API");
    assert_eq!(document.info.version, env!("CARGO_PKG_VERSION"));
    let version = serde_json::to_value(&document.openapi).unwrap();
    assert_eq!(version, serde_json::json!("3.1.0"));
}

#[test]
fn the_document_declares_the_two_key_schemes_the_auth_accepts() {
    let document = ApiDoc::openapi();
    let schemes = document
        .components
        .expect("the document has components")
        .security_schemes;
    let bearer = schemes
        .get("bearer_auth")
        .expect("the bearer scheme is declared");
    assert!(
        matches!(bearer, utoipa::openapi::security::SecurityScheme::Http(_)),
        "the bearer scheme is an http scheme: {}",
        serde_json::to_string(bearer).unwrap()
    );
    let api_key = schemes
        .get("api_key_header")
        .expect("the api-key scheme is declared");
    assert!(
        matches!(
            api_key,
            utoipa::openapi::security::SecurityScheme::ApiKey(_)
        ),
        "the api-key scheme is an api-key scheme: {}",
        serde_json::to_string(api_key).unwrap()
    );
    // The document asks for a key by default; the public routes say otherwise.
    // A key is sent one way or the other, never both, so the document offers
    // the choice: two requirement objects, not one object with two schemes.
    let requirements = document
        .security
        .expect("the document has a global security");
    let requirements = serde_json::to_value(&requirements).unwrap();
    assert_eq!(
        requirements,
        serde_json::json!([{ "bearer_auth": [] }, { "api_key_header": [] }])
    );
}

#[test]
fn the_public_routes_do_not_ask_for_a_key() {
    let document = ApiDoc::openapi();
    for path in ["/health", "/v1/health", "/v1/formats"] {
        let item = document.paths.get_path_item(path).expect("path");
        let operation = item.get.as_ref().expect("get");
        // `SecurityRequirement` has no `Debug`, so the comparison goes through
        // the JSON a reader of the document will see. One empty requirement
        // object is how OpenAPI says "this operation asks for no key": it
        // cancels the global requirement instead of naming a scheme.
        let security = serde_json::to_value(&operation.security).unwrap();
        assert_eq!(
            security,
            serde_json::json!([{}]),
            "{path} must ask for no key"
        );
    }
}

#[test]
fn every_tag_is_described() {
    let document = ApiDoc::openapi();
    let tags = document.tags.expect("the document names its tags");
    let used = [
        "documents",
        "search",
        "jobs",
        "tables",
        "taxonomy",
        "structure",
        "media",
        "events",
        "health",
    ];
    for name in used {
        let tag = tags
            .iter()
            .find(|tag| tag.name == name)
            .unwrap_or_else(|| panic!("{name} is not a tag of the document"));
        assert!(
            tag.description
                .as_ref()
                .is_some_and(|d| !d.trim().is_empty()),
            "{name} has no description"
        );
    }
}

// ─── The document is served, and it is public ────────────────────────────────

#[tokio::test]
async fn the_document_is_served_without_a_key() {
    let harness = harness::Harness::new();
    let response = request(&harness, Method::GET, "/openapi.json").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = harness::json_body(response).await;
    assert_eq!(body["openapi"], "3.1.0");
    assert_eq!(body["info"]["title"], "rantai-kb API");
    assert!(
        body["paths"]
            .as_object()
            .is_some_and(|paths| !paths.is_empty()),
        "the document has no paths"
    );
}

#[tokio::test]
async fn the_document_the_server_serves_is_the_document_the_code_builds() {
    let harness = harness::Harness::new();
    let response = request(&harness, Method::GET, "/openapi.json").await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let served: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let built: serde_json::Value =
        serde_json::from_str(&kb_server::openapi::spec_json().unwrap()).unwrap();
    assert_eq!(served, built);
}

#[tokio::test]
async fn the_ui_renders_the_served_document() {
    let harness = harness::Harness::new();
    let response = request(&harness, Method::GET, "/docs/").await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&bytes);
    assert!(
        html.contains("swagger"),
        "the UI page does not look like Swagger UI: {html}"
    );
    // The UI has to be pointed at the document this server serves, or it
    // renders an empty page. The URL lives in the initializer script.
    let response = request(&harness, Method::GET, "/docs/swagger-initializer.js").await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let initializer = String::from_utf8_lossy(&bytes);
    assert!(
        initializer.contains("/openapi.json"),
        "the UI does not read /openapi.json: {initializer}"
    );
}

#[tokio::test]
async fn the_ui_redirects_to_its_own_directory() {
    let harness = harness::Harness::new();
    let response = request(&harness, Method::GET, "/docs").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/docs/"),
        "the UI must send the browser to the directory it serves"
    );
}

#[tokio::test]
async fn the_ui_assets_are_embedded_and_not_fetched_from_a_cdn() {
    let harness = harness::Harness::new();
    let response = request(&harness, Method::GET, "/docs/").await;
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&bytes);
    assert!(
        !html.contains("https://unpkg.com") && !html.contains("cdn.jsdelivr.net"),
        "the UI must not reach a CDN at runtime: {html}"
    );
}

#[tokio::test]
async fn a_v1_route_still_needs_a_key() {
    let harness = harness::Harness::new();
    let response = request(&harness, Method::GET, "/v1/documents").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let text = harness::error_text(response).await;
    assert!(text.contains("Unauthorized"), "{text}");
}

// ─── The committed document is the generated one ─────────────────────────────

#[test]
fn the_committed_document_is_the_generated_one() {
    let generated = kb_server::openapi::spec_json().unwrap();
    let committed = std::fs::read_to_string(committed_path())
        .unwrap_or_else(|err| panic!("{}: {err}", committed_path().display()));
    assert_eq!(
        committed.trim_end(),
        generated.trim_end(),
        "{} is out of date — regenerate it with `cargo run -p kb-server -- openapi`",
        committed_path().display()
    );
}

/// The path of the committed document, relative to the crate.
fn committed_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../docs/openapi.json")
        .canonicalize()
        .unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/openapi.json")
        })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// A request with no credentials at all.
async fn request(
    harness: &harness::Harness,
    method: Method,
    uri: &str,
) -> axum::response::Response {
    harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .method(method)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// The OpenAPI spelling of an axum path: `:id` becomes `{id}`.
fn openapi_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(index) = rest.find(':') {
        out.push_str(&rest[..index]);
        out.push('{');
        rest = &rest[index + 1..];
        let end = rest.find('/').unwrap_or(rest.len());
        out.push_str(&rest[..end]);
        out.push('}');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The operation of a path item for one method.
fn operation(
    item: &utoipa::openapi::path::PathItem,
    method: Method,
) -> Option<&utoipa::openapi::path::Operation> {
    match method {
        Method::GET => item.get.as_ref(),
        Method::POST => item.post.as_ref(),
        Method::PATCH => item.patch.as_ref(),
        Method::DELETE => item.delete.as_ref(),
        Method::PUT => item.put.as_ref(),
        _ => None,
    }
}

/// Every operation of a path item, with its method.
fn operations(
    item: &utoipa::openapi::path::PathItem,
) -> Vec<(Method, &utoipa::openapi::path::Operation)> {
    let mut out = Vec::new();
    if let Some(get) = item.get.as_ref() {
        out.push((Method::GET, get));
    }
    if let Some(post) = item.post.as_ref() {
        out.push((Method::POST, post));
    }
    if let Some(put) = item.put.as_ref() {
        out.push((Method::PUT, put));
    }
    if let Some(patch) = item.patch.as_ref() {
        out.push((Method::PATCH, patch));
    }
    if let Some(delete) = item.delete.as_ref() {
        out.push((Method::DELETE, delete));
    }
    out
}
