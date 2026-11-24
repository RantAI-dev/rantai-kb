//! RET-15: `pageRange` and `section` narrow a search to one part of one
//! document, inside every arm. The instrument is the real router over the
//! in-process fakes; the fakes filter on the very same `DocumentScope` the
//! Qdrant adapter filters on, so a hit outside the part cannot slip through an
//! arm the test forgot.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_core::sparse::SparseVector;
use kb_engine::fakes::make_fake_hit;
use kb_ports::{ChunkId, ChunkType, DocumentId, IndexUnit, RetrievalIndex, TenantId};

use harness::{Harness, doc_meta, error_text, json_body, tenant_key};

const KEYWORD: &str = "zxqrange";

fn seed_doc(harness: &Harness, tenant: &str, id: &str, kb: &str) {
    let mut meta = doc_meta(id, &format!("Doc {id}"));
    meta.knowledge_base_ids = vec![kb.to_string()];
    harness
        .documents
        .insert_doc(TenantId(tenant.to_string()), meta);
}

fn keyword_hit(id: &str, content: &str, page: u32, chunk_index: u32) -> kb_ports::SearchHit {
    let mut hit = make_fake_hit(id, "doc-a", "Doc doc-a", content, chunk_index, 0.9);
    hit.chunk.metadata.page = Some(page);
    hit.chunk.metadata.chunk_index = Some(chunk_index);
    hit
}

fn seed_keyword(harness: &Harness, content: &str, page: u32, chunk_index: u32) {
    let mut hits = harness.chunks.bm25_hits.write().unwrap();
    hits.push(keyword_hit(
        &format!("doc-a-{chunk_index}"),
        content,
        page,
        chunk_index,
    ));
}

/// A figure the figure arm can select: it needs an asset key and a caption.
fn seed_figure(harness: &Harness, id: &str, page: u32, chunk_index: u32, caption: &str) {
    let mut hit = make_fake_hit(id, "doc-a", "Doc doc-a", caption, chunk_index, 0.9);
    hit.chunk.chunk_type = ChunkType::Figure;
    hit.chunk.metadata.page = Some(page);
    hit.chunk.metadata.chunk_index = Some(chunk_index);
    hit.chunk.metadata.asset_key = Some(format!("{id}.png"));
    hit.chunk.metadata.section = Some(caption.to_string());
    harness.chunks.figure_hits.write().unwrap().push(hit);
}

/// Stored chunks are what `list_chunk_meta` reads when a section id is
/// resolved, exactly as `/v1/documents/:id/structure` does.
async fn seed_stored_chunk(
    harness: &Harness,
    chunk_index: u32,
    page: u32,
    section: &str,
    content: &str,
) {
    harness
        .chunks
        .upsert(vec![IndexUnit {
            id: ChunkId(format!("doc-a-{chunk_index}")),
            document_id: DocumentId("doc-a".to_string()),
            tenant: TenantId("tenant-a".to_string()),
            knowledge_base_id: None,
            category: None,
            chunk_index,
            page: Some(page),
            chunk_type: ChunkType::Text,
            content: content.to_string(),
            contextual_prefix: None,
            metadata: serde_json::json!({
                "page": page,
                "chunkIndex": chunk_index,
                "section": section,
            }),
            dense: Vec::new(),
            sparse: SparseVector::empty(),
        }])
        .await
        .unwrap();
}

async fn search(harness: &Harness, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let response = harness
        .send_json("POST", "/v1/search", &tenant_key("tenant-a"), body)
        .await;
    let status = response.status();
    (status, json_body(response).await)
}

fn contents(body: &serde_json::Value) -> Vec<String> {
    body["chunks"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|c| c["content"].as_str().map(str::to_string))
        .collect()
}

/// T1: the same keyword on page 3 and page 30. Without a range both are
/// returned; with `pageRange 1–5` only the page-3 one survives.
#[tokio::test]
async fn a_page_range_keeps_only_the_keyword_hit_inside_it() {
    let harness = Harness::new();
    harness.with_tenant("tenant-a");
    seed_doc(&harness, "tenant-a", "doc-a", "kb-1");
    seed_keyword(&harness, &format!("marker-three {KEYWORD} here"), 3, 3);
    seed_keyword(&harness, &format!("marker-thirty {KEYWORD} here"), 30, 30);

    let (status, all) = search(
        &harness,
        serde_json::json!({"query": KEYWORD, "documentIds": ["doc-a"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let all = contents(&all);
    assert!(all.iter().any(|c| c.contains("marker-three")), "{all:?}");
    assert!(all.iter().any(|c| c.contains("marker-thirty")), "{all:?}");

    let (status, ranged) = search(
        &harness,
        serde_json::json!({
            "query": KEYWORD,
            "documentIds": ["doc-a"],
            "pageRange": {"start": 1, "end": 5},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ranged = contents(&ranged);
    assert!(
        ranged.iter().any(|c| c.contains("marker-three")),
        "{ranged:?}"
    );
    assert!(
        !ranged.iter().any(|c| c.contains("marker-thirty")),
        "a chunk outside the range leaked: {ranged:?}"
    );
}

/// T2: the figure arm cannot bring in a figure printed outside the range.
#[tokio::test]
async fn a_figure_outside_the_page_range_is_never_returned() {
    let harness = Harness::new();
    harness.with_tenant("tenant-a");
    seed_doc(&harness, "tenant-a", "doc-a", "kb-1");
    // A ranked text hit is needed for the figure arm to run at all.
    seed_keyword(&harness, &format!("the text about {KEYWORD}"), 3, 3);
    seed_figure(&harness, "fig-page-3", 3, 3, "a caption on page three");
    seed_figure(&harness, "fig-page-30", 30, 30, "a caption on page thirty");

    let (status, all) = search(
        &harness,
        serde_json::json!({"query": KEYWORD, "documentIds": ["doc-a"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let all = contents(&all);
    assert!(all.iter().any(|c| c.contains("page three")), "{all:?}");
    assert!(all.iter().any(|c| c.contains("page thirty")), "{all:?}");

    let (status, ranged) = search(
        &harness,
        serde_json::json!({
            "query": KEYWORD,
            "documentIds": ["doc-a"],
            "pageRange": {"start": 1, "end": 5},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ranged = contents(&ranged);
    assert!(
        ranged.iter().any(|c| c.contains("page three")),
        "{ranged:?}"
    );
    assert!(
        !ranged.iter().any(|c| c.contains("page thirty")),
        "a figure outside the range leaked: {ranged:?}"
    );
}

/// T3: `section` resolves through the outline v2 to that section's chunk
/// range, and only its chunks come back.
#[tokio::test]
async fn a_section_keeps_only_the_chunks_in_its_range() {
    let harness = Harness::new();
    harness.with_tenant("tenant-a");
    seed_doc(&harness, "tenant-a", "doc-a", "kb-1");
    for index in 0..10 {
        let section = if index < 5 {
            "Bab I Leave"
        } else {
            "Bab II Cuti"
        };
        seed_stored_chunk(
            &harness,
            index,
            index,
            section,
            &format!("chunk {index} body"),
        )
        .await;
    }
    seed_keyword(&harness, &format!("in section one {KEYWORD}"), 3, 3);
    seed_keyword(&harness, &format!("in section two {KEYWORD}"), 7, 7);

    let (status, all) = search(
        &harness,
        serde_json::json!({"query": KEYWORD, "documentIds": ["doc-a"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let all = contents(&all);
    assert!(all.iter().any(|c| c.contains("section one")), "{all:?}");
    assert!(all.iter().any(|c| c.contains("section two")), "{all:?}");

    let (status, ranged) = search(
        &harness,
        serde_json::json!({
            "query": KEYWORD,
            "documentIds": ["doc-a"],
            "section": "s2",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ranged}");
    let ranged = contents(&ranged);
    assert!(
        ranged.iter().any(|c| c.contains("section two")),
        "{ranged:?}"
    );
    assert!(
        !ranged.iter().any(|c| c.contains("section one")),
        "a chunk outside the section leaked: {ranged:?}"
    );
}

/// T4: the request-shape 400s and the unknown section id.
#[tokio::test]
async fn invalid_range_requests_are_rejected_with_400() {
    let harness = Harness::new();
    harness.with_tenant("tenant-a");
    seed_doc(&harness, "tenant-a", "doc-a", "kb-1");

    let cases = vec![
        (
            serde_json::json!({"query": KEYWORD, "pageRange": {"start": 1, "end": 5}}),
            "pageRange/section need exactly one documentIds entry",
        ),
        (
            serde_json::json!({"query": KEYWORD, "section": "s2"}),
            "pageRange/section need exactly one documentIds entry",
        ),
        (
            serde_json::json!({
                "query": KEYWORD,
                "documentIds": ["doc-a", "doc-a"],
                "pageRange": {"start": 1, "end": 5},
            }),
            "pageRange/section need exactly one documentIds entry",
        ),
        (
            serde_json::json!({
                "query": KEYWORD,
                "documentIds": ["doc-a"],
                "pageRange": {"start": 1, "end": 5},
                "section": "s2",
            }),
            "pageRange and section cannot be used together",
        ),
        (
            serde_json::json!({
                "query": KEYWORD,
                "documentIds": ["doc-a"],
                "pageRange": {"start": 5, "end": 1},
            }),
            "pageRange.start must be <= pageRange.end",
        ),
        (
            serde_json::json!({
                "query": KEYWORD,
                "documentIds": ["doc-a"],
                "section": "s99",
            }),
            "unknown section id: s99",
        ),
    ];
    for (body, expected) in cases {
        let response = harness
            .send_json("POST", "/v1/search", &tenant_key("tenant-a"), body.clone())
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(error_text(response).await, expected, "{body}");
    }
}

/// T5: a range on another tenant's document is an empty answer, not a 400:
/// the existing `documentIds` visibility rule still applies first.
#[tokio::test]
async fn a_page_range_on_another_tenants_document_is_empty_not_400() {
    let harness = Harness::new();
    harness.with_tenant("tenant-a");
    harness.with_tenant("tenant-b");
    seed_doc(&harness, "tenant-b", "doc-theirs", "kb-1");
    seed_keyword(&harness, &format!("{KEYWORD} on page three"), 3, 3);

    let (status, answer) = search(
        &harness,
        serde_json::json!({
            "query": KEYWORD,
            "documentIds": ["doc-theirs"],
            "pageRange": {"start": 1, "end": 5},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert!(contents(&answer).is_empty(), "{answer}");
}
