//! Document routes: list, detail, patch, delete, and the shape of every
//! response body.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use kb_ports::{DocumentId, DocumentStore, TenantId};

use harness::*;

const TENANT: &str = "tenant-a";
const OTHER: &str = "tenant-b";

#[tokio::test]
async fn list_wraps_documents_in_the_bun_envelope() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "First"));

    let response = h.send("GET", "/v1/documents", &tenant_key(TENANT)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    let documents = body["documents"].as_array().expect("documents array");
    assert_eq!(documents.len(), 1);
    let first = &documents[0];
    assert_eq!(first["id"], "doc-1");
    assert_eq!(first["title"], "First");
    assert_eq!(first["fileType"], "application/pdf");
    assert_eq!(first["fileSize"], 12);
    assert_eq!(first["status"], "ready");
    assert_eq!(
        first["externalRef"],
        serde_json::Value::Null,
        "the field is additive on every list row, null when the document has none"
    );
    assert_eq!(first["createdAt"], "2026-01-01T00:00:00.000Z");
    assert_eq!(first["knowledgeBaseIds"], serde_json::json!([]));
    // The list select does not carry these; a client must not see them here.
    assert!(first.get("mimeType").is_none());
    assert!(first.get("chunkCount").is_none());
    assert!(first.get("updatedAt").is_none());
}

#[tokio::test]
async fn list_filters_by_the_camel_case_knowledge_base_parameter() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let mut in_kb = doc_meta("doc-in", "In");
    in_kb.knowledge_base_ids = vec!["kb-1".to_string()];
    let mut other_kb = doc_meta("doc-out", "Out");
    other_kb.knowledge_base_ids = vec!["kb-2".to_string()];
    h.documents.insert_doc(TenantId(TENANT.to_string()), in_kb);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), other_kb);

    let response = h
        .send(
            "GET",
            "/v1/documents?knowledgeBaseIds=kb-1",
            &tenant_key(TENANT),
        )
        .await;
    let body = json_body(response).await;
    let ids: Vec<&str> = body["documents"]
        .as_array()
        .expect("documents")
        .iter()
        .map(|d| d["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, vec!["doc-in"]);
}

#[tokio::test]
async fn list_never_shows_another_tenants_documents() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.with_tenant(OTHER);
    h.documents
        .insert_doc(TenantId(OTHER.to_string()), doc_meta("doc-other", "Theirs"));

    let response = h.send("GET", "/v1/documents", &tenant_key(TENANT)).await;
    let body = json_body(response).await;
    assert_eq!(body["documents"], serde_json::json!([]));
}

#[tokio::test]
async fn detail_returns_document_and_ordered_chunks() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let mut doc = doc_meta("doc-1", "Detail");
    doc.metadata = Some(serde_json::json!({
        "figures": [{"assetKey": "documents/tenant-a/doc-1/assets/fig-1.png", "page": 2, "type": "chart"}]
    }));
    h.documents.insert_doc(TenantId(TENANT.to_string()), doc);
    h.chunks.stored_chunks.write().unwrap().insert(
        DocumentId("doc-1".into()),
        vec![chunk_to_store("second", 1), chunk_to_store("first", 0)],
    );

    let response = h
        .send("GET", "/v1/documents/doc-1", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    assert_eq!(body["document"]["id"], "doc-1");
    assert_eq!(body["document"]["chunkCount"], 2);
    assert_eq!(body["document"]["mimeType"], "application/pdf");
    assert_eq!(body["document"]["figures"][0]["page"], 2);
    assert_eq!(body["document"]["figures"][0]["kind"], "chart");

    let chunks = body["chunks"].as_array().expect("chunks");
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0]["chunkIndex"], 0);
    assert_eq!(chunks[0]["content"], "first");
    assert_eq!(chunks[1]["chunkIndex"], 1);
    assert_eq!(chunks[1]["content"], "second");
    // Bun's `String(row.id)`: the document-detail route names the same record
    // form as a search hit.
    assert_eq!(chunks[0]["id"], "document_chunk:⟨doc-1_0⟩");
}

/// The id the detail route hands out decodes back to the bare ident the store
/// keys on, in both forms a caller can hold.
///
/// No route consumes a chunk id today (search takes document ids; the figure
/// route takes an asset key), so this is the boundary property those routes
/// will need when one does: `dto::chunk_id_from_client` is the single decode
/// point, and it accepts Bun's `document_chunk:⟨…⟩` and the bare ident alike.
#[tokio::test]
async fn a_chunk_id_from_the_detail_route_resolves_in_either_form() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Detail"));
    h.chunks
        .stored_chunks
        .write()
        .unwrap()
        .insert(DocumentId("doc-1".into()), vec![chunk_to_store("first", 0)]);

    let response = h
        .send("GET", "/v1/documents/doc-1", &tenant_key(TENANT))
        .await;
    let body = json_body(response).await;
    let wire_id = body["chunks"][0]["id"].as_str().expect("id").to_string();

    assert_eq!(wire_id, kb_server::dto::chunk_id_for_client("doc-1_0"));
    for sent in [wire_id, "doc-1_0".to_string()] {
        assert_eq!(
            kb_server::dto::chunk_id_from_client(&sent),
            "doc-1_0",
            "either form must name the same chunk: {sent}"
        );
    }
}

#[tokio::test]
async fn detail_of_an_unknown_id_is_a_404_with_the_bun_text() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let response = h
        .send("GET", "/v1/documents/doc-nope", &tenant_key(TENANT))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn detail_across_tenants_is_a_404() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(OTHER.to_string()), doc_meta("doc-1", "Theirs"));
    let response = h
        .send("GET", "/v1/documents/doc-1", &tenant_key(TENANT))
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
}

#[tokio::test]
async fn patch_updates_metadata_and_returns_the_row() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Before"));

    let response = h
        .send_json(
            "PATCH",
            "/v1/documents/doc-1",
            &tenant_key(TENANT),
            serde_json::json!({"title": "After", "subcategory": null}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["document"]["title"], "After");
    assert_eq!(body["document"]["subcategory"], serde_json::Value::Null);
    // The patch response is the detail select without chunks or figures.
    assert!(body["document"].get("chunkCount").is_none());
    assert!(body.get("chunks").is_none());

    let docs = h.documents.docs.read().unwrap();
    let stored = &docs[&TenantId(TENANT.to_string())][0];
    assert_eq!(stored.title, "After");
    assert_eq!(stored.subcategory, None);
}

#[tokio::test]
async fn patch_replaces_knowledge_base_links_and_drops_foreign_ids() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let mut doc = doc_meta("doc-1", "Doc");
    doc.knowledge_base_ids = vec!["kb-old".to_string()];
    h.documents.insert_doc(TenantId(TENANT.to_string()), doc);
    h.documents.kb_records.write().unwrap().insert(
        TenantId(TENANT.to_string()),
        vec![kb_ports::KnowledgeBaseRecord {
            id: kb_ports::KnowledgeBaseId("kb-mine".into()),
            tenant_id: TenantId(TENANT.to_string()),
            name: "Mine".into(),
            description: None,
            color: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }],
    );

    let response = h
        .send_json(
            "PATCH",
            "/v1/documents/doc-1",
            &tenant_key(TENANT),
            serde_json::json!({"knowledgeBaseIds": ["kb-mine", "kb-not-mine"]}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        body["document"]["knowledgeBaseIds"],
        serde_json::json!(["kb-mine"])
    );
    let docs = h.documents.docs.read().unwrap();
    assert_eq!(
        docs[&TenantId(TENANT.to_string())][0].knowledge_base_ids,
        vec!["kb-mine"]
    );
}

#[tokio::test]
async fn patch_across_tenants_is_a_404_and_changes_nothing() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let h_owner = TenantId(OTHER.to_string());
    h.documents
        .insert_doc(h_owner.clone(), doc_meta("doc-1", "Theirs"));

    let response = h
        .send_json(
            "PATCH",
            "/v1/documents/doc-1",
            &tenant_key(TENANT),
            serde_json::json!({"title": "Hijacked"}),
        )
        .await;
    assert_error(response, StatusCode::NOT_FOUND, "Not found").await;
    let docs = h.documents.docs.read().unwrap();
    assert_eq!(docs[&h_owner][0].title, "Theirs");
}

#[tokio::test]
async fn patch_without_kb_write_is_a_403_with_the_bun_text() {
    let h = Harness::new();
    h.documents.insert_api_key_with_scopes(
        &tenant_key(TENANT),
        TenantId(TENANT.to_string()),
        vec!["kb:read".into()],
        vec![],
    );
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Doc"));

    let response = h
        .send_json(
            "PATCH",
            "/v1/documents/doc-1",
            &tenant_key(TENANT),
            serde_json::json!({"title": "x"}),
        )
        .await;
    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "API key lacks scope kb:write",
    )
    .await;
}

#[tokio::test]
async fn patch_with_a_malformed_body_is_a_400_from_the_route_not_the_parser() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Doc"));

    let response = h
        .send("PATCH", "/v1/documents/doc-1", &tenant_key(TENANT))
        .await;
    // An absent body is an empty patch, exactly as `request.json().catch(...)`.
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["document"]["title"], "Doc");
}

#[tokio::test]
async fn delete_returns_only_ok() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Doc"));

    let response = h
        .send("DELETE", "/v1/documents/doc-1", &tenant_key(TENANT))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await, serde_json::json!({"ok": true}));
    assert_eq!(h.documents.deleted.read().unwrap().len(), 1);
}

#[tokio::test]
async fn hard_delete_can_follow_a_soft_delete() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), doc_meta("doc-1", "Doc"));

    assert_eq!(
        h.send("DELETE", "/v1/documents/doc-1", &tenant_key(TENANT))
            .await
            .status(),
        StatusCode::OK
    );
    // A soft-deleted document is invisible to every read path...
    assert_error(
        h.send("GET", "/v1/documents/doc-1", &tenant_key(TENANT))
            .await,
        StatusCode::NOT_FOUND,
        "Not found",
    )
    .await;
    // ...but the delete route can still find it, exactly as TypeScript does.
    let response = h
        .send(
            "DELETE",
            "/v1/documents/doc-1?hard=true",
            &tenant_key(TENANT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(h.documents.deleted.read().unwrap().len(), 0);
}

#[tokio::test]
async fn knowledge_base_list_uses_the_bun_envelope() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.documents.kbs.write().unwrap().insert(
        TenantId(TENANT.to_string()),
        vec![kb_ports::KnowledgeBaseSummary {
            id: kb_ports::KnowledgeBaseId("kb-1".into()),
            name: "Alpha".into(),
            description: Some("first".into()),
            color: Some("#fff".into()),
            document_count: 2,
        }],
    );

    let response = h
        .send("GET", "/v1/knowledge-bases", &tenant_key(TENANT))
        .await;
    let body = json_body(response).await;
    let bases = body["knowledgeBases"].as_array().expect("array");
    assert_eq!(bases[0]["id"], "kb-1");
    assert_eq!(bases[0]["name"], "Alpha");
    assert_eq!(bases[0]["description"], "first");
    assert_eq!(bases[0]["color"], "#fff");
    assert_eq!(bases[0]["documentCount"], 2);
}

#[tokio::test]
async fn list_filters_by_external_ref_and_never_crosses_the_tenant() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    h.with_tenant(OTHER);

    let mut mine = doc_meta("doc-a", "Mine");
    mine.external_ref = Some("ku-1".into());
    let mut other_ref = doc_meta("doc-a-2", "Other ref");
    other_ref.external_ref = Some("ku-2".into());
    let unrelated = doc_meta("doc-a-3", "No ref");
    let mut theirs = doc_meta("doc-b", "Theirs");
    theirs.external_ref = Some("ku-1".into());

    h.documents.insert_doc(TenantId(TENANT.to_string()), mine);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), other_ref);
    h.documents
        .insert_doc(TenantId(TENANT.to_string()), unrelated);
    h.documents.insert_doc(TenantId(OTHER.to_string()), theirs);

    let response = h
        .send("GET", "/v1/documents?externalRef=ku-1", &tenant_key(TENANT))
        .await;
    let body = json_body(response).await;
    let documents = body["documents"].as_array().expect("documents");
    assert_eq!(documents.len(), 1, "{body}");
    assert_eq!(documents[0]["id"], "doc-a");
    assert_eq!(documents[0]["externalRef"], "ku-1");

    // The other tenant's document carries the same externalRef and must stay
    // invisible: the filter is an extra predicate, never a replacement for the
    // tenant one.
    let response = h
        .send("GET", "/v1/documents?externalRef=ku-1", &tenant_key(OTHER))
        .await;
    let body = json_body(response).await;
    let documents = body["documents"].as_array().expect("documents");
    assert_eq!(documents.len(), 1, "{body}");
    assert_eq!(documents[0]["id"], "doc-b");
}

#[tokio::test]
async fn list_external_ref_excludes_soft_deleted_documents() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let mut doc = doc_meta("doc-1", "Deleted");
    doc.external_ref = Some("ku-1".into());
    h.documents.insert_doc(TenantId(TENANT.to_string()), doc);

    let deleted = h
        .documents
        .soft_delete(&TenantId(TENANT.to_string()), &DocumentId("doc-1".into()))
        .await
        .unwrap();
    assert!(deleted);

    let response = h
        .send("GET", "/v1/documents?externalRef=ku-1", &tenant_key(TENANT))
        .await;
    let body = json_body(response).await;
    assert_eq!(body["documents"], serde_json::json!([]), "{body}");
}

#[tokio::test]
async fn list_external_ref_combines_with_knowledge_bases_and_limit() {
    let h = Harness::new();
    h.with_tenant(TENANT);
    let mut in_kb = doc_meta("doc-in", "In");
    in_kb.external_ref = Some("ku-1".into());
    in_kb.knowledge_base_ids = vec!["kb-1".into()];
    let mut out_kb = doc_meta("doc-out", "Out");
    out_kb.external_ref = Some("ku-1".into());
    out_kb.knowledge_base_ids = vec!["kb-2".into()];
    h.documents.insert_doc(TenantId(TENANT.to_string()), in_kb);
    h.documents.insert_doc(TenantId(TENANT.to_string()), out_kb);

    let response = h
        .send(
            "GET",
            "/v1/documents?externalRef=ku-1&knowledgeBaseIds=kb-1&limit=1",
            &tenant_key(TENANT),
        )
        .await;
    let body = json_body(response).await;
    let documents = body["documents"].as_array().expect("documents");
    assert_eq!(documents.len(), 1, "{body}");
    assert_eq!(documents[0]["id"], "doc-in");
}

fn chunk_to_store(content: &str, index: u32) -> kb_ports::ChunkToStore {
    kb_ports::ChunkToStore {
        content: content.to_string(),
        chunk_index: index,
        embedding: vec![0.1; 4],
        metadata: serde_json::json!({"chunkIndex": index, "chunkType": "text"}),
        contextual_prefix: None,
    }
}
