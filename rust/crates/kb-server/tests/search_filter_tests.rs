//! `RET-3`: metadata filters narrow the scope before every retrieval arm ranks.
//!
//! The instrument is the real router over the in-memory index: a document the
//! filter excludes is seeded into every arm (dense, BM25, neighbour, figure),
//! and the response must never name it. A post-filter could still pass a
//! single-arm test, so each test seeds both facet documents with hits in every
//! arm and asserts the returned document ids, not just the top one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use kb_engine::fakes::make_fake_hit;
use kb_ports::{DocumentMeta, KnowledgeBaseId, TenantId};

use harness::{Harness, doc_meta, json_body, tenant_key};

struct Fixture {
    harness: Harness,
    tenant: TenantId,
}

impl Fixture {
    /// Three documents:
    /// * `doc-a`: kb-a, externalRef `ref-a`, language `en`, `topic = "handbook"`;
    /// * `doc-b`: kb-b, externalRef `ref-b`, language `en`, `topic = "handbook"`;
    /// * `doc-c`: kb-a, externalRef `ref-c`, language `id`, `topics = ["alpha", "beta"]`.
    fn new() -> Self {
        let harness = Harness::new();
        let tenant = TenantId("tenant-1".into());
        harness
            .documents
            .insert_api_key(&tenant_key("tenant-1"), tenant.clone());

        Self::insert(
            &harness,
            &tenant,
            "doc-a",
            "Document A",
            "kb-a",
            "ref-a",
            serde_json::json!({"language": "en", "topic": "handbook"}),
        );
        Self::insert(
            &harness,
            &tenant,
            "doc-b",
            "Document B",
            "kb-b",
            "ref-b",
            serde_json::json!({"language": "en", "topic": "handbook"}),
        );
        Self::insert(
            &harness,
            &tenant,
            "doc-c",
            "Document C",
            "kb-a",
            "ref-c",
            serde_json::json!({"language": "id", "topics": ["alpha", "beta"]}),
        );

        for id in ["doc-a", "doc-b", "doc-c"] {
            Self::seed_arms(&harness, id);
        }

        Self { harness, tenant }
    }

    fn insert(
        harness: &Harness,
        tenant: &TenantId,
        id: &str,
        title: &str,
        kb: &str,
        external_ref: &str,
        metadata: serde_json::Value,
    ) {
        let mut meta: DocumentMeta = doc_meta(id, title);
        meta.knowledge_base_ids = vec![kb.to_string()];
        meta.external_ref = Some(external_ref.to_string());
        meta.categories = vec!["cat-1".to_string()];
        meta.metadata = Some(metadata);
        harness.documents.insert_doc(tenant.clone(), meta);
    }

    /// Every arm is pointed at every document. If any arm ignores the scope,
    /// the excluded document's id comes back and the test fails.
    fn seed_arms(harness: &Harness, id: &str) {
        let chunk = |suffix: &str| format!("{id}-{suffix}");
        harness
            .chunks
            .vector_hits
            .write()
            .unwrap()
            .push(make_fake_hit(
                &chunk("dense"),
                id,
                id,
                "shared body text",
                0,
                0.9,
            ));
        harness
            .chunks
            .bm25_hits
            .write()
            .unwrap()
            .push(make_fake_hit(
                &chunk("sparse"),
                id,
                id,
                "shared keyword text",
                1,
                0.9,
            ));
        harness
            .chunks
            .neighbour_hits
            .write()
            .unwrap()
            .push(make_fake_hit(
                &chunk("neighbour"),
                id,
                id,
                "neighbour text",
                2,
                0.5,
            ));
        let mut figure = make_fake_hit(&chunk("figure"), id, id, "a figure caption", 3, 0.5);
        figure.chunk.metadata.asset_key = Some(format!("documents/{id}/assets/1.png"));
        figure.chunk.chunk_type = kb_ports::ChunkType::Figure;
        harness.chunks.figure_hits.write().unwrap().push(figure);
    }

    fn auth_key(&self) -> String {
        tenant_key("tenant-1")
    }

    fn bind_key_to_kbs(&self, kbs: &[&str]) {
        self.harness.documents.insert_api_key_with_scopes(
            &self.auth_key(),
            self.tenant.clone(),
            vec!["kb:read".into(), "kb:write".into()],
            kbs.iter()
                .map(|kb| KnowledgeBaseId((*kb).to_string()))
                .collect(),
        );
    }

    async fn search_ok(&self, body: serde_json::Value) -> Vec<String> {
        let response = self
            .harness
            .send_json("POST", "/v1/search", &self.auth_key(), body.clone())
            .await;
        assert_eq!(response.status(), 200, "{body}");
        let body = json_body(response).await;
        returned_documents(&body)
    }
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

#[tokio::test]
async fn external_refs_returns_only_named_documents_from_every_arm() {
    let fixture = Fixture::new();
    let found = fixture
        .search_ok(serde_json::json!({"query": "shared", "externalRefs": ["ref-a"]}))
        .await;
    assert_eq!(found, vec!["doc-a"], "every arm must exclude B and C");
}

#[tokio::test]
async fn an_empty_external_refs_list_selects_nothing() {
    let fixture = Fixture::new();
    let found = fixture
        .search_ok(serde_json::json!({"query": "shared", "externalRefs": []}))
        .await;
    assert!(found.is_empty(), "{found:?}");
}

#[tokio::test]
async fn language_matches_the_metadata_language_key() {
    let fixture = Fixture::new();
    let found = fixture
        .search_ok(serde_json::json!({"query": "shared", "language": "en"}))
        .await;
    assert_eq!(found, vec!["doc-a", "doc-b"]);
}

#[tokio::test]
async fn metadata_scalar_equality_excludes_documents_that_do_not_match() {
    let fixture = Fixture::new();
    let found = fixture
        .search_ok(serde_json::json!({"query": "shared", "metadata": {"topic": "handbook"}}))
        .await;
    assert_eq!(found, vec!["doc-a", "doc-b"]);
}

#[tokio::test]
async fn a_metadata_array_value_matches_any_of_its_values() {
    let fixture = Fixture::new();
    let found = fixture
        .search_ok(
            serde_json::json!({"query": "shared", "metadata": {"topics": ["gamma", "beta"]}}),
        )
        .await;
    assert_eq!(found, vec!["doc-c"]);
}

#[tokio::test]
async fn metadata_and_language_intersect() {
    let fixture = Fixture::new();
    let found = fixture
        .search_ok(serde_json::json!({
            "query": "shared",
            "language": "en",
            "metadata": {"topic": "handbook"},
            "externalRefs": ["ref-a", "ref-c"],
        }))
        .await;
    assert_eq!(found, vec!["doc-a"]);
}

#[tokio::test]
async fn a_key_limited_to_a_kb_plus_a_metadata_filter_is_the_intersection() {
    let fixture = Fixture::new();
    fixture.bind_key_to_kbs(&["kb-a"]);

    // The request asks for both bases, but the key may only see kb-a. The
    // language filter then keeps A and drops C: narrow twice, never widen.
    let found = fixture
        .search_ok(serde_json::json!({
            "query": "shared",
            "knowledgeBaseIds": ["kb-a", "kb-b"],
            "language": "en",
        }))
        .await;
    assert_eq!(found, vec!["doc-a"]);
}

#[tokio::test]
async fn document_ids_cannot_widen_past_the_keys_kb_limit() {
    let fixture = Fixture::new();
    fixture.bind_key_to_kbs(&["kb-a"]);

    let found = fixture
        .search_ok(serde_json::json!({"query": "shared", "documentIds": ["doc-b"]}))
        .await;
    assert!(
        found.is_empty(),
        "documentIds must not escape the key's KBs: {found:?}"
    );
}

#[tokio::test]
async fn document_ids_intersect_metadata_filters() {
    let fixture = Fixture::new();
    let found = fixture
        .search_ok(serde_json::json!({
            "query": "shared",
            "documentIds": ["doc-a", "doc-c"],
            "language": "en",
        }))
        .await;
    assert_eq!(found, vec!["doc-a"]);
}

#[tokio::test]
async fn invalid_metadata_keys_are_a_400() {
    let fixture = Fixture::new();
    for key in ["", "has space", "slash/key", &"x".repeat(65)] {
        let body = serde_json::json!({"query": "shared", "metadata": {key: "v"}});
        let response = fixture
            .harness
            .send_json("POST", "/v1/search", &fixture.auth_key(), body.clone())
            .await;
        assert_eq!(response.status(), 400, "key {key:?} in {body}");
        let error = json_body(response).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(error.contains("Invalid metadata key"), "{error}");
    }
}

#[tokio::test]
async fn non_scalar_metadata_values_are_a_400() {
    let fixture = Fixture::new();
    for value in [
        serde_json::json!({"nested": "object"}),
        serde_json::json!([["nested"]]),
        serde_json::json!(null),
    ] {
        let body = serde_json::json!({"query": "shared", "metadata": {"k": value}});
        let response = fixture
            .harness
            .send_json("POST", "/v1/search", &fixture.auth_key(), body.clone())
            .await;
        assert_eq!(response.status(), 400, "value {body}");
        let error = json_body(response).await["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            error.contains("Invalid value for metadata key 'k'"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn a_non_object_metadata_field_is_a_400() {
    let fixture = Fixture::new();
    let response = fixture
        .harness
        .send_json(
            "POST",
            "/v1/search",
            &fixture.auth_key(),
            serde_json::json!({"query": "shared", "metadata": ["topic"]}),
        )
        .await;
    assert_eq!(response.status(), 400);
    let error = json_body(response).await["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("must be an object"), "{error}");
}

/// The filter is resolved into the scope, so an arm cannot see an excluded
/// document even when every arm holds a hit for it. This pins the response to
/// the allowed set for the dense, sparse, neighbour and figure arms at once.
#[tokio::test]
async fn no_arm_returns_an_excluded_document() {
    let fixture = Fixture::new();
    for (filter, expected) in [
        (
            serde_json::json!({"externalRefs": ["ref-a"]}),
            vec!["doc-a"],
        ),
        (serde_json::json!({"language": "id"}), vec!["doc-c"]),
        (
            serde_json::json!({"metadata": {"topics": ["alpha"]}}),
            vec!["doc-c"],
        ),
        (
            serde_json::json!({"metadata": {"topic": "handbook"}}),
            vec!["doc-a", "doc-b"],
        ),
    ] {
        let mut body = filter;
        body["query"] = serde_json::json!("shared");
        let found = fixture.search_ok(body.clone()).await;
        assert_eq!(
            found, expected,
            "filter {body} let an excluded document through"
        );
    }
}
