//! `API-4`: the thin Streamable-HTTP MCP endpoint behind `KB_MCP=on`.
//!
//! Every request here drives the real router over the in-process fakes. The
//! tests check both the JSON-RPC framing and — more importantly — that each
//! tool goes through the very same visibility check as its `/v1` route: a
//! foreign document is an `isError` tool result with no content, never a
//! successful answer with another tenant's text in it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kb_engine::fakes::make_fake_hit;
use kb_ports::{
    ChunkId, ChunkType, DocumentId, IndexUnit, KnowledgeBaseId, RetrievalIndex, TenantId,
};
use tower::ServiceExt;

use harness::{Harness, doc_meta, json_body, tenant_key};

const KEYWORD: &str = "zxqvortex";
const SECRET: &str = "SECRET-OTHER-TENANT-DO-NOT-LEAK";

// ─── JSON-RPC helpers ───────────────────────────────────────────────────────

async fn rpc(h: &Harness, key: &str, request: serde_json::Value) -> serde_json::Value {
    let response = h.send_json("POST", "/mcp", key, request).await;
    assert_eq!(response.status(), StatusCode::OK, "MCP POST was refused");
    json_body(response).await
}

fn call(name: &str, arguments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    })
}

fn structured(body: &serde_json::Value) -> &serde_json::Value {
    assert!(body["error"].is_null(), "unexpected JSON-RPC error: {body}");
    assert_ne!(body["result"]["isError"], true, "tool failed: {body}");
    &body["result"]["structuredContent"]
}

fn is_error(body: &serde_json::Value) -> String {
    assert_eq!(
        body["result"]["isError"], true,
        "expected a tool error: {body}"
    );
    body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ─── fixtures ───────────────────────────────────────────────────────────────

fn seed_doc(h: &Harness, tenant: &str, id: &str, title: &str, kb: Option<&str>) {
    let mut meta = doc_meta(id, title);
    meta.sha256 = Some(format!("{id}-hash"));
    if let Some(kb) = kb {
        meta.knowledge_base_ids = vec![kb.to_string()];
    }
    h.documents.insert_doc(TenantId(tenant.to_string()), meta);
}

fn seed_vector(h: &Harness, id: &str, content: &str) {
    h.chunks.vector_hits.write().unwrap().push(make_fake_hit(
        &format!("{id}-dense"),
        id,
        id,
        content,
        0,
        0.9,
    ));
}

async fn seed_units(h: &Harness, tenant: &str, doc: &str, rows: &[(u32, u32, &str)]) {
    let units: Vec<IndexUnit> = rows
        .iter()
        .map(|(index, page, content)| {
            let i = *index;
            IndexUnit {
                id: ChunkId(format!("{doc}_{i}")),
                document_id: DocumentId(doc.to_string()),
                tenant: TenantId(tenant.to_string()),
                knowledge_base_id: None,
                category: None,
                chunk_index: i,
                page: Some(*page),
                chunk_type: ChunkType::Text,
                content: (*content).to_string(),
                contextual_prefix: None,
                metadata: serde_json::json!({
                    "chunkIndex": i,
                    "chunkType": "text",
                    "page": *page,
                    "charStart": i * 100,
                    "charEnd": (i + 1) * 100,
                    "overlapPrefixLen": 0,
                }),
                dense: vec![0.1; 4],
                sparse: kb_core::sparse::SparseVector::default(),
            }
        })
        .collect();
    h.chunks.upsert(units).await.unwrap();
}

// ─── protocol ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn initialize_answers_server_info_and_tools_capability() {
    let h = Harness::with_mcp();
    h.with_tenant("tenant-a");
    let body = rpc(
        &h,
        &tenant_key("tenant-a"),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-03-26"},
        }),
    )
    .await;
    assert_eq!(body["result"]["protocolVersion"], "2025-03-26", "{body}");
    assert_eq!(body["result"]["serverInfo"]["name"], "rantai-kb", "{body}");
    assert_eq!(body["result"]["serverInfo"]["version"], "test", "{body}");
    assert!(
        body["result"]["capabilities"]["tools"].is_object(),
        "{body}"
    );

    // An unsupported version falls back to the one this server speaks.
    let body = rpc(
        &h,
        &tenant_key("tenant-a"),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "initialize",
            "params": {"protocolVersion": "1999-01-01"},
        }),
    )
    .await;
    assert_eq!(body["result"]["protocolVersion"], "2025-06-18", "{body}");
}

#[tokio::test]
async fn tools_list_advertises_five_tools_with_input_schemas() {
    let h = Harness::with_mcp();
    h.with_tenant("tenant-a");
    let body = rpc(
        &h,
        &tenant_key("tenant-a"),
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
    )
    .await;
    // Raw response, captured with `--nocapture` for the implementation report.
    println!("RAW_TOOLS_LIST_JSON={body}");
    let tools = body["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 5, "{body}");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        vec![
            "kb_search",
            "kb_list_documents",
            "kb_outline",
            "kb_read",
            "kb_query_table",
        ],
        "{body}"
    );
    for tool in tools {
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "{tool}"
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["inputSchema"]["properties"].is_object(), "{tool}");
    }
}

#[tokio::test]
async fn ping_is_answered() {
    let h = Harness::with_mcp();
    h.with_tenant("tenant-a");
    let body = rpc(
        &h,
        &tenant_key("tenant-a"),
        serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}),
    )
    .await;
    assert_eq!(body["result"], serde_json::json!({}), "{body}");
}

// ─── tools ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn kb_search_finds_a_knowledge_base_keyword() {
    let h = Harness::with_mcp();
    h.with_tenant("tenant-a");
    seed_doc(&h, "tenant-a", "doc-a", "Leave handbook", None);
    seed_vector(
        &h,
        "doc-a",
        &format!("the rare term {KEYWORD} appears in this handbook"),
    );

    let body = rpc(
        &h,
        &tenant_key("tenant-a"),
        call("kb_search", serde_json::json!({"query": KEYWORD})),
    )
    .await;
    let result = structured(&body);
    let text = result.to_string();
    assert!(
        text.contains(KEYWORD),
        "the keyword was not found: {result}"
    );
    assert!(
        text.contains("doc-a"),
        "no documentId in the answer: {result}"
    );
    assert_eq!(body["result"]["content"][0]["type"], "text", "{body}");
    // The same JSON is echoed as text and as structured content.
    let rendered = body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(rendered).unwrap(),
        *result
    );
}

#[tokio::test]
async fn kb_outline_then_kb_read_round_trips_a_section() {
    let h = Harness::with_mcp();
    h.with_tenant("tenant-a");
    seed_doc(&h, "tenant-a", "doc-a", "Peraturan", None);
    seed_units(
        &h,
        "tenant-a",
        "doc-a",
        &[
            (
                0,
                0,
                "BAB I KETENTUAN UMUM\nPasal 1\nBerhak atas pendidikan.",
            ),
            (1, 1, "BAB II HAK\nPasal 2\nBerhak atas pekerjaan."),
        ],
    )
    .await;

    let outline = rpc(
        &h,
        &tenant_key("tenant-a"),
        call("kb_outline", serde_json::json!({"documentId": "doc-a"})),
    )
    .await;
    let sections = structured(&outline)["sections"]
        .as_array()
        .expect("sections array");
    assert!(!sections.is_empty(), "{outline}");
    let section = sections[0]["id"].as_str().expect("section id");

    let read = rpc(
        &h,
        &tenant_key("tenant-a"),
        call(
            "kb_read",
            serde_json::json!({"documentId": "doc-a", "section": section, "maxChars": 8000}),
        ),
    )
    .await;
    let text = structured(&read)["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("Berhak atas pendidikan."),
        "read missed the section: {read}"
    );
    assert!(
        !text.contains("Berhak atas pekerjaan."),
        "read spilled the next section: {read}"
    );
}

#[tokio::test]
async fn another_tenants_document_is_a_tool_error_without_content() {
    let h = Harness::with_mcp();
    h.with_tenant("tenant-a");
    h.with_tenant("tenant-b");
    seed_doc(&h, "tenant-b", "theirs", "Foreign", None);
    seed_units(&h, "tenant-b", "theirs", &[(0, 0, SECRET)]).await;

    let body = rpc(
        &h,
        &tenant_key("tenant-a"),
        call("kb_read", serde_json::json!({"documentId": "theirs"})),
    )
    .await;
    let message = is_error(&body);
    assert_eq!(message, "Not found", "{body}");
    assert!(
        !body.to_string().contains(SECRET),
        "foreign content leaked: {body}"
    );
}

#[tokio::test]
async fn a_kb_limited_key_cannot_read_a_document_outside_its_kb() {
    let h = Harness::with_mcp();
    h.documents.insert_api_key_with_scopes(
        &tenant_key("tenant-a"),
        TenantId("tenant-a".to_string()),
        vec!["kb:read".to_string()],
        vec![KnowledgeBaseId("kb-a".to_string())],
    );
    seed_doc(&h, "tenant-a", "doc-out", "Outside", Some("kb-b"));
    seed_units(&h, "tenant-a", "doc-out", &[(0, 0, SECRET)]).await;

    let body = rpc(
        &h,
        &tenant_key("tenant-a"),
        call("kb_read", serde_json::json!({"documentId": "doc-out"})),
    )
    .await;
    let message = is_error(&body);
    assert_eq!(message, "Not found", "{body}");
    assert!(
        !body.to_string().contains(SECRET),
        "out-of-KB content leaked: {body}"
    );
}

// ─── auth and the flag ──────────────────────────────────────────────────────

#[tokio::test]
async fn a_request_without_a_key_gets_401() {
    let h = Harness::with_mcp();
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn with_kb_mcp_off_the_path_is_an_ordinary_404() {
    let h = Harness::new(); // MCP not mounted
    h.with_tenant("tenant-a");
    let response = h
        .send_json(
            "POST",
            "/mcp",
            &tenant_key("tenant-a"),
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
