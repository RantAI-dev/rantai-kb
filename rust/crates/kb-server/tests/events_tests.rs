//! `GET /v1/events` — the SSE stream, and CORS/preflight behaviour.
//!
//! The worker writes progress into the in-process bus; this test writes it
//! directly, which is the same code path (`ProgressSink::emit`) the worker uses.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use kb_ports::{ProgressSink, TenantId};
use tower::ServiceExt;

use harness::*;

const TENANT: &str = "tenant-a";

/// Read frames from a live response until `want` of them have arrived.
async fn read_frames(response: axum::response::Response, want: usize) -> Vec<String> {
    let mut stream = response.into_body().into_data_stream();
    let mut collected = Vec::new();
    while collected.len() < want {
        let chunk = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("frame within the timeout")
            .expect("stream item")
            .expect("data frame");
        collected.push(String::from_utf8_lossy(&chunk).to_string());
    }
    collected
}

#[tokio::test]
async fn events_stream_opens_with_a_connected_frame_and_the_bun_headers() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h.send("GET", "/v1/events", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    assert_eq!(
        response.headers().get("cache-control").unwrap(),
        "no-cache, no-transform"
    );
    assert_eq!(response.headers().get("connection").unwrap(), "keep-alive");
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "*"
    );

    let frames = read_frames(response, 1).await;
    assert_eq!(frames, vec![": connected\n\n".to_string()]);
}

#[tokio::test]
async fn events_stream_delivers_worker_progress_frames_for_this_tenant_only() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h.send("GET", "/v1/events", &tenant_key(TENANT)).await;

    let bus = Arc::clone(&h.events);
    // Another tenant's frame first: it must not reach this subscriber.
    let emitter = tokio::spawn(async move {
        let payload = serde_json::json!({"jobId": "other", "status": "processing"});
        ProgressSink::emit(
            &*bus,
            &TenantId("tenant-z".into()),
            "ingest:job:update",
            payload,
        )
        .await
        .unwrap();
        let payload = serde_json::json!({
            "jobId": "job-1",
            "documentId": "doc-1",
            "status": "processing",
            "step": "chunking",
            "progress": 30,
            "stepCurrent": null,
            "stepTotal": null,
            "etaSeconds": 5
        });
        ProgressSink::emit(
            &*bus,
            &TenantId(TENANT.into()),
            "ingest:job:update",
            payload,
        )
        .await
        .unwrap();
        let payload = serde_json::json!({"jobId": "job-1", "status": "ready", "step": "done", "progress": 100});
        ProgressSink::emit(
            &*bus,
            &TenantId(TENANT.into()),
            "ingest:job:update",
            payload,
        )
        .await
        .unwrap();
    });

    let frames = read_frames(response, 3).await;
    emitter.await.unwrap();

    assert_eq!(frames[0], ": connected\n\n");
    let mut first = frames[1].clone();
    assert!(
        first.starts_with("event: ingest:job:update\ndata: {"),
        "{first}"
    );
    first.clear();
    assert!(frames[2].starts_with("event: ingest:job:update\ndata: {"));
    assert!(frames[2].ends_with("}\n\n"));
    assert!(frames[1].contains(r#""jobId":"job-1""#));
    assert!(frames[1].contains(r#""step":"chunking""#));
    assert!(frames[2].contains(r#""status":"ready""#));
    assert!(
        !frames[1].contains("other"),
        "another tenant's frame leaked"
    );
}

#[tokio::test]
async fn events_stream_requires_a_key() {
    let h = Harness::new();
    let request = Request::builder()
        .uri("/v1/events")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let response = h.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn options_is_answered_before_authentication_with_the_cors_headers() {
    let h = Harness::new();
    let request = Request::builder()
        .uri("/v1/documents")
        .method("OPTIONS")
        .body(Body::empty())
        .unwrap();
    let response = h.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let headers = response.headers();
    assert_eq!(headers.get("access-control-allow-origin").unwrap(), "*");
    assert_eq!(
        headers.get("access-control-allow-methods").unwrap(),
        "GET,POST,PATCH,DELETE,OPTIONS"
    );
    assert_eq!(
        headers.get("access-control-allow-headers").unwrap(),
        "authorization,content-type,x-api-key"
    );
    assert_eq!(headers.get("access-control-max-age").unwrap(), "86400");
}

#[tokio::test]
async fn every_response_carries_the_allow_origin_header() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    for (method, uri) in [
        ("GET", "/health"),
        ("GET", "/v1/formats"),
        ("GET", "/v1/documents"),
        ("GET", "/v1/documents/missing"),
    ] {
        let response = h
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .method(method)
                    .header("authorization", format!("Bearer {}", tenant_key(TENANT)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "*",
            "missing CORS header on {method} {uri}"
        );
    }
}

#[tokio::test]
async fn unknown_paths_authenticate_first_and_then_404() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let anonymous = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/nope")
                .method("GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let response = h.send("GET", "/v1/nope", &tenant_key(TENANT)).await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn formats_reports_the_accepted_extensions_and_the_byte_limit() {
    let h = Harness::new();
    let response = h.send("GET", "/v1/formats", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["maxFileBytes"], 50 * 1024 * 1024);
    let extensions = body["extensions"].as_array().expect("extensions");
    assert_eq!(extensions[0], ".pdf");
    assert!(extensions.contains(&serde_json::json!(".gltf")));
    assert!(extensions.contains(&serde_json::json!(".kt")));
}

#[tokio::test]
async fn health_reports_both_checks_and_the_version() {
    let h = Harness::new();
    let response = h.send("GET", "/health", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["checks"]["postgres"], true);
    assert_eq!(body["checks"]["vectors"], true);
    assert_eq!(body["version"], "test");
}

#[tokio::test]
async fn formats_needs_no_key() {
    let h = Harness::new();
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/formats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["maxFileBytes"], 50 * 1024 * 1024);
}

#[tokio::test]
async fn health_needs_no_key() {
    let h = Harness::new();
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
