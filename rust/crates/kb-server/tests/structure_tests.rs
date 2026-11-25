//! `GET /v1/documents/:id/structure`: pages, figures and the chunk → page map.
//!
//! The fixture builds the same stores ingest writes: text and figure units in
//! the text index, page units in the page-image index, and figure rows on the
//! document. The assertions are in terms of what a search hit can reference —
//! a figure chunk's `key` and a chunk/page pair — rather than in terms of an
//! arbitrary response shape.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_ports::{
    ChunkId, ChunkType, DocumentId, DocumentMeta, IndexUnit, KnowledgeBaseId, RetrievalIndex,
    TenantId,
};

use harness::*;

const TENANT: &str = "tenant-a";
const OTHER: &str = "tenant-b";
const KB_A: &str = "kb-a";
const KB_B: &str = "kb-b";
const LIMITED: &str = "limited-key";

fn unit(
    tenant: &str,
    doc: &str,
    index: u32,
    chunk_type: ChunkType,
    page: Option<u32>,
    content: &str,
    metadata: serde_json::Value,
) -> IndexUnit {
    IndexUnit {
        id: ChunkId(format!("{doc}_{index}")),
        document_id: DocumentId(doc.to_string()),
        tenant: TenantId(tenant.to_string()),
        knowledge_base_id: None,
        category: None,
        chunk_index: index,
        page,
        chunk_type,
        content: content.to_string(),
        contextual_prefix: None,
        metadata,
        dense: vec![0.1; 4],
        sparse: kb_core::sparse::SparseVector::default(),
    }
}

fn doc_with_ref(id: &str) -> DocumentMeta {
    let mut doc = doc_meta(id, "Annual report");
    doc.external_ref = Some("calling-app-ref-1".to_string());
    doc
}

/// A document with two text chunks, two figure chunks (one cropped, one a
/// visual description), a figure row with no chunk, and three page units.
async fn seed(h: &Harness) {
    h.documents.insert_doc(
        TenantId(TENANT.to_string()),
        DocumentMeta {
            sha256: None,
            doc_key: None,
            version: None,
            supersedes_document_id: None,
            effective_from: None,
            effective_to: None,
            is_latest: true,
            acl_principals: Vec::new(),
            metadata: Some(serde_json::json!({"figures": [
                {
                    "assetKey": "documents/tenant-a/doc-1/assets/fig-p1-1.png",
                    "page": 1,
                    "caption": "Revenue chart",
                    "bbox": [10.0, 20.0, 110.0, 220.0],
                    "type": "chart"
                },
                {
                    "assetKey": "documents/tenant-a/doc-1/assets/fig-p3-1.png",
                    "page": 3,
                    "caption": "Appendix table",
                    "bbox": [5.0, 6.0, 105.0, 206.0],
                    "type": "table"
                }
            ]})),
            ..doc_with_ref("doc-1")
        },
    );

    h.chunks
        .upsert(vec![
            unit(
                TENANT,
                "doc-1",
                0,
                ChunkType::Text,
                Some(0),
                "page zero text",
                serde_json::json!({"chunkIndex": 0, "chunkType": "text", "page": 0}),
            ),
            unit(
                TENANT,
                "doc-1",
                1,
                ChunkType::Figure,
                Some(1),
                "[chart] Revenue chart",
                serde_json::json!({
                    "chunkIndex": 1,
                    "chunkType": "figure",
                    "page": 1,
                    "section": "Revenue chart",
                    "assetKey": "documents/tenant-a/doc-1/assets/fig-p1-1.png"
                }),
            ),
            unit(
                TENANT,
                "doc-1",
                2,
                ChunkType::Text,
                Some(2),
                "page two text",
                serde_json::json!({"chunkIndex": 2, "chunkType": "text", "page": 2}),
            ),
            unit(
                TENANT,
                "doc-1",
                3,
                ChunkType::Figure,
                Some(2),
                "[Visual] a diagram",
                serde_json::json!({
                    "chunkIndex": 3,
                    "chunkType": "figure",
                    "page": 2,
                    "section": "Visual description"
                }),
            ),
        ])
        .await
        .unwrap();

    let pages = h.page_index.as_ref().expect("page index");
    pages
        .upsert(vec![
            unit(
                TENANT,
                "doc-1",
                0,
                ChunkType::Page,
                Some(0),
                "the text layer of page zero",
                serde_json::json!({
                    "chunkIds": ["doc-1_0"],
                    "page": 0,
                    "hasTextLayer": true,
                    "ocr": false,
                    "image": true
                }),
            ),
            unit(
                TENANT,
                "doc-1",
                1,
                ChunkType::Page,
                Some(1),
                "",
                serde_json::json!({
                    "chunkIds": ["doc-1_1"],
                    "page": 1,
                    "hasTextLayer": false,
                    "ocr": false,
                    "image": true
                }),
            ),
            unit(
                TENANT,
                "doc-1",
                2,
                ChunkType::Page,
                Some(2),
                "OCR page two text\n\n[Visual] a diagram",
                serde_json::json!({
                    "chunkIds": ["doc-1_2", "doc-1_3"],
                    "page": 2,
                    "hasTextLayer": false,
                    "ocr": true,
                    "image": false
                }),
            ),
        ])
        .await
        .unwrap();
}

#[tokio::test]
async fn structure_lists_every_page_and_figure_a_search_can_return() {
    let h = Harness::with_page_index();
    h.with_tenant(TENANT);
    seed(&h).await;

    let response = h
        .send("GET", "/v1/documents/doc-1/structure", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    assert_eq!(body["documentId"], "doc-1");
    assert_eq!(body["externalRef"], "calling-app-ref-1");
    assert_eq!(body["title"], "Annual report");
    assert_eq!(body["truncated"], false);

    // Every page whose number a seeded chunk or figure can reference is here.
    let numbers: Vec<u64> = body["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .map(|p| p["number"].as_u64().expect("number"))
        .collect();
    assert_eq!(numbers, vec![0, 1, 2, 3], "pages are 0-based and ascending");

    let page_zero = &body["pages"][0];
    assert_eq!(page_zero["hasTextLayer"], true);
    assert_eq!(page_zero["ocrText"], serde_json::Value::Null);
    assert_eq!(
        page_zero["imageAssetKey"],
        "documents/tenant-a/doc-1/pages/p-00001.jpg"
    );

    let page_two = &body["pages"][2];
    assert_eq!(page_two["hasTextLayer"], false);
    assert_eq!(
        page_two["ocrText"], "OCR page two text",
        "the visual description the caption stage appended is not OCR output"
    );
    assert_eq!(page_two["imageAssetKey"], serde_json::Value::Null);

    // Page 3 has no page unit: it is listed from the figure row and the
    // document row's figure, with no invented page fields.
    let page_three = &body["pages"][3];
    assert_eq!(page_three["hasTextLayer"], serde_json::Value::Null);
    assert_eq!(page_three["ocrText"], serde_json::Value::Null);
    assert_eq!(page_three["imageAssetKey"], serde_json::Value::Null);

    // Every figure chunk a search can return is listed, under the same key a
    // search hit uses.
    let keys: Vec<&str> = body["figures"]
        .as_array()
        .expect("figures")
        .iter()
        .map(|f| f["key"].as_str().expect("figure key"))
        .collect();
    for chunk_index in [1u32, 3u32] {
        let expected = kb_server::dto::chunk_id_for_client(&format!("doc-1_{chunk_index}"));
        assert!(
            keys.contains(&expected.as_str()),
            "missing {expected}: {keys:?}"
        );
    }
    // The cropped figure merges bbox and caption from the document row.
    let chart = body["figures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["assetKey"] == "documents/tenant-a/doc-1/assets/fig-p1-1.png")
        .expect("cropped figure");
    assert_eq!(chart["page"], 1);
    assert_eq!(chart["bbox"], serde_json::json!([10.0, 20.0, 110.0, 220.0]));
    assert_eq!(chart["caption"], "Revenue chart");
    assert_eq!(chart["ocrText"], serde_json::Value::Null);
    // The visual-description figure has no crop, so its asset key is null.
    let visual = body["figures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["caption"] == "Visual description")
        .expect("visual figure");
    assert_eq!(visual["assetKey"], serde_json::Value::Null);
    // The figure row with no chunk is still a figure of the document.
    let uncropped = body["figures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["assetKey"] == "documents/tenant-a/doc-1/assets/fig-p3-1.png")
        .expect("figure row without a chunk");
    assert_eq!(uncropped["page"], 3);
    assert_eq!(
        uncropped["key"],
        "documents/tenant-a/doc-1/assets/fig-p3-1.png"
    );

    // The chunk map names every text-index unit, in chunk order.
    let chunk_ids: Vec<&str> = body["chunks"]
        .as_array()
        .expect("chunks")
        .iter()
        .map(|c| c["chunkId"].as_str().expect("chunk id"))
        .collect();
    let expected: Vec<String> = (0..4)
        .map(|i| kb_server::dto::chunk_id_for_client(&format!("doc-1_{i}")))
        .collect();
    assert_eq!(chunk_ids, expected);
    assert_eq!(body["chunks"][0]["page"], 0);
    assert_eq!(body["chunks"][2]["page"], 2);
}

#[tokio::test]
async fn page_units_from_before_provenance_was_recorded_answer_null_not_a_guess() {
    let h = Harness::with_page_index();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_with_ref("doc-old"));
    let pages = h.page_index.as_ref().unwrap();
    pages
        .upsert(vec![unit(
            TENANT,
            "doc-old",
            0,
            ChunkType::Page,
            Some(0),
            "some stored page text",
            serde_json::json!({"chunkIds": [], "page": 0}),
        )])
        .await
        .unwrap();

    let response = h
        .send(
            "GET",
            "/v1/documents/doc-old/structure",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let page = &body["pages"][0];
    assert_eq!(page["number"], 0);
    assert_eq!(page["hasTextLayer"], serde_json::Value::Null);
    assert_eq!(page["ocrText"], serde_json::Value::Null);
    assert_eq!(page["imageAssetKey"], serde_json::Value::Null);
}

// ─── SEC-1: tenant and the key's knowledge-base limit ───────────────────────

fn limited_key(h: &Harness) {
    h.documents.insert_api_key_with_scopes(
        LIMITED,
        TenantId(TENANT.to_string()),
        vec!["kb:read".into()],
        vec![KnowledgeBaseId(KB_A.to_string())],
    );
}

fn doc_in_kb(h: &Harness, id: &str, kb: &str) {
    let mut doc = doc_with_ref(id);
    doc.knowledge_base_ids = vec![kb.to_string()];
    h.documents.insert_doc(TenantId(TENANT.to_string()), doc);
}

#[tokio::test]
async fn structure_across_tenants_is_a_404() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(OTHER.to_string()), doc_with_ref("doc-other"));

    let response = h
        .send(
            "GET",
            "/v1/documents/doc-other/structure",
            &tenant_key(TENANT),
        )
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn structure_outside_the_keys_knowledge_bases_is_a_404() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    limited_key(&h);
    doc_in_kb(&h, "doc-b", KB_B);

    let response = h
        .send("GET", "/v1/documents/doc-b/structure", LIMITED)
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn structure_in_no_knowledge_base_is_a_404_for_a_limited_key() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    limited_key(&h);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_with_ref("doc-none"));

    let response = h
        .send("GET", "/v1/documents/doc-none/structure", LIMITED)
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn structure_in_the_keys_knowledge_base_is_returned() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    limited_key(&h);
    doc_in_kb(&h, "doc-a", KB_A);

    let response = h
        .send("GET", "/v1/documents/doc-a/structure", LIMITED)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["documentId"], "doc-a");
}

#[tokio::test]
async fn an_unlimited_key_reads_structure_in_any_knowledge_base() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    doc_in_kb(&h, "doc-b", KB_B);

    let response = h
        .send("GET", "/v1/documents/doc-b/structure", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

// ─── RET-13: outline v2 ─────────────────────────────────────────────────────

/// An Indonesian regulation: no markdown headings, just legal markers at the
/// start of lines. One marker per chunk so every section owns a chunk range.
async fn seed_regulation(h: &Harness, id: &str) {
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta(id, "Peraturan"));
    let rows = [
        (
            0u32,
            0u32,
            "BAB I KETENTUAN UMUM\nPasal 1\n(1) Setiap orang berhak atas pendidikan.",
            (0u32, 100u32, 0u32),
        ),
        (
            1,
            0,
            "Pasal 2\n(1) Setiap orang berhak atas pekerjaan.",
            (100, 200, 10),
        ),
        (
            2,
            1,
            "BAB II HAK DAN KEWAJIBAN\nBagian Kesatu\nHak",
            (200, 300, 0),
        ),
        (
            3,
            1,
            "Pasal 3\n(1) Pekerja berhcp atas upah.",
            (300, 450, 12),
        ),
    ];
    let units: Vec<IndexUnit> = rows
        .into_iter()
        .map(|(index, page, content, (start, end, overlap))| {
            unit(
                TENANT,
                id,
                index,
                ChunkType::Text,
                Some(page),
                content,
                serde_json::json!({
                    "chunkIndex": index,
                    "chunkType": "text",
                    "page": page,
                    "charStart": start,
                    "charEnd": end,
                    "overlapPrefixLen": overlap,
                }),
            )
        })
        .collect();
    h.chunks.upsert(units).await.unwrap();
}

#[tokio::test]
async fn outline_v2_of_a_regulation_has_the_bab_pasal_tree_with_ranges() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    seed_regulation(&h, "regulation").await;

    let response = h
        .send(
            "GET",
            "/v1/documents/regulation/structure",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    // All the old fields are still on the wire, unchanged.
    assert_eq!(body["documentId"], "regulation");
    assert_eq!(body["title"], "Peraturan");
    assert!(body.get("pages").is_some());
    assert!(body.get("figures").is_some());
    assert!(body.get("chunks").is_some());
    assert_eq!(body["truncated"], false);

    assert_eq!(body["outlineVersion"], 2);
    assert_eq!(body["hasSpans"], true);
    assert_eq!(body["estimatedChars"], 450);

    let sections = body["sections"].as_array().expect("sections");
    let titles: Vec<&str> = sections
        .iter()
        .map(|s| s["title"].as_str().expect("title"))
        .collect();
    assert_eq!(
        titles,
        [
            "BAB I KETENTUAN UMUM",
            "Pasal 1",
            "Pasal 2",
            "BAB II HAK DAN KEWAJIBAN",
            "Bagian Kesatu",
            "Pasal 3",
        ]
    );
    assert_eq!(sections[0]["marker"], "bab");
    assert_eq!(sections[0]["level"], 1);
    assert_eq!(
        sections[0]["path"],
        serde_json::json!(["BAB I KETENTUAN UMUM"])
    );
    assert_eq!(sections[1]["marker"], "pasal");
    assert_eq!(sections[1]["id"], "s2");
    assert_eq!(
        sections[1]["path"],
        serde_json::json!(["BAB I KETENTUAN UMUM", "Pasal 1"])
    );
    assert_eq!(sections[2]["id"], "s3");
    assert_eq!(sections[2]["chunkStart"], 1);
    assert_eq!(sections[2]["chunkEnd"], 1);
    assert_eq!(sections[2]["pageStart"], 0);
    assert_eq!(sections[2]["pageEnd"], 0);
    assert_eq!(sections[2]["estimatedChars"], 100);
    assert_eq!(sections[4]["marker"], "bagian");
    assert_eq!(sections[5]["marker"], "pasal");
    assert_eq!(
        sections[5]["path"],
        serde_json::json!(["BAB II HAK DAN KEWAJIBAN", "Bagian Kesatu", "Pasal 3"])
    );
    assert_eq!(sections[5]["pageStart"], 1);
    assert_eq!(sections[5]["estimatedChars"], 150);
}

#[tokio::test]
async fn outline_v2_falls_back_to_per_page_sections_without_spans() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("plain", "Scanned"));
    h.chunks
        .upsert(vec![
            unit(
                TENANT,
                "plain",
                0,
                ChunkType::Text,
                Some(0),
                "aaaa",
                serde_json::json!({"chunkIndex": 0, "page": 0}),
            ),
            unit(
                TENANT,
                "plain",
                1,
                ChunkType::Text,
                Some(1),
                "bb",
                serde_json::json!({"chunkIndex": 1, "page": 1}),
            ),
        ])
        .await
        .unwrap();

    let response = h
        .send("GET", "/v1/documents/plain/structure", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["outlineVersion"], 2);
    assert_eq!(body["hasSpans"], false);
    let sections = body["sections"].as_array().expect("sections");
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0]["title"], "Page 1");
    assert_eq!(sections[0]["marker"], "page");
    assert_eq!(sections[1]["title"], "Page 2");
    assert_eq!(body["estimatedChars"], 6);
}

#[tokio::test]
async fn outline_v2_never_lists_another_tenants_chunks() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.with_tenant(OTHER);
    seed_regulation(&h, "secret").await;

    let response = h
        .send("GET", "/v1/documents/secret/structure", &tenant_key(OTHER))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn outline_v2_stays_hidden_from_a_key_outside_the_documents_kb() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    limited_key(&h);
    doc_in_kb(&h, "restricted", KB_B);
    h.chunks
        .upsert(vec![unit(
            TENANT,
            "restricted",
            0,
            ChunkType::Text,
            Some(0),
            "BAB I RAHASIA",
            serde_json::json!({"chunkIndex": 0, "page": 0}),
        )])
        .await
        .unwrap();

    let response = h
        .send("GET", "/v1/documents/restricted/structure", LIMITED)
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn structure_without_kb_read_is_a_403() {
    let h = Harness::new();
    h.documents.insert_api_key_with_scopes(
        &tenant_key(TENANT),
        TenantId(TENANT.to_string()),
        vec!["kb:write".into()],
        vec![],
    );
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_with_ref("doc-1"));

    let response = h
        .send("GET", "/v1/documents/doc-1/structure", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
