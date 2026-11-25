//! SEC-3: per-document access lists, enforced default-deny.
//!
//! The fake document store mirrors the Postgres `scope_sql_inner` predicate, so
//! these tests drive the real router with real auth and assert both sides of
//! every rule: an ACL'd document is hidden without a matching asserted
//! principal and visible with one, while an un-ACL'd document is unaffected.
//! The dedicated arm test at the bottom probes every retrieval arm with a
//! canary that only the ACL'd document holds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kb_core::policy::UploadDedupe;
use kb_engine::fakes::{
    FakeBlobStore, FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeEntityStore, FakeJobStore,
    FakeReranker, FakeTableStore, make_fake_hit,
};
use kb_engine::{EngineSettings, PageArm, PageArmConfig, TreeArm, TreeArmConfig};
use kb_ports::{
    BlobStore, DocumentFilter, DocumentId, DocumentMeta, DocumentScope, DocumentStore,
    PageEmbedder, PageImage, PortError, RetrievalIndex, ScopeRequest, TenantId,
};
use kb_server::events::EventBus;
use kb_server::state::{AppState, StateParts};
use tower::ServiceExt;

use harness::*;

const TENANT: &str = "tenant-a";
const FULL_KEY: &str = "acl-full-key";
const ASSERT_KEY: &str = "acl-assert-key";
const PLAIN_KEY: &str = "acl-plain-key";
const A: &str = "doc-a";
const B: &str = "doc-b";

fn acl_doc(id: &str, acl: &[&str]) -> DocumentMeta {
    let mut meta = doc_meta(id, id);
    meta.acl_principals = acl.iter().map(|p| (*p).to_string()).collect();
    meta
}

/// The corpus: A is readable only by `group:hr`; B has no ACL at all.
async fn seed(h: &Harness) {
    h.documents.insert_api_key_with_scopes(
        FULL_KEY,
        TenantId(TENANT.to_string()),
        vec!["kb:read".into(), "kb:write".into()],
        vec![],
    );
    h.documents.insert_api_key_with_scopes(
        ASSERT_KEY,
        TenantId(TENANT.to_string()),
        vec!["kb:read".into(), "kb:assert-principals".into()],
        vec![],
    );
    h.documents.insert_api_key_with_scopes(
        PLAIN_KEY,
        TenantId(TENANT.to_string()),
        vec!["kb:read".into()],
        vec![],
    );
    let tenant = TenantId(TENANT.to_string());
    h.documents
        .insert_doc(tenant.clone(), acl_doc(A, &["group:hr"]));
    h.documents.insert_doc(tenant, acl_doc(B, &[]));

    // Both documents have a vector hit, so a search can name either one.
    h.chunks
        .vector_hits
        .write()
        .unwrap()
        .push(make_fake_hit("a-v", A, A, "alpha canary", 0, 0.9));
    h.chunks
        .vector_hits
        .write()
        .unwrap()
        .push(make_fake_hit("b-v", B, B, "beta canary", 0, 0.8));

    // The raw route needs a stored original; the other sub-resource routes only
    // need the document row, which the fixture above supplies.
    h.blobs
        .upload(
            &format!("documents/{TENANT}/{A}/{A}.pdf"),
            b"acl-secret".to_vec(),
            "application/pdf",
        )
        .await
        .unwrap();
}

fn returned_documents(body: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = body["chunks"]
        .as_array()
        .map(|chunks| {
            chunks
                .iter()
                .filter_map(|c| c["documentId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

async fn search(h: &Harness, key: &str, principals: &[&str]) -> (StatusCode, serde_json::Value) {
    let response = h
        .send_with_principals(
            "POST",
            "/v1/search",
            key,
            principals,
            serde_json::json!({"query": "canary", "maxChunks": 50}),
        )
        .await;
    let status = response.status();
    (status, json_body(response).await)
}

#[tokio::test]
async fn a_search_without_principals_sees_only_the_unacled_document() {
    let h = Harness::new();
    seed(&h).await;
    let (status, body) = search(&h, FULL_KEY, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(returned_documents(&body), vec![B]);
}

#[tokio::test]
async fn a_matching_principal_with_the_scope_sees_both_documents() {
    let h = Harness::new();
    seed(&h).await;
    let (status, body) = search(&h, ASSERT_KEY, &["group:hr"]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(returned_documents(&body), vec![A, B]);
}

#[tokio::test]
async fn a_non_matching_principal_sees_only_the_unacled_document() {
    let h = Harness::new();
    seed(&h).await;
    let (status, body) = search(&h, ASSERT_KEY, &["group:finance"]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(returned_documents(&body), vec![B]);
}

#[tokio::test]
async fn asserting_principals_without_the_scope_is_a_403() {
    let h = Harness::new();
    seed(&h).await;
    let (status, _) = search(&h, PLAIN_KEY, &["group:hr"]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The body field is the same assertion: it must not be a scope bypass.
    let response = h
        .send_json(
            "POST",
            "/v1/search",
            PLAIN_KEY,
            serde_json::json!({"query": "canary", "principals": ["group:hr"]}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // A malformed principal is a 400, never a silently emptied assertion.
    let response = h
        .send_with_principals(
            "POST",
            "/v1/search",
            ASSERT_KEY,
            &["bad principal!"],
            serde_json::json!({"query": "canary"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_document_list_hides_an_acled_document_without_the_principal() {
    let h = Harness::new();
    seed(&h).await;

    let body = json_body(h.send("GET", "/v1/documents", FULL_KEY).await).await;
    let ids: Vec<String> = body["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![B]);
    assert_eq!(body["total"], 1, "the count must not include A either");

    let response = h
        .send_with_principals(
            "GET",
            "/v1/documents",
            ASSERT_KEY,
            &["group:hr"],
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let ids: Vec<String> = body["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&A.to_string()), "{ids:?}");
    assert!(ids.contains(&B.to_string()), "{ids:?}");
    assert_eq!(body["total"], 2);
}

#[tokio::test]
async fn every_by_id_route_404s_without_the_principal_and_200s_with_it() {
    let h = Harness::new();
    seed(&h).await;
    for route in [
        format!("/v1/documents/{A}"),
        format!("/v1/documents/{A}/raw"),
        format!("/v1/documents/{A}/structure"),
        format!("/v1/documents/{A}/content"),
        format!("/v1/documents/{A}/assets?key=documents%2Ftenant-a%2Fdoc-a%2Fassets%2Ffig-1.png"),
        format!("/v1/documents/{A}/intelligence"),
    ] {
        let response = h.send("GET", &route, FULL_KEY).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{route} must not disclose an ACL'd document"
        );
        let response = h
            .send_with_principals(
                "GET",
                &route,
                ASSERT_KEY,
                &["group:hr"],
                serde_json::Value::Null,
            )
            .await;
        // The asset route needs a stored image; a missing one is a 404 for a
        // different reason. Every other route must answer 200 with the
        // principal, which is the positive control for the 404 above.
        if route.contains("/assets") {
            continue;
        }
        assert_eq!(response.status(), StatusCode::OK, "{route}");
    }
}

#[tokio::test]
async fn get_document_returns_the_access_list_additively() {
    let h = Harness::new();
    seed(&h).await;
    let body = json_body(h.send("GET", "/v1/documents/doc-b", FULL_KEY).await).await;
    assert_eq!(body["document"]["aclPrincipals"], serde_json::json!([]));
    let response = h
        .send_with_principals(
            "GET",
            "/v1/documents/doc-a",
            ASSERT_KEY,
            &["group:hr"],
            serde_json::Value::Null,
        )
        .await;
    let body = json_body(response).await;
    assert_eq!(
        body["document"]["aclPrincipals"],
        serde_json::json!(["group:hr"])
    );
}

// ─── Predicate mode ──────────────────────────────────────────────────────────

/// A JSON-RPC `tools/call` with the caller's principals in the header (SEC-3).
async fn mcp_call(
    h: &Harness,
    key: &str,
    principals: &[&str],
    tool: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json");
    if !principals.is_empty() {
        builder = builder.header("x-kb-principals", principals.join(","));
    }
    let response = h
        .app
        .clone()
        .oneshot(
            builder
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 7,
                        "method": "tools/call",
                        "params": {"name": tool, "arguments": arguments},
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "mcp {tool}: {body}");
    body
}

fn mcp_text(body: &serde_json::Value) -> String {
    body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn mcp_tools_honour_the_same_principals_and_scope() {
    let h = Harness::with_mcp();
    seed(&h).await;

    // Without the principal, an ACL'd document is absent from every tool.
    let body = mcp_call(
        &h,
        FULL_KEY,
        &[],
        "kb_search",
        serde_json::json!({"query": "canary"}),
    )
    .await;
    let text = mcp_text(&body);
    assert!(!text.contains(A), "MCP search leaked A: {text}");
    let body = mcp_call(
        &h,
        FULL_KEY,
        &[],
        "kb_list_documents",
        serde_json::json!({}),
    )
    .await;
    let text = mcp_text(&body);
    assert!(!text.contains(A), "MCP list leaked A: {text}");
    for (tool, arguments) in [
        ("kb_outline", serde_json::json!({"documentId": A})),
        ("kb_read", serde_json::json!({"documentId": A})),
    ] {
        let body = mcp_call(&h, FULL_KEY, &[], tool, arguments).await;
        assert_eq!(
            body["result"]["isError"], true,
            "{tool} must refuse an ACL'd document without the principal"
        );
    }

    // With the principal and the privileged key, the same tools work.
    let body = mcp_call(
        &h,
        ASSERT_KEY,
        &["group:hr"],
        "kb_search",
        serde_json::json!({"query": "canary"}),
    )
    .await;
    assert!(
        mcp_text(&body).contains(A),
        "MCP search must find A when entitled: {body}"
    );

    // The header assertion still needs the privileged scope on /mcp: the
    // auth extractor refuses the whole request before any tool runs.
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("authorization", format!("Bearer {PLAIN_KEY}"))
                .header("content-type", "application/json")
                .header("x-kb-principals", "group:hr")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 7,
                        "method": "tools/call",
                        "params": {"name": "kb_search", "arguments": {"query": "canary"}},
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = json_body(response).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("kb:assert-principals"),
        "the scope failure must be visible: {body}"
    );
}

async fn build_scope(h: &Harness) -> DocumentScope {
    h.documents
        .build_scope(&ScopeRequest {
            tenant: TenantId(TENANT.to_string()),
            filter: DocumentFilter::default(),
            force_ids: false,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_acled_tenant_forces_ids_mode_when_the_id_limit_is_crossed() {
    let mut h = Harness::new();
    seed(&h).await;
    h.documents.scope_id_limit = Some(1);
    // Two visible documents and an ACL'd tenant: predicate mode would have been
    // chosen without the ACL, but the payload has no ACL key.
    assert!(!build_scope(&h).await.is_predicate());

    let mut plain = Harness::new();
    plain.with_tenant(TENANT);
    let tenant = TenantId(TENANT.to_string());
    plain
        .documents
        .insert_doc(tenant.clone(), acl_doc("p-1", &[]));
    plain.documents.insert_doc(tenant, acl_doc("p-2", &[]));
    plain.documents.scope_id_limit = Some(1);
    assert!(
        build_scope(&plain).await.is_predicate(),
        "the control must prove the limit alone would switch modes"
    );
}

// ─── Every retrieval arm ─────────────────────────────────────────────────────

struct FourPageEmbedder;

#[async_trait::async_trait]
impl PageEmbedder for FourPageEmbedder {
    async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        Ok(vec![vec![0.1; 4]; pages.len()])
    }

    async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, PortError> {
        Ok(vec![0.1; 4])
    }

    fn dimension(&self) -> u32 {
        4
    }
}

/// A router with the page and tree arms on, in addition to the text arms the
/// default harness already runs. The canaries are unique to A, so a probe that
/// returns any of them is a leak.
async fn arm_harness() -> (axum::Router, FakeDocumentStore) {
    let documents = FakeDocumentStore::default();
    let chunks = FakeChunkStore::default();
    let page_index = FakeChunkStore::default();
    let blobs = FakeBlobStore::default();
    let entities = FakeEntityStore::default();
    let jobs = FakeJobStore::default();
    let embedder = FakeEmbedder::default();
    let tables = FakeTableStore::default();
    let events = EventBus::new();

    documents.insert_api_key_with_scopes(
        ASSERT_KEY,
        TenantId(TENANT.to_string()),
        vec!["kb:read".into(), "kb:assert-principals".into()],
        vec![],
    );
    let tenant = TenantId(TENANT.to_string());
    documents.insert_doc(tenant.clone(), acl_doc(A, &["group:hr"]));
    documents.insert_doc(tenant.clone(), acl_doc(B, &[]));
    documents.insert_api_key(FULL_KEY, tenant.clone());

    chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "a-vector",
        A,
        A,
        "ACLVECTOR canary",
        0,
        0.9,
    ));
    chunks
        .bm25_hits
        .write()
        .unwrap()
        .push(make_fake_hit("a-bm25", A, A, "ACLBM25 canary", 1, 0.9));
    chunks.neighbour_hits.write().unwrap().push(make_fake_hit(
        "a-neighbour",
        A,
        A,
        "ACLNEIGHBOUR canary",
        2,
        0.5,
    ));
    let mut figure = make_fake_hit("a-figure", A, A, "ACLFIGURE canary", 3, 0.5);
    figure.chunk.metadata.asset_key = Some(format!("documents/{TENANT}/{A}/assets/1.png"));
    figure.chunk.chunk_type = kb_ports::ChunkType::Figure;
    chunks.figure_hits.write().unwrap().push(figure);

    documents.insert_outline(
        DocumentId(A.to_string()),
        kb_core::tree::Outline {
            document_title: Some("ACLTREE canary".to_string()),
            method: kb_core::tree::OutlineMethod::Bookmarks,
            nodes: vec![kb_core::tree::OutlineNode {
                title: "ACLTREE canary".to_string(),
                level: 1,
                start_page: 0,
                end_page: 1,
            }],
        },
    );
    page_index
        .upsert(vec![kb_ports::IndexUnit {
            id: kb_ports::ChunkId("a-page".to_string()),
            document_id: DocumentId(A.to_string()),
            tenant: tenant.clone(),
            knowledge_base_id: None,
            category: None,
            chunk_index: 0,
            page: Some(0),
            chunk_type: kb_ports::ChunkType::Text,
            content: "ACLPAGE canary".to_string(),
            contextual_prefix: None,
            metadata: serde_json::json!({"page": 0}),
            dense: vec![0.1; 4],
            sparse: kb_core::sparse::SparseVector::default(),
        }])
        .await
        .unwrap();

    let settings = EngineSettings {
        candidate_pool_size: 20,
        rrf_k: 60,
        rerank_candidate_limit: 10,
        rerank_char_budget: 350,
        rerank_breadcrumbs: false,
        rerank_timeout_ms: 3000,
        rerank_final_k: 10,
        rerank_max_k: 10,
        max_chunks: 50,
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
    };
    let mut state = AppState::from_parts(
        StateParts {
            documents: Arc::new(documents.clone()),
            index: Arc::new(chunks),
            entities: Some(Arc::new(entities)),
            graph: None,
            graph_arm: None,
            blob: Arc::new(blobs),
            embedder: Arc::new(embedder),
            jobs: Arc::new(jobs),
            events: Arc::clone(&events),
            tables: Some(Arc::new(tables)),
            version: "acl-test".to_string(),
            upload_dedupe: UploadDedupe::default(),
        },
        Arc::new(FakeReranker::default()),
        settings,
    );
    state.engine.inputs.tree_arm = Some(Arc::new(TreeArm::new(TreeArmConfig::default())));
    state.engine.inputs.page_arm = Some(Arc::new(PageArm {
        embedder: Arc::new(FourPageEmbedder),
        index: Arc::new(page_index),
        vision: None,
        config: PageArmConfig::default(),
    }));
    (kb_server::build_router(state), documents)
}

#[tokio::test]
async fn no_arm_can_return_an_acled_documents_canary_without_the_principal() {
    let (app, _) = arm_harness().await;
    let probe = |key: &str, principals: &[&str]| {
        let app = app.clone();
        let key = key.to_string();
        let header = principals.join(",");
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/search")
                        .header("authorization", format!("Bearer {key}"))
                        .header("content-type", "application/json")
                        .header("x-kb-principals", header)
                        .body(Body::from(
                            serde_json::json!({"query": "canary", "maxChunks": 50}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await.to_string()
        }
    };

    let hidden = probe(FULL_KEY, &[]).await;
    for canary in [
        "ACLVECTOR",
        "ACLBM25",
        "ACLNEIGHBOUR",
        "ACLFIGURE",
        "ACLPAGE",
        "ACLTREE",
    ] {
        assert!(
            !hidden.contains(canary),
            "the {canary} arm leaked an ACL'd document: {hidden}"
        );
    }

    let visible = probe(ASSERT_KEY, &["group:hr"]).await;
    assert!(
        visible.contains("ACLVECTOR"),
        "positive control: the vector arm must find A when entitled: {visible}"
    );
}
