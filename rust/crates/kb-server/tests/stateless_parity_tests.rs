//! API-5: pin stateless mode (`/v1/encode`, `/v1/rerank`) to the vectors and
//! orderings that full mode produces, with a shared fake embedder / reranker.
//!
//! Both modes are driven against the *same* recording fake backend, so a
//! difference in the result can only come from the mode's own composition
//! logic (prefix config, text composition, sparse tokenizer, rerank input),
//! never from model non-determinism.
//!
//! What is pinned, and where full mode does it:
//! - passage dense: `kb-engine/src/ingest.rs` `embedding_text` → `embed_passages`
//! - query dense: `kb-engine/src/retrieval.rs` `embed_query_detailed` → `embed_query`
//! - BM25 sparse: `kb-engine/src/ingest.rs` `kb_core::sparse::encode_document`
//! - rerank: `kb-engine/src/retrieval.rs` `RerankCandidate { text: content }` plus
//!   score-desc ordering, with `KB_RERANK_BREADCRUMBS=off`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::{Json, State};
use axum::http::{Method, Request, StatusCode};
use axum::routing::post;
use kb_engine::{DocumentProcessor, ProcessorSettings};
use kb_inference::{EmbedderImpl, EmbedderSettings};
use kb_ports::{ChunkId, Embedder, PortError, RerankCandidate, RerankResult, Reranker};
use kb_server::config::EmbedderConfig;
use kb_server::stateless::{
    StatelessAuth, StatelessConfig, StatelessState, build_stateless_router,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tower::ServiceExt;

// ─── Recording fake embedding backend ────────────────────────────────────────

/// A vector that is a pure function of the exact (already prefixed) text the
/// provider received. If two modes send different text they get different
/// vectors, so a bit-for-bit comparison is also a provider-text comparison.
fn vector_for(text: &str) -> Vec<f32> {
    let byte_sum: u32 = text.bytes().map(u32::from).sum();
    vec![
        byte_sum as f32,
        text.chars().count() as f32,
        text.len() as f32,
        0.5,
    ]
}

#[derive(Clone)]
struct EmbedState {
    /// One entry per HTTP request: the `input` array exactly as it arrived.
    seen: Arc<Mutex<Vec<Vec<String>>>>,
}

async fn embed_handler(State(state): State<EmbedState>, Json(body): Json<Value>) -> Json<Value> {
    let inputs: Vec<String> = body["input"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    state.seen.lock().await.push(inputs.clone());
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, t)| json!({ "index": i, "embedding": vector_for(t) }))
        .collect();
    Json(json!({ "object": "list", "data": data }))
}

struct RecordingEmbedServer {
    url: String,
    seen: Arc<Mutex<Vec<Vec<String>>>>,
}

async fn recording_embed_server() -> RecordingEmbedServer {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/embeddings", post(embed_handler))
        .with_state(EmbedState {
            seen: Arc::clone(&seen),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake embed server");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    RecordingEmbedServer {
        url: format!("http://{addr}/v1/embeddings"),
        seen,
    }
}

fn lookup_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    let mut map = std::collections::HashMap::new();
    for (k, v) in pairs {
        map.insert(*k, *v);
    }
    move |key: &str| map.get(key).copied().map(str::to_string)
}

/// Build the real stateless HTTP app from the same env lookup a deployment
/// uses, so the prefix env vars are read through the production config path.
/// Returns the app and the exact `EmbedderConfig` it wired, which the full-mode
/// side mirrors.
fn stateless_app_with_embedder(
    url: &str,
    query_prefix: &str,
    passage_prefix: &str,
) -> (Router, EmbedderConfig) {
    let pairs = [
        ("KB_STATELESS_ALLOW_UNAUTHENTICATED", "true"),
        ("KB_EMBEDDING_BASE_URL", url),
        ("KB_EMBEDDING_MODEL", "parity/fake-4d"),
        ("KB_EMBEDDING_DIM", "4"),
        ("KB_EMBEDDING_BATCH_SIZE", "8"),
        ("KB_EMBEDDING_CONCURRENCY", "1"),
        ("KB_EMBEDDING_QUERY_PREFIX", query_prefix),
        ("KB_EMBEDDING_PASSAGE_PREFIX", passage_prefix),
    ];
    let cfg = StatelessConfig::from_lookup(lookup_from(&pairs)).expect("stateless config");
    let embed_cfg = cfg.embedder.clone().expect("stateless embedder wired");
    let app = build_stateless_router(
        StatelessState::from_config(cfg).expect("stateless state with fake embedder"),
    );
    (app, embed_cfg)
}

/// Mirror of `kb-server/src/state.rs::embedder_settings_from` (crate-private,
/// so an integration test rebuilds it from the very fields the stateless
/// config produced). No field is re-derived: the two modes therefore embed
/// through identical settings.
fn embedder_settings(c: &EmbedderConfig) -> EmbedderSettings {
    EmbedderSettings {
        base_url: c.base_url.clone(),
        model: c.model.clone(),
        api_key: c.api_key.clone(),
        dimension: c.dimension,
        batch_size: c.batch_size,
        embed_concurrency: c.embed_concurrency,
        query_prefix: c.query_prefix.clone(),
        passage_prefix: c.passage_prefix.clone(),
        send_input_type: c.send_input_type,
        format: c.format,
        request_timeout: c.request_timeout,
    }
}

async fn encode(app: &Router, mode: &str, texts: &[&str]) -> Value {
    let body = json!({
        "texts": texts,
        "mode": mode,
        "dense": true,
        "sparse": true,
    })
    .to_string();
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/encode")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn f32_bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn json_f32_bits(v: &Value) -> Vec<u32> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| (x.as_f64().unwrap() as f32).to_bits())
        .collect()
}

/// Flatten the provider's recorded `input` arrays.
async fn provider_inputs(server: &RecordingEmbedServer) -> Vec<String> {
    server.seen.lock().await.iter().flatten().cloned().collect()
}

/// Full-mode ingest's passage text for a chunk (`ingest.rs:1260`). With no
/// title and no heading path this is the chunk content itself, which is the
/// "chunk with that exact text" case the parity promise is about.
fn full_mode_passage_text(text: &str) -> String {
    kb_core::title::embedding_text("", None, text)
}

// ─── 1. Passage dense parity ─────────────────────────────────────────────────

#[tokio::test]
async fn encode_passage_matches_full_mode_ingest_bit_for_bit_without_prefix() {
    let server = recording_embed_server().await;
    let (app, embed_cfg) = stateless_app_with_embedder(&server.url, "", "");

    let texts = [
        "short passage",
        "Cuti tahunan selama dua belas hari kerja.",
        "unicode αβγ 東京 passage",
    ];
    let encoded = encode(&app, "passage", &texts).await;
    let got = encoded["embeddings"].as_array().unwrap();

    let full = EmbedderImpl::new(embedder_settings(&embed_cfg)).unwrap();
    let full_texts: Vec<String> = texts.iter().map(|t| full_mode_passage_text(t)).collect();
    let want = full.embed_passages(&full_texts).await.unwrap();

    assert_eq!(got.len(), want.len());
    for (i, (got_v, want_v)) in got.iter().zip(&want).enumerate() {
        assert_eq!(
            json_f32_bits(got_v),
            f32_bits(want_v),
            "passage {i} vector bits differ"
        );
    }

    // The provider received the same text from both modes: unprefixed.
    let seen = provider_inputs(&server).await;
    for text in texts {
        assert!(
            seen.iter().any(|s| s == text),
            "provider never received {text:?}; saw {seen:?}"
        );
        assert!(
            seen.iter().all(|s| !s.starts_with("passage: ")),
            "unexpected passage prefix: {seen:?}"
        );
    }
}

#[tokio::test]
async fn encode_passage_matches_full_mode_ingest_bit_for_bit_with_prefix() {
    let server = recording_embed_server().await;
    let (app, embed_cfg) = stateless_app_with_embedder(&server.url, "", "passage: ");
    assert_eq!(embed_cfg.passage_prefix, "passage: ");

    let texts = ["first prefixed passage", "data tenant 42"];
    let encoded = encode(&app, "passage", &texts).await;
    let got = encoded["embeddings"].as_array().unwrap();

    let full = EmbedderImpl::new(embedder_settings(&embed_cfg)).unwrap();
    let full_texts: Vec<String> = texts.iter().map(|t| full_mode_passage_text(t)).collect();
    let want = full.embed_passages(&full_texts).await.unwrap();

    for (i, (got_v, want_v)) in got.iter().zip(&want).enumerate() {
        assert_eq!(json_f32_bits(got_v), f32_bits(want_v), "passage {i}");
    }

    let seen = provider_inputs(&server).await;
    for text in texts {
        let expected = format!("passage: {text}");
        assert!(
            seen.iter().any(|s| s == &expected),
            "provider never received {expected:?}; saw {seen:?}"
        );
    }
}

// ─── 2. Query dense parity ───────────────────────────────────────────────────

#[tokio::test]
async fn encode_query_matches_full_mode_search_bit_for_bit_without_prefix() {
    let server = recording_embed_server().await;
    let (app, embed_cfg) = stateless_app_with_embedder(&server.url, "", "");

    let queries = ["rotate api keys", "berlaku 12 hari"];
    let encoded = encode(&app, "query", &queries).await;
    let got = encoded["embeddings"].as_array().unwrap();

    // Full mode: retrieval.rs:995 `embed_query_detailed` defaults to `embed_query`.
    let full = EmbedderImpl::new(embedder_settings(&embed_cfg)).unwrap();
    let mut want = Vec::with_capacity(queries.len());
    for q in queries {
        want.push(full.embed_query(q).await.unwrap());
    }

    for (i, (got_v, want_v)) in got.iter().zip(&want).enumerate() {
        assert_eq!(json_f32_bits(got_v), f32_bits(want_v), "query {i}");
    }

    let seen = provider_inputs(&server).await;
    for q in queries {
        assert!(
            seen.iter().any(|s| s == q),
            "query text {q:?} not sent: {seen:?}"
        );
    }
}

#[tokio::test]
async fn encode_query_matches_full_mode_search_bit_for_bit_with_prefix() {
    let server = recording_embed_server().await;
    let (app, embed_cfg) = stateless_app_with_embedder(&server.url, "query: ", "");
    assert_eq!(embed_cfg.query_prefix, "query: ");

    let queries = ["prefixed query one", "prefixed query two"];
    let encoded = encode(&app, "query", &queries).await;
    let got = encoded["embeddings"].as_array().unwrap();

    let full = EmbedderImpl::new(embedder_settings(&embed_cfg)).unwrap();
    let mut want = Vec::with_capacity(queries.len());
    for q in queries {
        want.push(full.embed_query(q).await.unwrap());
    }
    for (i, (got_v, want_v)) in got.iter().zip(&want).enumerate() {
        assert_eq!(json_f32_bits(got_v), f32_bits(want_v), "query {i}");
    }

    let seen = provider_inputs(&server).await;
    for q in queries {
        let expected = format!("query: {q}");
        assert!(
            seen.iter().any(|s| s == &expected),
            "provider never received {expected:?}; saw {seen:?}"
        );
    }
}

// ─── 3. Sparse BM25 parity ───────────────────────────────────────────────────

#[tokio::test]
async fn encode_sparse_matches_full_mode_bm25_indexer_and_query() {
    // Prefixes are irrelevant to BM25; set them to prove that too.
    let server = recording_embed_server().await;
    let (app, _) = stateless_app_with_embedder(&server.url, "query: ", "passage: ");
    let texts = ["index this passage", "FNV-1a term ids for tenant scope"];

    let passage = encode(&app, "passage", &texts).await;
    let passage_sparse = passage["sparseVectors"].as_array().unwrap();
    for (i, text) in texts.iter().enumerate() {
        // ingest.rs:1331 — what full mode indexes for a chunk of this text.
        let want = kb_engine::SparseVectorData::from(kb_core::sparse::encode_document(text));
        let got = &passage_sparse[i];
        let got_indices: Vec<u32> = got["indices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        assert_eq!(got_indices, want.indices, "sparse indices {i}");
        assert_eq!(
            json_f32_bits(&got["values"]),
            f32_bits(&want.values),
            "sparse values {i}"
        );
    }

    let query = encode(&app, "query", &["index this passage"]).await;
    let got = &query["sparseVectors"][0];
    // retrieval.rs:1017 — full mode's BM25 query encoding.
    let want = kb_core::sparse::encode_query("index this passage");
    let got_indices: Vec<u32> = got["indices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    assert_eq!(got_indices, want.indices);
    assert_eq!(json_f32_bits(&got["values"]), f32_bits(&want.values));
}

// ─── 4. Rerank parity ────────────────────────────────────────────────────────

/// Deterministic cross-encoder; records every call. Scores are exactly
/// representable in f32 (length / 16 plus a whole-number match bonus) so a
/// JSON f64 → f32 round trip cannot change a bit.
/// (query, candidate texts, `final_k`) for one reranker call.
type RerankCall = (String, Vec<String>, u32);

#[derive(Clone)]
struct RecordingReranker {
    seen: Arc<Mutex<Vec<RerankCall>>>,
}

#[async_trait]
impl Reranker for RecordingReranker {
    fn scores_every_candidate(&self) -> bool {
        true
    }

    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError> {
        self.seen.lock().await.push((
            query.to_string(),
            candidates.iter().map(|c| c.text.clone()).collect(),
            final_k,
        ));
        let q = query.to_lowercase();
        let mut scored: Vec<(RerankCandidate, f32)> = candidates
            .into_iter()
            .map(|c| {
                let bonus = if c.text.to_lowercase().contains(&q) {
                    1.0
                } else {
                    0.0
                };
                let score = (c.text.len() as f32) / 16.0 + bonus;
                (c, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok(scored
            .into_iter()
            .take(final_k as usize)
            .enumerate()
            .map(|(rank, (c, s))| RerankResult {
                id: c.id,
                final_rank: rank as u32,
                score: s,
            })
            .collect())
    }
}

fn rerank_app(reranker: Arc<dyn Reranker>) -> Router {
    let extractor = kb_extract::build_production_extractor(&kb_extract::ExtractConfig::default())
        .unwrap()
        .shared();
    let processor = DocumentProcessor::new(extractor, ProcessorSettings::default())
        .with_reranker(reranker, Some("parity/fake-rerank".to_string()));
    let state = StatelessState::from_parts(
        processor,
        StatelessAuth::Unauthenticated,
        "test-parity".into(),
    );
    build_stateless_router(state)
}

/// Full-mode rerank stage: candidate text is the bare content with
/// `KB_RERANK_BREADCRUMBS=off` (retrieval.rs:1434-1443), and the engine sorts
/// the provider's results by score (retrieval.rs:1501).
async fn full_mode_rerank(
    reranker: &dyn Reranker,
    query: &str,
    texts: &[String],
) -> Vec<(usize, f32)> {
    let candidates: Vec<RerankCandidate> = texts
        .iter()
        .enumerate()
        .map(|(i, text)| RerankCandidate {
            id: ChunkId(i.to_string()),
            text: text.clone(),
            original_rank: i as u32,
            original_score: 0.0,
        })
        .collect();
    let mut ordered = reranker
        .rerank(query, candidates, texts.len() as u32)
        .await
        .unwrap();
    ordered.sort_by(|a, b| b.score.total_cmp(&a.score));
    ordered
        .into_iter()
        .map(|r| (r.id.0.parse::<usize>().unwrap(), r.score))
        .collect()
}

async fn stateless_rerank(app: &Router, query: &str, docs: &[&str]) -> Vec<(usize, f32)> {
    let body = json!({
        "query": query,
        "documents": docs,
        "topK": docs.len(),
    })
    .to_string();
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/rerank")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["index"].as_u64().unwrap() as usize,
                r["score"].as_f64().unwrap() as f32,
            )
        })
        .collect()
}

#[tokio::test]
async fn rerank_order_and_scores_match_full_mode_with_breadcrumbs_off() {
    let reranker = Arc::new(RecordingReranker {
        seen: Arc::new(Mutex::new(Vec::<RerankCall>::new())),
    });
    let app = rerank_app(reranker.clone());

    let query = "qdrant";
    let docs = vec![
        "unrelated memo".to_string(),
        "Qdrant hybrid search".to_string(),
        "qdrant sparse vectors".to_string(),
        "other text".to_string(),
    ];
    let doc_refs: Vec<&str> = docs.iter().map(String::as_str).collect();

    let got = stateless_rerank(&app, query, &doc_refs).await;
    let want = full_mode_rerank(reranker.as_ref(), query, &docs).await;

    assert_eq!(
        got.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        want.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        "rerank order differs"
    );
    assert_eq!(
        got.iter().map(|(_, s)| s.to_bits()).collect::<Vec<_>>(),
        want.iter().map(|(_, s)| s.to_bits()).collect::<Vec<_>>(),
        "rerank scores differ"
    );

    // Both modes handed the reranker the exact same candidate text sequence.
    let seen = reranker.seen.lock().await;
    assert_eq!(seen.len(), 2, "one call per mode");
    assert_eq!(seen[0].0, query);
    assert_eq!(seen[0].1, docs);
    assert_eq!(seen[1].1, seen[0].1, "candidate text differs between modes");
}

// ─── 5. RET-12 breadcrumb difference (documented, not a bug) ─────────────────

#[tokio::test]
async fn full_mode_breadcrumb_text_is_client_side_for_stateless_rerank() {
    // With KB_RERANK_BREADCRUMBS=on full mode prepends this exact string,
    // then a newline, then the content (retrieval.rs:1434-1440). Stateless
    // /v1/rerank has no document title/section/page and sends the bare text,
    // so the caller must prepend the same breadcrumb for parity.
    let crumb =
        kb_core::rerank::breadcrumb("Kebijakan Cuti", Some("Bab IV › Cuti Tahunan"), Some(12));
    assert_eq!(crumb, "Kebijakan Cuti › Bab IV › Cuti Tahunan · p.12");

    let content = "berlaku 12 hari kerja";
    let full_mode_input = format!("{crumb}\n{content}");
    assert_eq!(
        full_mode_input,
        "Kebijakan Cuti › Bab IV › Cuti Tahunan · p.12\nberlaku 12 hari kerja"
    );

    // The stateless side cannot see those metadata fields: the only text it
    // has is `content`, so a client that sends `full_mode_input` as its
    // `documents[]` entry reproduces the full-mode reranker input exactly.
    let client_prepended = format!("{crumb}\n{content}");
    assert_eq!(client_prepended, full_mode_input);
}
