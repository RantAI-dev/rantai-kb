//! `RET-18`: the additive `confidence` object over the real router.
//!
//! A caller must be able to decide "answer or say I don't know" from the
//! response. These tests pin the wire shape: it is present on standard and
//! `documentIds` searches, absent on the `hybrid: true` stub, uncalibrated by
//! default with a band from `minScore`, and driven by a loaded override table.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod harness;

use axum::http::StatusCode;
use harness::{Harness, doc_meta, json_body, tenant_key};
use kb_core::CalibrationTable;
use kb_ports::{DocumentMeta, TenantId};

const TENANT: &str = "tenant-1";

fn fixture() -> Harness {
    let harness = Harness::new();
    let tenant = TenantId(TENANT.into());
    harness
        .documents
        .insert_api_key(&tenant_key(TENANT), tenant.clone());
    for id in ["a", "b"] {
        let doc: DocumentMeta = doc_meta(id, &format!("Title {id}"));
        harness.documents.insert_doc(tenant.clone(), doc);
    }
    for (index, id) in ["a", "b"].iter().enumerate() {
        let mut hit = kb_engine::fakes::make_fake_hit(
            &format!("{id}_0"),
            id,
            &format!("Title {id}"),
            &format!("Body {id}"),
            0,
            0.9 - index as f32 * 0.1,
        );
        // The stub embedder's cosine is the comparable score; `make_fake_hit`
        // leaves it unset, so pin it here to exercise the dense arm.
        hit.dense_score = Some(hit.similarity);
        harness.chunks.vector_hits.write().unwrap().push(hit);
    }
    harness
}

async fn search(harness: &Harness, body: serde_json::Value) -> serde_json::Value {
    let response = harness
        .send_json("POST", "/v1/search", &tenant_key(TENANT), body)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

#[tokio::test]
async fn confidence_is_always_present_and_uncalibrated_with_a_floor_fallback() {
    let harness = fixture();

    let with_floor = search(
        &harness,
        serde_json::json!({"query": "Title", "minScore": 0.5}),
    )
    .await;
    let confidence = &with_floor["confidence"];
    assert_eq!(confidence["scoreKind"], "dense");
    assert_eq!(confidence["band"], "low");
    assert_eq!(confidence["calibrated"].as_bool(), Some(false));
    assert!(
        (confidence["topScore"].as_f64().unwrap() - 0.9).abs() < 1e-6,
        "{confidence}"
    );
    assert!(
        (confidence["margin"].as_f64().unwrap() - 0.1).abs() < 1e-6,
        "{confidence}"
    );

    // Without a floor, an unfitted score has no comparable meaning: say so.
    let no_floor = search(&harness, serde_json::json!({"query": "Title"})).await;
    assert_eq!(no_floor["confidence"]["band"], "unknown");
    assert_eq!(no_floor["confidence"]["calibrated"].as_bool(), Some(false));

    // Nothing passed the floor: `none` is a relevance answer, not a failure.
    let empty = search(
        &harness,
        serde_json::json!({"query": "Title", "minScore": 0.95}),
    )
    .await;
    assert_eq!(empty["confidence"]["band"], "none");
    assert_eq!(empty["confidence"]["topScore"], serde_json::Value::Null);
    assert_eq!(empty["confidence"]["margin"], serde_json::Value::Null);
    assert_eq!(empty["confidence"]["calibrated"].as_bool(), Some(false));

    // A documentIds search shares the same tail and the same object.
    let narrowed = search(
        &harness,
        serde_json::json!({"query": "Title", "documentIds": ["a"], "minScore": 0.5}),
    )
    .await;
    assert_eq!(narrowed["confidence"]["band"], "low");
    assert_eq!(narrowed["confidence"]["calibrated"].as_bool(), Some(false));

    // The Bun stub runs no search, so it has no score to report.
    let hybrid = search(
        &harness,
        serde_json::json!({"query": "Title", "hybrid": true}),
    )
    .await;
    assert!(hybrid.get("confidence").is_none(), "{hybrid}");
}

#[tokio::test]
async fn a_loaded_calibration_file_drives_the_band() {
    // This is the same text `KB_CONFIDENCE_CALIBRATION` reads at startup.
    let raw = r#"[{"modelId":"stub-embed","scoreKind":"dense","high":0.8,"medium":0.6,"low":0.4}]"#;
    let table = CalibrationTable::from_json(raw).expect("valid calibration file");
    let harness = Harness::with_confidence(table, "stub-embed".to_string(), None);
    let tenant = TenantId(TENANT.into());
    harness
        .documents
        .insert_api_key(&tenant_key(TENANT), tenant.clone());
    let doc: DocumentMeta = doc_meta("a", "Title a");
    harness.documents.insert_doc(tenant, doc);
    let mut hit = kb_engine::fakes::make_fake_hit("a_0", "a", "Title a", "Body a", 0, 0.9);
    hit.dense_score = Some(0.9);
    harness.chunks.vector_hits.write().unwrap().push(hit);

    let fitted = search(&harness, serde_json::json!({"query": "Title"})).await;
    assert_eq!(fitted["confidence"]["band"], "high");
    assert_eq!(fitted["confidence"]["calibrated"].as_bool(), Some(true));
    assert_eq!(fitted["confidence"]["scoreKind"], "dense");
}

#[tokio::test]
async fn a_model_mismatch_stays_uncalibrated_even_with_a_loaded_table() {
    let raw =
        r#"[{"modelId":"other-model","scoreKind":"dense","high":0.8,"medium":0.6,"low":0.4}]"#;
    let table = CalibrationTable::from_json(raw).expect("valid calibration file");
    let harness = Harness::with_confidence(table, "stub-embed".to_string(), None);
    let tenant = TenantId(TENANT.into());
    harness
        .documents
        .insert_api_key(&tenant_key(TENANT), tenant.clone());
    let doc: DocumentMeta = doc_meta("a", "Title a");
    harness.documents.insert_doc(tenant, doc);
    let mut hit = kb_engine::fakes::make_fake_hit("a_0", "a", "Title a", "Body a", 0, 0.9);
    hit.dense_score = Some(0.9);
    harness.chunks.vector_hits.write().unwrap().push(hit);

    let body = search(&harness, serde_json::json!({"query": "Title"})).await;
    assert_eq!(body["confidence"]["calibrated"].as_bool(), Some(false));
    assert_eq!(body["confidence"]["band"], "unknown");
}
