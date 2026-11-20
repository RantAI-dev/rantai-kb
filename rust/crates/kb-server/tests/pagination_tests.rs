//! `GET /v1/documents` paging: the cursor, the total, and the limit the
//! route actually applied.
//!
//! The cursor is a new way into the list, so the tenant scope is tested
//! through it: a cursor is a position, never a grant.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_ports::{DocumentMeta, TenantId};

use harness::*;

const TENANT: &str = "tenant-a";
const OTHER: &str = "tenant-b";

/// A document row with a chosen creation time, so a list has a deterministic
/// order.
fn doc_at(id: &str, created_at: &str) -> DocumentMeta {
    let mut doc = doc_meta(id, id);
    doc.created_at = created_at.to_string();
    doc
}

/// The ids of a list page, in order.
fn ids_of(body: &serde_json::Value) -> Vec<String> {
    body["documents"]
        .as_array()
        .expect("documents")
        .iter()
        .map(|d| d["id"].as_str().expect("id").to_string())
        .collect()
}

#[tokio::test]
async fn list_reports_the_total_and_a_cursor_when_another_page_follows() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    for (id, at) in [
        ("doc-1", "2026-01-03T00:00:00.000Z"),
        ("doc-2", "2026-01-02T00:00:00.000Z"),
        ("doc-3", "2026-01-01T00:00:00.000Z"),
    ] {
        h.documents
            .insert_doc(TenantId(TENANT.to_string()), doc_at(id, at));
    }

    let response = h
        .send("GET", "/v1/documents?limit=2", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    assert_eq!(ids_of(&body), vec!["doc-1", "doc-2"]);
    assert_eq!(body["total"], 3, "the whole filtered set, not the page");
    assert_eq!(body["limit"], 2);
    assert_eq!(body["limitClamped"], false);

    let cursor = body["nextCursor"].as_str().expect("a cursor").to_string();
    assert!(!cursor.is_empty());
    // The cursor is opaque: it does not read as a document id or a timestamp.
    assert!(!cursor.contains("doc-2"), "{cursor}");
    assert!(!cursor.contains("2026-01-02"), "{cursor}");
}

#[tokio::test]
async fn the_cursor_continues_the_walk_where_the_last_page_stopped() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    for (id, at) in [
        ("doc-1", "2026-01-03T00:00:00.000Z"),
        ("doc-2", "2026-01-02T00:00:00.000Z"),
        ("doc-3", "2026-01-01T00:00:00.000Z"),
    ] {
        h.documents
            .insert_doc(TenantId(TENANT.to_string()), doc_at(id, at));
    }

    let body = json_body(
        h.send("GET", "/v1/documents?limit=2", &tenant_key(TENANT))
            .await,
    )
    .await;
    let cursor = body["nextCursor"].as_str().expect("a cursor").to_string();

    let response = h
        .send(
            "GET",
            &format!("/v1/documents?limit=2&cursor={cursor}"),
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    assert_eq!(ids_of(&body), vec!["doc-3"]);
    assert_eq!(body["total"], 3, "the total is the whole set on every page");
    assert_eq!(
        body["nextCursor"],
        serde_json::Value::Null,
        "the last page has no cursor"
    );
}

#[tokio::test]
async fn paging_covers_every_document_exactly_once() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    for i in 0..7 {
        let id = format!("doc-{i}");
        let at = format!("2026-01-0{}T00:00:00.000Z", 7 - i);
        h.documents
            .insert_doc(TenantId(TENANT.to_string()), doc_at(&id, &at));
    }

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            Some(c) => format!("/v1/documents?limit=3&cursor={c}"),
            None => "/v1/documents?limit=3".to_string(),
        };
        let response = h.send("GET", &uri, &tenant_key(TENANT)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        seen.extend(ids_of(&body));
        assert_eq!(body["total"], 7, "the same total on every page");
        match body["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    seen.sort();
    let mut expected: Vec<String> = (0..7).map(|i| format!("doc-{i}")).collect();
    expected.sort();
    assert_eq!(seen, expected, "no document is listed twice or skipped");
}

#[tokio::test]
async fn documents_created_in_the_same_millisecond_keep_a_stable_order() {
    // Two rows, one creation time: the id breaks the tie, so the walk cannot
    // list one of them twice or skip it.
    let h = Harness::new();
    h.with_tenant(TENANT);
    for id in ["doc-a", "doc-b"] {
        h.documents.insert_doc(
            TenantId(TENANT.to_string()),
            doc_at(id, "2026-01-01T00:00:00.000Z"),
        );
    }

    let body = json_body(
        h.send("GET", "/v1/documents?limit=1", &tenant_key(TENANT))
            .await,
    )
    .await;
    let first = ids_of(&body);
    let cursor = body["nextCursor"].as_str().expect("a cursor").to_string();

    let body = json_body(
        h.send(
            "GET",
            &format!("/v1/documents?limit=1&cursor={cursor}"),
            &tenant_key(TENANT),
        )
        .await,
    )
    .await;
    let second = ids_of(&body);

    let mut all = [first, second].concat();
    all.sort();
    assert_eq!(all, vec!["doc-a", "doc-b"], "both are listed, each once");
}

#[tokio::test]
async fn a_limit_above_the_cap_is_clamped_and_said_so() {
    // Not a 400: the callers that ask for 500 documents keep working. But they
    // are told, because a caller that asked for 500 and got 200 has to know
    // which it got.
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send("GET", "/v1/documents?limit=500", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["limit"], 200, "the cap");
    assert_eq!(body["limitClamped"], true);
}

#[tokio::test]
async fn a_limit_inside_the_cap_is_applied_as_it_is() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send("GET", "/v1/documents?limit=7", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["limit"], 7);
    assert_eq!(body["limitClamped"], false);
}

#[tokio::test]
async fn an_unreadable_limit_is_the_default_and_is_not_a_clamp() {
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send("GET", "/v1/documents?limit=lots", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["limit"], 100, "the default page size");
    assert_eq!(
        body["limitClamped"], false,
        "an ignored limit is not a clamped one"
    );
}

#[tokio::test]
async fn a_cursor_this_server_did_not_issue_is_a_400() {
    // An empty page would be a valid answer that looks like the end of the
    // list, so rubbish is refused instead.
    let h = Harness::new();
    h.with_tenant(TENANT);

    let response = h
        .send(
            "GET",
            "/v1/documents?cursor=not-a-cursor",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("cursor"),
        "{body}"
    );
}

#[tokio::test]
async fn a_cursor_never_reaches_another_tenants_documents() {
    // The cursor is a position, not a grant. Another tenant's cursor — and a
    // cursor another tenant holds — must not widen what the caller sees.
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.with_tenant(OTHER);
    h.documents.insert_doc(
        TenantId(TENANT.to_string()),
        doc_at("doc-a", "2026-01-03T00:00:00.000Z"),
    );
    h.documents.insert_doc(
        TenantId(TENANT.to_string()),
        doc_at("doc-b", "2026-01-02T00:00:00.000Z"),
    );
    h.documents.insert_doc(
        TenantId(OTHER.to_string()),
        doc_at("doc-theirs", "2026-01-01T00:00:00.000Z"),
    );

    let body = json_body(
        h.send("GET", "/v1/documents?limit=1", &tenant_key(TENANT))
            .await,
    )
    .await;
    let cursor = body["nextCursor"].as_str().expect("a cursor").to_string();

    // The other tenant sends the first tenant's cursor back.
    let response = h
        .send(
            "GET",
            &format!("/v1/documents?cursor={cursor}"),
            &tenant_key(OTHER),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        ids_of(&body),
        vec!["doc-theirs"],
        "only the caller's own documents, never the cursor owner's"
    );
    assert_eq!(body["total"], 1, "the count is the caller's own set");
}
