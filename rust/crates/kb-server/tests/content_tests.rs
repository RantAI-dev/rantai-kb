//! `GET /v1/documents/:id/content`: bounded, byte-exact reading with cursors.
//!
//! The fixture builds the same store records `ING-13` writes: text units whose
//! `content` repeats the tail of the previous unit and whose metadata carries
//! the character spans. The assertions are on the wire text and ranges, so a
//! route that re-searched, duplicated overlap or leaked another document fails
//! here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_ports::{
    ChunkId, ChunkType, DocumentId, IndexUnit, KnowledgeBaseId, RetrievalIndex, TenantId,
};

use harness::*;

const TENANT: &str = "tenant-a";
const OTHER: &str = "tenant-b";
const KB_A: &str = "kb-a";
const KB_B: &str = "kb-b";
const LIMITED: &str = "limited-key";

fn unit(doc: &str, index: u32, page: u32, content: &str, metadata: serde_json::Value) -> IndexUnit {
    IndexUnit {
        id: ChunkId(format!("{doc}_{index}")),
        document_id: DocumentId(doc.to_string()),
        tenant: TenantId(TENANT.to_string()),
        knowledge_base_id: None,
        category: None,
        chunk_index: index,
        page: Some(page),
        chunk_type: ChunkType::Text,
        content: content.to_string(),
        contextual_prefix: None,
        metadata,
        dense: vec![0.1; 4],
        sparse: kb_core::sparse::SparseVector::default(),
    }
}

/// A long document whose chunks overlap, plus the text a stitcher must return.
/// `expected` is built independently of `kb_core::content::stitch`.
struct Fixture {
    units: Vec<IndexUnit>,
    expected: String,
}

fn long_fixture(doc: &str, n: usize) -> Fixture {
    let mut units = Vec::new();
    let mut expected = String::new();
    let mut char_pos = 0u32;
    for i in 0..n {
        let fresh = format!(
            "Paragraph {i:02}: {} END_{i:02}.",
            "lorem ipsum dolor sit amet consectetur ".repeat(6)
        );
        let overlap = if i == 0 || i % 4 == 0 { 0 } else { 12 };
        let content = if overlap == 0 {
            fresh.clone()
        } else {
            let tail: String = expected
                .chars()
                .rev()
                .take(overlap)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("{tail}{fresh}")
        };
        if i == 0 {
            expected.push_str(&content);
        } else if overlap == 0 {
            expected.push_str("\n\n");
            expected.push_str(&content);
        } else {
            expected.push_str(&content.chars().skip(overlap).collect::<String>());
        }
        let added = content.chars().count() - overlap;
        let end = char_pos + added as u32;
        let page = (i / 3) as u32;
        units.push(unit(
            doc,
            i as u32,
            page,
            &content,
            serde_json::json!({
                "chunkIndex": i,
                "chunkType": "text",
                "page": page,
                "charStart": char_pos,
                "charEnd": end,
                "overlapPrefixLen": overlap,
            }),
        ));
        char_pos = end;
    }
    Fixture { units, expected }
}

async fn seed_long(h: &Harness, doc: &str, fixture: &Fixture, updated_at: Option<&str>) {
    let mut meta = doc_meta(doc, "Long document");
    meta.sha256 = None; // the hash fallback is updatedAt
    if let Some(value) = updated_at {
        meta.updated_at = value.to_string();
    }
    h.documents.insert_doc(TenantId(TENANT.to_string()), meta);
    h.chunks.upsert(fixture.units.clone()).await.unwrap();
}

/// An Indonesian regulation with one legal marker per chunk, so sections own
/// single chunks and the section selector's range is unambiguous.
async fn seed_regulation(h: &Harness, doc: &str) {
    let mut meta = doc_meta(doc, "Peraturan");
    meta.sha256 = None;
    h.documents.insert_doc(TenantId(TENANT.to_string()), meta);
    let rows = [
        (
            0u32,
            0u32,
            "BAB I KETENTUAN UMUM\nPasal 1\n(1) Berhak atas pendidikan.",
        ),
        (1, 0, "Pasal 2\n(1) Berhak atas pekerjaan."),
        (2, 1, "BAB II HAK DAN KEWAJIBAN\nBagian Kesatu\nHak"),
        (3, 1, "Pasal 3\n(1) Berhak atas upah."),
    ];
    let units = rows
        .into_iter()
        .map(|(index, page, content)| {
            unit(
                doc,
                index,
                page,
                content,
                serde_json::json!({
                    "chunkIndex": index,
                    "chunkType": "text",
                    "page": page,
                    "charStart": index * 40,
                    "charEnd": (index + 1) * 40,
                    "overlapPrefixLen": 0,
                }),
            )
        })
        .collect();
    h.chunks.upsert(units).await.unwrap();
}

#[tokio::test]
async fn paged_reading_is_byte_exact_and_never_repeats_an_overlap() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let fixture = long_fixture("long", 14);
    seed_long(&h, "long", &fixture, None).await;

    // One unbounded read is the reference.
    let whole = h
        .send(
            "GET",
            "/v1/documents/long/content?maxChars=50000",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(whole.status(), StatusCode::OK);
    let whole = json_body(whole).await;
    assert_eq!(whole["text"], fixture.expected);
    assert_eq!(whole["truncated"], false);
    assert_eq!(whole["nextCursor"], serde_json::Value::Null);

    // Walk the same document in small pages, following only the cursor.
    let mut joined = String::new();
    let mut uri = "/v1/documents/long/content?maxChars=500".to_string();
    let mut pages = 0usize;
    loop {
        let response = h.send("GET", &uri, &tenant_key(TENANT)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        joined.push_str(body["text"].as_str().expect("text"));
        pages += 1;
        match body["nextCursor"].as_str() {
            Some(cursor) => {
                uri = format!("/v1/documents/long/content?cursor={cursor}&maxChars=500")
            }
            None => break,
        }
        assert!(pages < 100, "the cursor never ended");
    }
    assert!(pages > 1, "the fixture should need several pages");
    assert_eq!(joined, fixture.expected, "paged text differs from one read");
    for i in 0..14 {
        let marker = format!("Paragraph {i:02}:");
        assert_eq!(
            joined.matches(&marker).count(),
            1,
            "{marker} was duplicated at a page join"
        );
    }
}

#[tokio::test]
async fn max_chars_is_respected_and_at_least_one_chunk_is_emitted() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let fixture = long_fixture("long", 14);
    seed_long(&h, "long", &fixture, None).await;

    let response = h
        .send(
            "GET",
            "/v1/documents/long/content?maxChars=500",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let text = body["text"].as_str().expect("text");
    assert!(!text.is_empty());
    assert!(
        text.chars().count() <= 500,
        "{} chars",
        text.chars().count()
    );
    assert_eq!(body["truncated"], true);
    assert!(body["nextCursor"].as_str().is_some());

    // No selector means the whole document from chunk 0.
    let all = h
        .send("GET", "/v1/documents/long/content", &tenant_key(TENANT))
        .await;
    assert_eq!(json_body(all).await["range"]["chunkStart"], 0);
}

#[tokio::test]
async fn a_document_that_changes_between_pages_answers_409() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let fixture = long_fixture("long", 14);
    seed_long(&h, "long", &fixture, Some("2026-03-01T00:00:00.000Z")).await;

    let first = h
        .send(
            "GET",
            "/v1/documents/long/content?maxChars=500",
            &tenant_key(TENANT),
        )
        .await;
    let cursor = json_body(first).await["nextCursor"]
        .as_str()
        .expect("cursor")
        .to_string();

    // PATCH bumps updatedAt in the fake document store, and sha256 is null, so
    // the content hash the cursor was issued with no longer matches.
    let patch = h
        .send_json(
            "PATCH",
            "/v1/documents/long",
            &tenant_key(TENANT),
            serde_json::json!({"title": "Changed"}),
        )
        .await;
    assert_eq!(patch.status(), StatusCode::OK);

    let response = h
        .send(
            "GET",
            &format!("/v1/documents/long/content?cursor={cursor}&maxChars=500"),
            &tenant_key(TENANT),
        )
        .await;
    assert_error(
        response,
        StatusCode::CONFLICT,
        "document changed since cursor was issued",
    )
    .await;
}

#[tokio::test]
async fn content_hides_another_tenant_a_kb_limited_document_and_a_deleted_one() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.with_tenant(OTHER);
    let fixture = long_fixture("secret", 8);

    // Another tenant's document: the same 404 as an unknown id.
    seed_long(&h, "secret", &fixture, None).await;
    let response = h
        .send("GET", "/v1/documents/secret/content", &tenant_key(OTHER))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;

    // A key limited to KB_A cannot see a document in KB_B.
    h.documents.insert_api_key_with_scopes(
        LIMITED,
        TenantId(TENANT.to_string()),
        vec!["kb:read".into()],
        vec![KnowledgeBaseId(KB_A.to_string())],
    );
    let mut limited_doc = doc_meta("restricted", "Restricted");
    limited_doc.knowledge_base_ids = vec![KB_B.to_string()];
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), limited_doc);
    h.chunks.upsert(fixture.units.clone()).await.unwrap();
    let response = h
        .send("GET", "/v1/documents/restricted/content", LIMITED)
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;

    // A soft delete hides the document from every later read.
    let mut deleted = doc_meta("deleted", "Deleted");
    deleted.sha256 = None;
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), deleted);
    h.chunks.upsert(fixture.units.clone()).await.unwrap();
    let removed = h
        .send("DELETE", "/v1/documents/deleted", &tenant_key(TENANT))
        .await;
    assert_eq!(removed.status(), StatusCode::OK);
    let response = h
        .send("GET", "/v1/documents/deleted/content", &tenant_key(TENANT))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn a_cursor_for_document_a_cannot_be_used_on_document_b() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let fixture = long_fixture("doc-a", 14);
    seed_long(&h, "doc-a", &fixture, None).await;
    let other = long_fixture("doc-b", 14);
    seed_long(&h, "doc-b", &other, None).await;

    let first = h
        .send(
            "GET",
            "/v1/documents/doc-a/content?maxChars=500",
            &tenant_key(TENANT),
        )
        .await;
    let cursor = json_body(first).await["nextCursor"]
        .as_str()
        .expect("cursor")
        .to_string();

    let response = h
        .send(
            "GET",
            &format!("/v1/documents/doc-b/content?cursor={cursor}"),
            &tenant_key(TENANT),
        )
        .await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "cursor belongs to another document",
    )
    .await;
}

#[tokio::test]
async fn section_selects_exactly_one_sections_chunk_range() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    seed_regulation(&h, "regulation").await;

    let structure = h
        .send(
            "GET",
            "/v1/documents/regulation/structure",
            &tenant_key(TENANT),
        )
        .await;
    let body = json_body(structure).await;
    let section = body["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .find(|section| section["title"] == "Pasal 2")
        .expect("Pasal 2 section");
    let id = section["id"].as_str().expect("section id");
    assert_eq!(section["chunkStart"], 1);
    assert_eq!(section["chunkEnd"], 1);

    let response = h
        .send(
            "GET",
            &format!("/v1/documents/regulation/content?section={id}"),
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["range"]["chunkStart"], 1);
    assert_eq!(body["range"]["chunkEnd"], 1);
    assert_eq!(
        body["text"], "Pasal 2\n(1) Berhak atas pekerjaan.",
        "only this section's chunk is emitted"
    );
}

#[tokio::test]
async fn two_selectors_are_a_400() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    seed_regulation(&h, "regulation").await;

    let response = h
        .send(
            "GET",
            "/v1/documents/regulation/content?section=s1&pageStart=0&pageEnd=1",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        h.documents
            .docs
            .read()
            .unwrap()
            .values()
            .flatten()
            .any(|doc| doc.id.as_str() == "regulation"),
        "the fixture is still visible: the 400 is about selectors"
    );
}

#[tokio::test]
async fn an_unknown_section_and_a_malformed_cursor_are_a_400() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    seed_regulation(&h, "regulation").await;

    for uri in [
        "/v1/documents/regulation/content?section=s99",
        "/v1/documents/regulation/content?cursor=not-a-cursor",
        "/v1/documents/regulation/content?pageStart=0",
    ] {
        let response = h.send("GET", uri, &tenant_key(TENANT)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
}
