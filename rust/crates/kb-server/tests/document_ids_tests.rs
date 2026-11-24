//! RET-11: `documentIds` runs the full pipeline, inside a scope that can only
//! narrow what the key can already see.
//!
//! The instrument is the real router over the in-process fakes. The fake index
//! answers an arm only when the caller's scope contains the hit's document, so
//! these tests can tell "the arm ran" from "the arm was scoped": a keyword-only
//! hit inside A must be found, while the same kind of hit in B must never be
//! returned when the caller asked for A alone.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use kb_engine::fakes::make_fake_hit;
use kb_ports::{KnowledgeBaseId, TenantId};

use harness::{Harness, doc_meta, json_body, tenant_key};

const KEYWORD: &str = "zxqvortex";

fn seed_doc(harness: &Harness, tenant: &str, id: &str, kb: &str) {
    let mut meta = doc_meta(id, &format!("Doc {id}"));
    meta.knowledge_base_ids = vec![kb.to_string()];
    harness
        .documents
        .insert_doc(TenantId(tenant.to_string()), meta);
}

/// A dense hit, for the arms that existed on the old by-id branch.
fn seed_vector(harness: &Harness, id: &str, content: &str) {
    harness
        .chunks
        .vector_hits
        .write()
        .unwrap()
        .push(make_fake_hit(
            &format!("{id}-dense"),
            id,
            id,
            content,
            0,
            0.9,
        ));
}

/// A keyword-only hit: the fake index returns it iff the search runs BM25.
fn seed_keyword(harness: &Harness, id: &str, content: &str) {
    harness
        .chunks
        .bm25_hits
        .write()
        .unwrap()
        .push(make_fake_hit(
            &format!("{id}-keyword"),
            id,
            id,
            content,
            1,
            0.9,
        ));
}

fn bind_key(harness: &Harness, key: &str, tenant: &str, kbs: &[&str]) {
    harness.documents.insert_api_key_with_scopes(
        key,
        TenantId(tenant.to_string()),
        vec!["kb:read".to_string(), "kb:write".to_string()],
        kbs.iter()
            .map(|kb| KnowledgeBaseId((*kb).to_string()))
            .collect(),
    );
}

async fn search(harness: &Harness, key: &str, document_ids: &[&str]) -> serde_json::Value {
    let body = serde_json::json!({
        "query": KEYWORD,
        "documentIds": document_ids,
    });
    let response = harness
        .send_json("POST", "/v1/search", key, body.clone())
        .await;
    assert_eq!(response.status(), 200, "{body}");
    json_body(response).await
}

fn chunks(body: &serde_json::Value) -> &[serde_json::Value] {
    body["chunks"].as_array().map_or(&[], Vec::as_slice)
}

fn documents_in(body: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = chunks(body)
        .iter()
        .filter_map(|c| c["documentId"].as_str().map(str::to_string))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// T1: a keyword hit in a sibling document must stay out of a `documentIds`
/// search even though the BM25 arm now runs on this branch.
#[tokio::test]
async fn a_keyword_hit_in_a_sibling_document_is_not_returned() {
    let harness = Harness::new();
    harness.with_tenant("tenant-a");
    seed_doc(&harness, "tenant-a", "doc-a", "kb-1");
    seed_doc(&harness, "tenant-a", "doc-b", "kb-1");
    seed_vector(&harness, "doc-a", "the handbook about leave");
    seed_keyword(
        &harness,
        "doc-b",
        &format!("the rare term {KEYWORD} appears here"),
    );
    seed_vector(
        &harness,
        "doc-b",
        &format!("a dense hit with {KEYWORD} in it"),
    );

    let answer = search(&harness, &tenant_key("tenant-a"), &["doc-a"]).await;
    assert_eq!(documents_in(&answer), vec!["doc-a"], "{answer}");
    let rendered = answer.to_string();
    assert!(!rendered.contains(KEYWORD), "B's keyword leaked: {answer}");
}

/// T2: the requested id belongs to another tenant. It is not alive for this
/// key's tenant, so the branch answers empty before any arm runs.
#[tokio::test]
async fn another_tenants_document_is_not_searchable_by_id() {
    let harness = Harness::new();
    harness.with_tenant("tenant-a");
    harness.with_tenant("tenant-b");
    seed_doc(&harness, "tenant-a", "doc-a", "kb-1");
    seed_doc(&harness, "tenant-b", "doc-theirs", "kb-1");
    seed_vector(&harness, "doc-theirs", "a foreign document");
    seed_keyword(&harness, "doc-theirs", KEYWORD);

    let answer = search(&harness, &tenant_key("tenant-a"), &["doc-theirs"]).await;
    assert!(chunks(&answer).is_empty(), "{answer}");
    assert_eq!(answer["context"], "", "{answer}");
}

/// T3: the key may only see KB-1, and the named document is in KB-2. The full
/// pipeline inherits the key's scope and cannot widen it.
#[tokio::test]
async fn a_kb_limited_key_cannot_reach_a_document_in_another_kb() {
    let harness = Harness::new();
    bind_key(&harness, &tenant_key("tenant-a"), "tenant-a", &["kb-1"]);
    seed_doc(&harness, "tenant-a", "doc-in", "kb-1");
    seed_doc(&harness, "tenant-a", "doc-out", "kb-2");
    seed_vector(&harness, "doc-out", KEYWORD);
    seed_keyword(&harness, "doc-out", KEYWORD);

    let answer = search(&harness, &tenant_key("tenant-a"), &["doc-out"]).await;
    assert!(chunks(&answer).is_empty(), "{answer}");
}

/// T4: a keyword-only match inside the requested document. The old by-id
/// branch ran dense only, so this hit was invisible. The full pipeline also
/// attaches sources and the standard context.
#[tokio::test]
async fn a_keyword_only_match_in_the_requested_document_is_found() {
    let harness = Harness::new();
    harness.with_tenant("tenant-a");
    seed_doc(&harness, "tenant-a", "doc-a", "kb-1");
    seed_keyword(
        &harness,
        "doc-a",
        &format!("the only place with {KEYWORD} is this chunk"),
    );

    let answer = search(&harness, &tenant_key("tenant-a"), &["doc-a"]).await;
    let found: Vec<&str> = chunks(&answer)
        .iter()
        .filter_map(|c| c["content"].as_str())
        .collect();
    assert!(
        found.iter().any(|content| content.contains(KEYWORD)),
        "the BM25 arm did not find the keyword-only chunk: {answer}"
    );
    assert_eq!(documents_in(&answer), vec!["doc-a"], "{answer}");
    assert!(
        answer["sources"].as_array().is_some_and(|s| !s.is_empty()),
        "the full pipeline must attach sources: {answer}"
    );
    let context = answer["context"].as_str().unwrap_or_default();
    assert!(context.contains("[1]"), "standard context, got {context:?}");
    assert!(
        context.contains(KEYWORD),
        "the context must carry the hit: {context:?}"
    );
}
