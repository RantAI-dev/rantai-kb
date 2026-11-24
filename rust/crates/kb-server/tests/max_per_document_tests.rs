//! `RET-17`: `maxPerDocument` over the real router.
//!
//! The engine unit tests pin the cap inside the pipeline; these tests pin the
//! HTTP contract: the additive field, the 400 on an out-of-range value, and
//! the additive `stats.perDocumentCapped` count.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use kb_engine::fakes::make_fake_hit;
use kb_ports::{DocumentMeta, TenantId};

use harness::{Harness, doc_meta, json_body, tenant_key};

struct Fixture {
    harness: Harness,
    key: String,
}

impl Fixture {
    fn new() -> Self {
        let harness = Harness::new();
        let tenant = TenantId("tenant-1".into());
        let key = tenant_key("tenant-1");
        harness.documents.insert_api_key(&key, tenant.clone());
        for id in ["A", "B"] {
            let meta: DocumentMeta = doc_meta(id, &format!("Title {id}"));
            harness.documents.insert_doc(tenant.clone(), meta);
        }
        // Ten hits from A, two from B, in descending similarity so the fused
        // (and reranked) order is a1..a10, b1, b2.
        for i in 1..=10u32 {
            harness
                .chunks
                .vector_hits
                .write()
                .unwrap()
                .push(make_fake_hit(
                    &format!("a{i}"),
                    "A",
                    "Title A",
                    "vendor A text",
                    i,
                    1.0 - i as f32 * 0.01,
                ));
        }
        for (i, sim) in [0.89f32, 0.88].into_iter().enumerate() {
            harness
                .chunks
                .vector_hits
                .write()
                .unwrap()
                .push(make_fake_hit(
                    &format!("b{}", i + 1),
                    "B",
                    "Title B",
                    "vendor B text",
                    11 + i as u32,
                    sim,
                ));
        }
        Self { harness, key }
    }

    async fn search(&self, body: serde_json::Value) -> axum::response::Response {
        self.harness
            .send_json("POST", "/v1/search", &self.key, body)
            .await
    }
}

fn chunk_ids(body: &serde_json::Value) -> Vec<String> {
    body["chunks"]
        .as_array()
        .unwrap()
        .iter()
        // The DTO exposes Bun-shaped ids (`document_chunk:⟨a1⟩`); the test
        // seeds bare ids, so strip the envelope before comparing.
        .map(|c| {
            c["id"]
                .as_str()
                .unwrap()
                .trim_start_matches("document_chunk:⟨")
                .trim_end_matches('⟩')
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn max_per_document_caps_each_document_and_counts_the_drops() {
    let fixture = Fixture::new();
    let response = fixture
        .search(serde_json::json!({
            "query": "vendor comparison",
            "maxChunks": 4,
            "maxPerDocument": 2
        }))
        .await;
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(
        chunk_ids(&body),
        ["a1", "a2", "b1", "b2"],
        "the cap keeps each document's first hits in rerank order"
    );
    assert_eq!(
        body["stats"]["perDocumentCapped"], 8,
        "ten A hits plus two B hits, capped at two each: eight dropped"
    );
}

#[tokio::test]
async fn without_the_field_the_answer_is_unchanged_and_the_stat_is_absent() {
    let fixture = Fixture::new();
    let response = fixture
        .search(serde_json::json!({"query": "vendor comparison", "maxChunks": 4}))
        .await;
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(chunk_ids(&body), ["a1", "a2", "a3", "a4"]);
    assert!(
        body["stats"].get("perDocumentCapped").is_none(),
        "an old client sees exactly the earlier stats shape"
    );
}

#[tokio::test]
async fn a_cap_of_one_keeps_one_chunk_per_document() {
    let fixture = Fixture::new();
    let response = fixture
        .search(serde_json::json!({
            "query": "vendor comparison",
            "maxChunks": 4,
            "maxPerDocument": 1
        }))
        .await;
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(chunk_ids(&body), ["a1", "b1"]);
    assert_eq!(body["stats"]["perDocumentCapped"], 10);
}

#[tokio::test]
async fn zero_and_fifty_one_are_bad_requests() {
    let fixture = Fixture::new();
    for bad in [0, 51] {
        let response = fixture
            .search(serde_json::json!({
                "query": "vendor comparison",
                "maxPerDocument": bad
            }))
            .await;
        assert_eq!(response.status(), 400, "maxPerDocument={bad}");
        let body = json_body(response).await;
        assert!(
            body.to_string().contains("maxPerDocument"),
            "the error names the field it rejected: {body}"
        );
    }
}
