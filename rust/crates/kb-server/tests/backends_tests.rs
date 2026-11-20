//! The health route reports the state of the model backends (`#36`).
//!
//! A client that uploads a scanned document needs to know whether the OCR
//! backend can serve it before it uploads it. The liveness answer does not
//! change — a missing OCR model does not make the service dead — but the body
//! says what each backend is doing.

mod harness;

use axum::http::StatusCode;
use harness::{Harness, json_body, serve_status, tenant_key};
use kb_server::backends::{BackendTargets, Endpoint};

const TENANT: &str = "t-acme";

#[tokio::test]
async fn health_reports_the_state_of_the_three_backends() {
    let url = serve_status(200).await;
    let h = Harness::with_backends(BackendTargets {
        embedder: Some(Endpoint::anonymous(url.clone())),
        reranker: Some(Endpoint::anonymous(url.clone())),
        ocr: Some(Endpoint::anonymous(url)),
    });

    let response = h.send("GET", "/v1/health", &tenant_key(TENANT)).await;
    // The backends do not decide whether the service is alive: the status
    // code is unchanged (`#36`).
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    for backend in ["embedder", "reranker", "ocr"] {
        assert_eq!(
            body["backends"][backend]["state"], "ready",
            "{backend}: {body}"
        );
        assert_eq!(
            body["backends"][backend]["detail"], "HTTP 200 OK",
            "{backend}: {body}"
        );
    }
}

#[tokio::test]
async fn health_says_when_a_backend_is_still_starting() {
    // A model that is still loading answers `503`.
    let loading = serve_status(503).await;
    let h = Harness::with_backends(BackendTargets {
        embedder: Some(Endpoint::anonymous(loading)),
        ..BackendTargets::default()
    });

    let response = h.send("GET", "/v1/health", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["backends"]["embedder"]["state"], "starting");
    assert_eq!(
        body["backends"]["embedder"]["detail"],
        "HTTP 503 Service Unavailable"
    );
}

#[tokio::test]
async fn health_says_when_a_backend_is_not_there() {
    // Nothing listens on port 1 of the loopback interface.
    let h = Harness::with_backends(BackendTargets {
        ocr: Some(Endpoint::anonymous("http://127.0.0.1:1/v1/models")),
        ..BackendTargets::default()
    });

    let response = h.send("GET", "/v1/health", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["backends"]["ocr"]["state"], "unavailable");
    assert!(
        body["backends"]["ocr"]["detail"]
            .as_str()
            .is_some_and(|d| !d.is_empty()),
        "the reason is told: {body}"
    );
}

#[tokio::test]
async fn health_says_when_a_backend_is_not_configured() {
    let h = Harness::new();

    let response = h.send("GET", "/v1/health", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    for backend in ["embedder", "reranker", "ocr"] {
        assert_eq!(
            body["backends"][backend]["state"], "unavailable",
            "{backend}: {body}"
        );
        assert_eq!(body["backends"][backend]["detail"], "not configured");
    }
}

#[tokio::test]
async fn the_health_path_without_a_version_says_the_same() {
    let url = serve_status(200).await;
    let h = Harness::with_backends(BackendTargets {
        ocr: Some(Endpoint::anonymous(url)),
        ..BackendTargets::default()
    });

    let response = h.send("GET", "/health", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["backends"]["ocr"]["state"], "ready");
    assert_eq!(body["backends"]["embedder"]["state"], "unavailable");
}
