//! `/v1/tables` and the table fields of a search answer (`TBL-1`).
//!
//! The table routes are document-authorised: tenant plus the key's
//! knowledge-base limit. These tests drive the real router over fakes and pin
//! the two things that must never regress:
//!
//! * tenant B cannot list, get or query tenant A's table, and cannot see its
//!   table chunks in a search answer;
//! * a document's `tablePrivacy: "aggregate-only"` reaches the store as a
//!   `QueryPolicy` and a refused query answers 400 with the `QueryError` JSON.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_core::tables::TableSource;
use kb_core::tables::{ColumnInfo, ColumnKind, ColumnRole, Confidence, TableManifest};
use kb_engine::fakes::make_fake_hit;
use kb_ports::SearchHit;
use kb_ports::tables::{TableId, TableRecord};
use kb_ports::{Chunk, ChunkId, ChunkMetadata, ChunkType, DocumentId, TenantId};

use harness::{Harness, doc_meta, json_body, tenant_key};

fn manifest() -> TableManifest {
    TableManifest {
        columns: vec![
            ColumnInfo {
                name: "Region".to_string(),
                kind: ColumnKind::Text,
                role: ColumnRole::Identifier,
                unit: None,
                sample_values: vec!["Bali".to_string()],
                distinct_count: 2,
            },
            ColumnInfo {
                name: "Revenue".to_string(),
                kind: ColumnKind::Number,
                role: ColumnRole::Quantity,
                unit: Some("Rp juta".to_string()),
                sample_values: Vec::new(),
                distinct_count: 2,
            },
        ],
        derived_rows: vec![2],
        formulas_known: true,
        confidence: Confidence::High,
        n_rows: 3,
        n_cols: 2,
        truncated: false,
        notes: Vec::new(),
    }
}

fn record(tenant: &str, document: &str) -> TableRecord {
    let document_id = DocumentId::new_unchecked(document);
    TableRecord {
        id: TableId::of(&document_id, 0),
        tenant_id: TenantId::new_unchecked(tenant),
        document_id: document_id.clone(),
        index: 0,
        version: 1,
        source: TableSource::Xlsx,
        sheet: Some("Sheet1".to_string()),
        page_span: None,
        title: Some("Revenue by region".to_string()),
        caption: None,
        header: vec!["Region".to_string(), "Revenue".to_string()],
        manifest: manifest(),
        grid_key: format!("tables/{tenant}/{document}/{document}_t0/v1.parquet"),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
    }
}

fn seed_document(harness: &Harness, tenant: &str, id: &str, metadata: Option<serde_json::Value>) {
    let mut meta = doc_meta(id, "Revenue report");
    meta.metadata = metadata;
    harness
        .documents
        .insert_doc(TenantId::new_unchecked(tenant), meta);
}

fn table_hit() -> SearchHit {
    SearchHit {
        chunk: Chunk {
            id: ChunkId("doc-a_0".to_string()),
            document_id: DocumentId::new_unchecked("doc-a"),
            document_title: "Revenue report".to_string(),
            external_ref: None,
            content: "Table: Revenue by region\n- Region: text identifier".to_string(),
            chunk_type: ChunkType::TableSummary,
            metadata: ChunkMetadata {
                chunk_index: Some(0),
                table_id: Some("doc-a_t0".to_string()),
                ..ChunkMetadata::default()
            },
            categories: Vec::new(),
            subcategory: None,
        },
        similarity: 0.91,
        rank: 0,
        is_bm25: false,
        retrieval: None,
        dense_score: Some(0.91),
        rerank_score: None,
    }
}

#[tokio::test]
async fn a_tenant_lists_reads_and_queries_its_own_table() {
    let h = Harness::new();
    h.with_tenant("tenant-a");
    seed_document(&h, "tenant-a", "doc-a", None);
    h.tables.seed(record("tenant-a", "doc-a"));
    let key = tenant_key("tenant-a");

    let response = h.send("GET", "/v1/tables?documentId=doc-a", &key).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["id"], "doc-a_t0");
    assert_eq!(body[0]["manifest"]["confidence"], "high");
    assert_eq!(body[0]["manifest"]["derived_rows"][0], 2);

    let response = h.send("GET", "/v1/tables/doc-a_t0", &key).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["title"], "Revenue by region");
    assert_eq!(body["source"], "xlsx");

    let response = h
        .send_json(
            "POST",
            "/v1/tables/doc-a_t0/query",
            &key,
            serde_json::json!({"select": [{"agg": "count"}]}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["columns"][0], "count");
    assert_eq!(body["rows"][0][0], 3);
    assert_eq!(body["excluded_derived_rows"], 1);
    // The policy the route passed was the open one: no suppression.
    assert_eq!(body["suppressed_groups"], 0);
}

#[tokio::test]
async fn another_tenant_cannot_list_get_or_query_a_table() {
    let h = Harness::new();
    h.with_tenant("tenant-a");
    h.with_tenant("tenant-b");
    seed_document(&h, "tenant-a", "doc-a", None);
    h.tables.seed(record("tenant-a", "doc-a"));
    let other = tenant_key("tenant-b");

    // A document in another tenant is not even addressable: the list route
    // resolves it through the document route's rules and answers 404.
    let response = h.send("GET", "/v1/tables?documentId=doc-a", &other).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = h.send("GET", "/v1/tables/doc-a_t0", &other).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = h
        .send_json(
            "POST",
            "/v1/tables/doc-a_t0/query",
            &other,
            serde_json::json!({"select": [{"agg": "count"}]}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // The table route asked the store as tenant B, never as tenant A: the
    // store's own tenant filter is the second fence, and this proves the first
    // one (the route) is passing the caller's tenant through.
    let calls = h.tables.calls();
    assert!(
        calls
            .iter()
            .filter(|(op, _)| op == "get" || op == "query")
            .all(|(_, tenant)| tenant == "tenant-b"),
        "{calls:?}"
    );
}

#[tokio::test]
async fn an_aggregate_only_document_refuses_plain_rows_with_the_query_error() {
    let h = Harness::new();
    h.with_tenant("tenant-a");
    seed_document(
        &h,
        "tenant-a",
        "doc-a",
        Some(serde_json::json!({"tablePrivacy": "aggregate-only"})),
    );
    h.tables.seed(record("tenant-a", "doc-a"));
    let key = tenant_key("tenant-a");

    let response = h
        .send_json(
            "POST",
            "/v1/tables/doc-a_t0/query",
            &key,
            serde_json::json!({"select": [{"column": "Region"}]}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    // The QueryError JSON itself: tag plus fields, not a prose message.
    assert_eq!(body["error"], "restricted");
    assert!(body["reason"].as_str().unwrap().contains("aggregate"));
}

#[tokio::test]
async fn a_search_answer_names_the_table_and_lists_it() {
    let h = Harness::new();
    h.with_tenant("tenant-a");
    h.with_tenant("tenant-b");
    seed_document(&h, "tenant-a", "doc-a", None);
    h.tables.seed(record("tenant-a", "doc-a"));
    h.chunks.vector_hits.write().unwrap().push(table_hit());

    let response = h
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key("tenant-a"),
            serde_json::json!({"query": "revenue by region"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let chunk = &body["chunks"][0];
    assert_eq!(chunk["tableId"], "doc-a_t0");
    assert_eq!(chunk["tableTitle"], "Revenue by region");
    assert_eq!(body["tables"][0]["tableId"], "doc-a_t0");
    assert_eq!(body["tables"][0]["documentId"], "doc-a");

    // The same query as the other tenant sees neither the chunk nor the table.
    let response = h
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key("tenant-b"),
            serde_json::json!({"query": "revenue by region"}),
        )
        .await;
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("tables").is_none(),
        "tenant B was handed tenant A's tables: {body}"
    );
    assert!(
        body["chunks"].as_array().is_none_or(Vec::is_empty),
        "tenant B was handed tenant A's chunk: {body}"
    );
}

#[tokio::test]
async fn the_search_answer_is_unchanged_without_tables() {
    let h = Harness::new();
    h.with_tenant("tenant-a");
    seed_document(&h, "tenant-a", "doc-a", None);
    h.chunks.vector_hits.write().unwrap().push(make_fake_hit(
        "doc-a_0",
        "doc-a",
        "Revenue report",
        "text",
        0,
        0.9,
    ));
    let response = h
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key("tenant-a"),
            serde_json::json!({"query": "revenue"}),
        )
        .await;
    let body = json_body(response).await;
    assert_eq!(body["chunks"][0]["content"], "text");
    assert!(
        body.get("tables").is_none(),
        "a document with no tables must not grow a tables member: {body}"
    );
    assert!(body["chunks"][0].get("tableId").is_none());
}
