//! The tiny fixture must parse: it is the shape every real dataset takes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use kb_eval::dataset::{Dataset, base_doc_key, gold_for_documents, gold_for_pages, page_of};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tiny")
}

#[test]
fn tiny_fixture_loads() {
    let dataset = Dataset::load(fixture()).expect("load fixture");
    assert_eq!(dataset.name, "tiny");

    // Corpus: five markdown notes plus a two-page PDF, keyed by file stem.
    let keys: Vec<&str> = dataset.corpus.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "coffee",
            "guitar",
            "mars",
            "mission",
            "photosynthesis",
            "rust-lang"
        ]
    );

    assert_eq!(dataset.queries.len(), 12);
    assert_eq!(dataset.queries[0].id, "q1");
    assert!(dataset.queries[0].text.contains("largest volcano"));

    // Twelve judgements, of which two are page-level.
    assert_eq!(dataset.qrels.len(), 12);
    let page_qrels: Vec<_> = dataset
        .qrels
        .iter()
        .filter(|q| page_of(&q.corpus_id).is_some())
        .collect();
    assert_eq!(page_qrels.len(), 2);
    assert_eq!(page_qrels[0].corpus_id, "mission#p2");
    assert_eq!(page_of("mission#p2"), Some(2));

    // Optional answers are parsed but not scored.
    assert_eq!(dataset.answers.len(), 3);
    assert_eq!(dataset.answers[0].answer, "Olympus Mons");
}

#[test]
fn fixture_gold_sets_are_consistent() {
    let dataset = Dataset::load(fixture()).expect("load fixture");

    let q1 = dataset.qrels_for("q1");
    assert_eq!(q1.len(), 1);
    assert_eq!(gold_for_documents(&q1).get("mars"), Some(&2));

    // A page-level query has no document-level gold but does have page gold.
    let q11 = dataset.qrels_for("q11");
    assert_eq!(gold_for_documents(&q11).get("mission"), Some(&2));
    assert_eq!(gold_for_pages(&q11).get("mission#p2"), Some(&2));
    assert_eq!(base_doc_key("mission#p2"), "mission");
}

#[test]
fn every_query_has_at_least_one_judgement() {
    let dataset = Dataset::load(fixture()).expect("load fixture");
    for query in &dataset.queries {
        assert!(
            !dataset.qrels_for(&query.id).is_empty(),
            "query {} has no qrels",
            query.id
        );
    }
}
