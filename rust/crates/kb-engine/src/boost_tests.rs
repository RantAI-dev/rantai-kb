//! `RET-4`: recency and authority boosts.
//!
//! Three properties are load-bearing and each has a test here:
//!
//! * a request without the field (or with an empty one) ranks exactly as
//!   before — the boost is off, not "neutral";
//! * a recency boost promotes the newer of two otherwise equal documents, and
//!   an authority boost promotes the higher-valued one;
//! * a missing or invalid field is not an error and does not change that
//!   document's base relevance.
//!
//! The floor is checked too: it must keep reading the raw rerank/dense score,
//! never the boosted order.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use kb_core::{AuthorityBoost, BoostConfig, RecencyBoost, RecencyField};
use kb_ports::{DocumentId, DocumentMeta, DocumentScope, TenantId};

use crate::fakes::{
    FakeChunkStore, FakeDocumentStore, FakeEmbedder, FakeReranker, make_fake_hit, make_fake_scope,
};
use crate::retrieval::{RetrievalOptions, apply_boosts, execute_pipeline};

const NOW: &str = "2025-06-01T00:00:00Z";

fn now() -> DateTime<Utc> {
    kb_core::parse_date(NOW).unwrap()
}

fn doc(id: &str, effective_date: Option<&str>, authority: Option<f64>) -> DocumentMeta {
    let mut metadata = serde_json::Map::new();
    if let Some(date) = effective_date {
        metadata.insert("effectiveDate".into(), serde_json::json!(date));
    }
    if let Some(value) = authority {
        metadata.insert("authority".into(), serde_json::json!(value));
    }
    DocumentMeta {
        id: DocumentId(id.into()),
        title: format!("Title {id}"),
        categories: vec![],
        subcategory: None,
        status: "ready".into(),
        external_ref: None,
        chunk_count: 1,
        file_type: None,
        file_size: None,
        mime_type: None,
        s3_key: None,
        created_at: "2024-01-01T00:00:00.000Z".into(),
        updated_at: "2024-01-01T00:00:00.000Z".into(),
        knowledge_base_ids: vec![],
        metadata: if metadata.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(metadata))
        },
    }
}

struct Fixture {
    docs: Arc<FakeDocumentStore>,
    chunks: Arc<FakeChunkStore>,
    embedder: Arc<FakeEmbedder>,
    reranker: Arc<FakeReranker>,
    scope: DocumentScope,
}

impl Fixture {
    /// Documents are seeded in `pre_boost_order`; every hit has the same dense
    /// similarity and rank, so without a boost the order is exactly the seeding
    /// order.
    fn new(documents: Vec<DocumentMeta>, pre_boost_order: &[&str], similarity: f32) -> Self {
        let tenant = TenantId("t1".into());
        let docs = Arc::new(FakeDocumentStore::default());
        for meta in documents {
            docs.insert_doc(tenant.clone(), meta);
        }
        let chunks = Arc::new(FakeChunkStore::default());
        for (index, doc_id) in pre_boost_order.iter().enumerate() {
            // A descending fused score makes the arm's pre-boost order exactly
            // the seeding order; the hits still share the same rank, so the
            // boost's base relevance (`1/(1+rank)`) is equal for all of them.
            chunks.vector_hits.write().unwrap().push(make_fake_hit(
                &format!("{doc_id}_0"),
                doc_id,
                &format!("Title {doc_id}"),
                &format!("Body {doc_id}"),
                0,
                similarity - index as f32 * 0.01,
            ));
        }
        let scope = make_fake_scope(
            tenant,
            pre_boost_order
                .iter()
                .map(|d| DocumentId((*d).into()))
                .collect(),
        );
        Self {
            docs,
            chunks,
            embedder: Arc::new(FakeEmbedder::default()),
            reranker: Arc::new(FakeReranker::default()),
            scope,
        }
    }

    async fn run(&self, mut opts: RetrievalOptions) -> crate::RetrievalOutcome {
        opts.hybrid_bm25_enabled = false;
        opts.boost_now = Some(now());
        execute_pipeline(
            &(self.docs.clone() as Arc<_>),
            &(self.chunks.clone() as Arc<_>),
            &(self.embedder.clone() as Arc<_>),
            &(self.reranker.clone() as Arc<_>),
            &noop_expander(),
            &self.scope,
            "what is the current allowance",
            &opts,
            &crate::timing::SystemClock,
        )
        .await
        .unwrap()
    }
}

fn noop_expander() -> Arc<dyn crate::QueryExpander> {
    Arc::new(crate::NoopExpander)
}

fn recency(field: RecencyField, half_life_days: f64, weight: f64) -> BoostConfig {
    BoostConfig {
        recency: Some(RecencyBoost {
            field,
            half_life_days,
            weight,
        }),
        authority: None,
    }
}

fn ids(outcome: &crate::RetrievalOutcome) -> Vec<&str> {
    outcome
        .chunks
        .iter()
        .map(|h| h.chunk.document_id.0.as_str())
        .collect()
}

#[tokio::test]
async fn boost_absent_is_rank_identical_to_an_empty_boost() {
    let documents = vec![
        doc("old", Some("2020-01-01"), None),
        doc("new", Some("2025-05-31"), None),
        doc("middle", Some("2023-01-01"), None),
    ];
    let absent = Fixture::new(documents.clone(), &["old", "new", "middle"], 0.9);
    let empty = Fixture::new(documents, &["old", "new", "middle"], 0.9);

    let without = absent.run(RetrievalOptions::default()).await;
    let with_empty = empty
        .run(RetrievalOptions {
            boost: Some(BoostConfig::default()),
            ..Default::default()
        })
        .await;

    // The pre-boost order here is deliberately not date order: an inert boost
    // must not re-sort it.
    assert_eq!(ids(&without), vec!["old", "new", "middle"]);
    assert_eq!(ids(&with_empty), ids(&without));
    assert_eq!(with_empty.boost, None, "an empty boost reports nothing");
}

#[test]
fn recency_promotes_the_newer_when_base_relevance_is_exactly_equal() {
    // Two hits with the same base relevance (same rank, no rerank score). The
    // only difference is the date, so the flip can only come from the boost.
    let mut metas = HashMap::new();
    metas.insert(
        DocumentId("old".into()),
        doc("old", Some("2023-01-01"), None),
    );
    metas.insert(
        DocumentId("new".into()),
        doc("new", Some("2025-05-31"), None),
    );
    let old = make_fake_hit("old_0", "old", "Old", "Old body", 0, 0.9);
    let new = make_fake_hit("new_0", "new", "New", "New body", 0, 0.9);
    let mut hits = vec![old, new];
    let stats = apply_boosts(
        &mut hits,
        &metas,
        &recency(RecencyField::Metadata("effectiveDate".into()), 365.0, 0.5),
        now(),
    );
    assert_eq!(stats.recency, 2);
    assert_eq!(
        hits.iter()
            .map(|h| h.chunk.document_id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["new", "old"]
    );
}

#[tokio::test]
async fn recency_promotes_the_newer_of_two_near_equal_reranked_docs() {
    let fixture = Fixture::new(
        vec![
            doc("old", Some("2023-01-01"), None),
            doc("new", Some("2025-05-31"), None),
        ],
        &["old", "new"],
        0.9,
    );

    let boosted = fixture
        .run(RetrievalOptions {
            // Reranked, so the two hits start with near-equal base relevance
            // (sigmoid 1.0 vs 0.95) and the pre-boost order is old → new.
            rerank_enabled: true,
            boost: Some(recency(
                RecencyField::Metadata("effectiveDate".into()),
                365.0,
                0.5,
            )),
            ..Default::default()
        })
        .await;

    assert_eq!(ids(&boosted), vec!["new", "old"]);
    assert_eq!(
        boosted.boost,
        Some(crate::BoostStats {
            recency: 2,
            authority: 0
        })
    );
    // rank is renumbered after the re-sort; the scores are untouched.
    assert_eq!(boosted.chunks[0].rank, 0);
    assert_eq!(boosted.chunks[1].rank, 1);
}

#[tokio::test]
async fn a_document_missing_the_date_keeps_its_place() {
    let fixture = Fixture::new(
        vec![
            doc("undated", None, None),
            doc("old", Some("2020-01-01"), None),
        ],
        &["undated", "old"],
        0.9,
    );

    let boosted = fixture
        .run(RetrievalOptions {
            boost: Some(recency(
                RecencyField::Metadata("effectiveDate".into()),
                30.0,
                1.0,
            )),
            ..Default::default()
        })
        .await;

    assert_eq!(
        ids(&boosted),
        vec!["undated", "old"],
        "a missing field gets factor 1.0 and is not an error"
    );
    assert_eq!(
        boosted.boost,
        Some(crate::BoostStats {
            recency: 1,
            authority: 0
        })
    );
}

#[tokio::test]
async fn authority_promotes_the_higher_value() {
    let fixture = Fixture::new(
        vec![doc("low", None, Some(0.1)), doc("high", None, Some(0.9))],
        &["low", "high"],
        0.9,
    );

    let boosted = fixture
        .run(RetrievalOptions {
            rerank_enabled: true,
            boost: Some(BoostConfig {
                recency: None,
                authority: Some(AuthorityBoost {
                    field: "authority".into(),
                    weight: 0.5,
                }),
            }),
            ..Default::default()
        })
        .await;

    assert_eq!(ids(&boosted), vec!["high", "low"]);
    assert_eq!(
        boosted.boost,
        Some(crate::BoostStats {
            recency: 0,
            authority: 2
        })
    );
}

#[tokio::test]
async fn the_floor_still_reads_the_raw_dense_score() {
    let fixture = Fixture::new(
        vec![
            doc("relevant", Some("2020-01-01"), None),
            doc("irrelevant", Some("2025-05-31"), None),
        ],
        &["relevant", "irrelevant"],
        1.0,
    );
    // Lower the second hit below the floor while the recency boost would love
    // to promote it. The floor must drop it first; the boost cannot resurrect
    // it and cannot lower the first hit's raw score either.
    fixture.chunks.vector_hits.write().unwrap()[1].similarity = 0.5;

    let outcome = fixture
        .run(RetrievalOptions {
            min_score: Some(0.6),
            boost: Some(recency(
                RecencyField::Metadata("effectiveDate".into()),
                30.0,
                1.0,
            )),
            ..Default::default()
        })
        .await;

    assert_eq!(ids(&outcome), vec!["relevant"]);
    assert_eq!(outcome.floor.unwrap().dropped, 1);
    assert_eq!(outcome.chunks[0].dense_score, Some(1.0));
}

#[tokio::test]
async fn scope_is_still_the_callers_scope_with_a_boost_on() {
    let fixture = Fixture::new(
        vec![doc("mine", Some("2020-01-01"), Some(0.1))],
        &["mine"],
        0.9,
    );
    // A document outside the scope exists in the store and in the arm. The arm
    // filters by scope, and the boost reads metadata only for hits that came
    // back, so the foreign document cannot appear.
    fixture.docs.insert_doc(
        TenantId("t1".into()),
        doc("not-mine", Some("2025-05-31"), Some(9.9)),
    );
    fixture
        .chunks
        .vector_hits
        .write()
        .unwrap()
        .push(make_fake_hit(
            "not-mine_0",
            "not-mine",
            "Not Mine",
            "secret",
            0,
            0.99,
        ));

    let outcome = fixture
        .run(RetrievalOptions {
            boost: Some(BoostConfig {
                recency: Some(RecencyBoost {
                    field: RecencyField::CreatedAt,
                    half_life_days: 30.0,
                    weight: 0.5,
                }),
                authority: Some(AuthorityBoost {
                    field: "authority".into(),
                    weight: 0.5,
                }),
            }),
            ..Default::default()
        })
        .await;

    assert_eq!(ids(&outcome), vec!["mine"]);
}

#[test]
fn ties_break_on_content_then_page_then_chunk_index_never_on_ids() {
    let meta = doc("d", Some("2025-06-01"), None);
    let mut meta_by_id = HashMap::new();
    meta_by_id.insert(DocumentId("d".into()), meta);
    let config = recency(RecencyField::Metadata("effectiveDate".into()), 365.0, 0.5);

    let mut a = make_fake_hit("zzz_a", "d", "D", "same", 0, 0.9);
    let mut b = make_fake_hit("aaa_b", "d", "D", "same", 0, 0.9);
    a.chunk.metadata.page = Some(2);
    b.chunk.metadata.page = Some(1);
    let mut hits = vec![a, b];
    apply_boosts(&mut hits, &meta_by_id, &config, now());
    assert_eq!(
        hits.iter()
            .map(|h| h.chunk.metadata.page.unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2],
        "equal finals and content break on page, matching sort_candidates"
    );

    // Different content: content decides before page, as it does in
    // `sort_candidates`.
    let mut a = make_fake_hit("zzz_a", "d", "D", "alpha", 0, 0.9);
    let mut b = make_fake_hit("aaa_b", "d", "D", "bravo", 0, 0.9);
    a.chunk.metadata.page = Some(2);
    b.chunk.metadata.page = Some(1);
    let mut hits = vec![b, a];
    apply_boosts(&mut hits, &meta_by_id, &config, now());
    assert_eq!(
        hits.iter()
            .map(|h| h.chunk.content.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "bravo"]
    );

    // Same content and page: chunk index decides.
    let mut a = make_fake_hit("zzz_a", "d", "D", "same", 5, 0.9);
    let mut b = make_fake_hit("aaa_b", "d", "D", "same", 2, 0.9);
    a.chunk.metadata.page = Some(1);
    b.chunk.metadata.page = Some(1);
    let mut hits = vec![a, b];
    apply_boosts(&mut hits, &meta_by_id, &config, now());
    assert_eq!(
        hits.iter()
            .map(|h| h.chunk.metadata.chunk_index.unwrap())
            .collect::<Vec<_>>(),
        vec![2, 5]
    );

    // Same content/page/index: the ids are ignored, and the stable sort keeps
    // the order the pipeline already had.
    let mut a = make_fake_hit("zzz_a", "d", "D", "same", 0, 0.9);
    let mut b = make_fake_hit("aaa_b", "d", "D", "same", 0, 0.9);
    a.chunk.metadata.page = Some(1);
    b.chunk.metadata.page = Some(1);
    let mut hits = vec![a, b];
    apply_boosts(&mut hits, &meta_by_id, &config, now());
    assert_eq!(
        hits.iter()
            .map(|h| h.chunk.id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["zzz_a", "aaa_b"]
    );
}
