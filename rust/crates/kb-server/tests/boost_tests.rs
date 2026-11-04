//! `RET-4`: the optional recency/authority boost over the real router.
//!
//! The body is additive: absent means off, and an off request must produce the
//! same chunk order and no `stats.boost` member. Invalid shapes are this
//! route's own 400s. The engine's arithmetic is unit-tested in `kb-core` and
//! `kb-engine`; these tests pin the wire contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use harness::{Harness, doc_meta, json_body, tenant_key};
use kb_ports::{DocumentMeta, KnowledgeBaseId, TenantId};

const TENANT: &str = "tenant-1";

fn meta(id: &str, effective_date: Option<&str>, authority: Option<f64>) -> DocumentMeta {
    let mut doc = doc_meta(id, &format!("Title {id}"));
    let mut metadata = serde_json::Map::new();
    if let Some(date) = effective_date {
        metadata.insert("effectiveDate".into(), serde_json::json!(date));
    }
    if let Some(value) = authority {
        metadata.insert("authority".into(), serde_json::json!(value));
    }
    doc.metadata = (!metadata.is_empty()).then_some(serde_json::Value::Object(metadata));
    doc
}

/// Two dated documents, seeded into the dense arm in `pre_boost` order with
/// descending similarity so the arm's order is predictable.
fn fixture(docs: Vec<DocumentMeta>, pre_boost: &[&str]) -> Harness {
    let harness = Harness::new();
    let tenant = TenantId(TENANT.into());
    harness.documents.insert_api_key(&tenant_key(TENANT), tenant.clone());
    for doc in docs {
        harness.documents.insert_doc(tenant.clone(), doc);
    }
    for (index, id) in pre_boost.iter().enumerate() {
        harness
            .chunks
            .vector_hits
            .write()
            .unwrap()
            .push(kb_engine::fakes::make_fake_hit(
                &format!("{id}_0"),
                id,
                &format!("Title {id}"),
                &format!("Body {id}"),
                0,
                0.9 - index as f32 * 0.1,
            ));
    }
    harness
}

fn doc_ids(body: &serde_json::Value) -> Vec<String> {
    body["chunks"]
        .as_array()
        .map(|chunks| {
            chunks
                .iter()
                .filter_map(|c| c["documentId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn recency_boost(field: &str, half_life_days: f64, weight: f64) -> serde_json::Value {
    serde_json::json!({
        "query": "current allowance",
        "boost": {
            "recency": {
                "field": field,
                "halfLifeDays": half_life_days,
                "weight": weight,
            }
        }
    })
}

#[tokio::test]
async fn a_boost_absent_and_an_empty_boost_rank_identically() {
    let documents = || {
        vec![
            meta("old", Some("2020-01-01"), None),
            meta("new", Some("2025-05-31"), None),
        ]
    };
    let absent = fixture(documents(), &["old", "new"]);
    let empty = fixture(documents(), &["old", "new"]);

    let without = absent
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key(TENANT),
            serde_json::json!({"query": "current allowance"}),
        )
        .await;
    assert_eq!(without.status(), StatusCode::OK);
    let without = json_body(without).await;

    let with_empty = empty
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key(TENANT),
            serde_json::json!({"query": "current allowance", "boost": {}}),
        )
        .await;
    assert_eq!(with_empty.status(), StatusCode::OK);
    let with_empty = json_body(with_empty).await;

    assert_eq!(doc_ids(&without), vec!["old", "new"]);
    assert_eq!(doc_ids(&with_empty), doc_ids(&without));
    assert!(
        without["stats"].get("boost").is_none(),
        "an off request must not grow a stats.boost member: {}",
        without["stats"]
    );
    assert!(with_empty["stats"].get("boost").is_none());
}

#[tokio::test]
async fn recency_promotes_the_newer_document_and_reports_the_count() {
    let harness = fixture(
        vec![
            meta("old", Some("2023-01-01"), None),
            meta("new", Some("2025-05-31"), None),
        ],
        &["old", "new"],
    );

    let response = harness
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key(TENANT),
            serde_json::json!({
                "query": "current allowance",
                "boost": {
                    "recency": {
                        "field": "metadata.effectiveDate",
                        "halfLifeDays": 365,
                        "weight": 1.0
                    }
                }
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(doc_ids(&body), vec!["new", "old"]);
    assert_eq!(body["stats"]["boost"]["recency"], 2);
    assert_eq!(body["stats"]["boost"]["authority"], 0);
}

#[tokio::test]
async fn authority_promotes_the_higher_value() {
    let harness = fixture(
        vec![
            meta("low", None, Some(0.1)),
            meta("high", None, Some(0.9)),
        ],
        &["low", "high"],
    );

    let response = harness
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key(TENANT),
            serde_json::json!({
                "query": "source policy",
                "boost": {
                    "authority": {"field": "metadata.authority", "weight": 1.0}
                }
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(doc_ids(&body), vec!["high", "low"]);
    assert_eq!(body["stats"]["boost"]["authority"], 2);
}

#[tokio::test]
async fn a_document_without_the_field_keeps_its_place() {
    let harness = fixture(
        vec![
            meta("undated", None, None),
            meta("old", Some("2020-01-01"), None),
        ],
        &["undated", "old"],
    );

    let response = harness
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key(TENANT),
            recency_boost("metadata.effectiveDate", 30.0, 1.0),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(doc_ids(&body), vec!["undated", "old"]);
    assert_eq!(
        body["stats"]["boost"]["recency"], 1,
        "only the dated document is counted; the missing field is not an error"
    );
}

#[tokio::test]
async fn the_by_id_branch_boosts_too() {
    let harness = fixture(
        vec![
            meta("old", Some("2020-01-01"), None),
            meta("new", Some("2025-05-31"), None),
        ],
        &["old", "new"],
    );

    let response = harness
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key(TENANT),
            serde_json::json!({
                "query": "current allowance",
                "documentIds": ["old", "new"],
                "boost": {
                    "recency": {
                        "field": "metadata.effectiveDate",
                        "halfLifeDays": 365,
                        "weight": 1.0
                    }
                }
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(doc_ids(&body), vec!["new", "old"]);
    assert_eq!(body["stats"]["boost"]["recency"], 2);
}

#[tokio::test]
async fn a_key_limited_to_a_knowledge_base_still_cannot_see_another() {
    let harness = Harness::new();
    let tenant = TenantId(TENANT.into());
    harness.documents.insert_api_key_with_scopes(
        &tenant_key(TENANT),
        tenant.clone(),
        vec!["kb:read".into(), "kb:write".into()],
        vec![KnowledgeBaseId("kb-a".into())],
    );
    let mut allowed = meta("allowed", Some("2020-01-01"), Some(0.1));
    allowed.knowledge_base_ids = vec!["kb-a".into()];
    let mut hidden = meta("hidden", Some("2025-05-31"), Some(9.9));
    hidden.knowledge_base_ids = vec!["kb-b".into()];
    harness.documents.insert_doc(tenant.clone(), allowed);
    harness.documents.insert_doc(tenant, hidden);
    for (index, id) in ["allowed", "hidden"].iter().enumerate() {
        harness
            .chunks
            .vector_hits
            .write()
            .unwrap()
            .push(kb_engine::fakes::make_fake_hit(
                &format!("{id}_0"),
                id,
                id,
                "body",
                0,
                0.9 - index as f32 * 0.1,
            ));
    }

    let response = harness
        .send_json(
            "POST",
            "/v1/search",
            &tenant_key(TENANT),
            serde_json::json!({
                "query": "policy",
                "boost": {
                    "recency": {
                        "field": "metadata.effectiveDate",
                        "halfLifeDays": 365,
                        "weight": 1.0
                    },
                    "authority": {"field": "metadata.authority", "weight": 1.0}
                }
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(doc_ids(&body), vec!["allowed"]);
}

#[tokio::test]
async fn invalid_boost_shapes_are_400s() {
    let harness = fixture(vec![meta("a", Some("2025-01-01"), Some(1.0))], &["a"]);
    let key = tenant_key(TENANT);
    let cases: Vec<(serde_json::Value, &str)> = vec![
        (
            serde_json::json!({"boost": "yes"}),
            "'boost' must be an object",
        ),
        (
            serde_json::json!({"boost": {"freshness": {}}}),
            "Unknown boost member",
        ),
        (
            serde_json::json!({"boost": {"recency": "today"}}),
            "boost.recency must be an object",
        ),
        (
            serde_json::json!({"boost": {"recency": {"field": "publishedAt", "halfLifeDays": 30, "weight": 0.5}}}),
            "Invalid boost.recency.field",
        ),
        (
            serde_json::json!({"boost": {"recency": {"field": "metadata.bad/key", "halfLifeDays": 30, "weight": 0.5}}}),
            "Invalid boost.recency.field",
        ),
        (
            serde_json::json!({"boost": {"recency": {"field": "createdAt", "halfLifeDays": 0, "weight": 0.5}}}),
            "boost.recency.halfLifeDays",
        ),
        (
            serde_json::json!({"boost": {"recency": {"field": "createdAt", "halfLifeDays": 30, "weight": 0}}}),
            "boost.recency.weight",
        ),
        (
            serde_json::json!({"boost": {"recency": {"field": "createdAt", "halfLifeDays": 30, "weight": 1.5}}}),
            "boost.recency.weight",
        ),
        (
            serde_json::json!({"boost": {"recency": {"field": "createdAt", "halfLifeDays": 30, "weight": 0.5, "extra": 1}}}),
            "Unknown boost.recency member",
        ),
        (
            serde_json::json!({"boost": {"authority": {"field": "authority", "weight": 0.5}}}),
            "Invalid boost.authority.field",
        ),
        (
            serde_json::json!({"boost": {"authority": {"field": "metadata.source", "weight": 2}}}),
            "boost.authority.weight",
        ),
    ];

    for (body, expected) in cases {
        let mut body = body;
        body["query"] = serde_json::json!("policy");
        let response = harness
            .send_json("POST", "/v1/search", &key, body.clone())
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        let text = harness::error_text(response).await;
        assert!(
            text.contains(expected),
            "expected {expected:?} in {text:?} for {body}"
        );
    }
}
