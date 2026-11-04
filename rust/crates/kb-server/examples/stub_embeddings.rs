//! Stub OpenAI-compatible embedding endpoint for the smoke test.
//!
//! Deterministic, offline and free: each dimension is a function of the input
//! text, so the same query lands next to the same chunk without a model, a
//! network or an API key. Run it with the port the service should call:
//!
//! ```text
//! cargo run -p kb-server --example stub_embeddings -- 127.0.0.1:9111 8
//! ```
//!
//! Only the shape matters, and only the shape is implemented: `data[].embedding`
//! as an OpenAI-compatible endpoint returns it.
//!
//! `POST /page-embeddings` stands in for the page-image arm's multimodal
//! embedder (`KB_PAGE_EMBEDDING_BASE_URL`). It takes the shape
//! `kb_inference::HttpPageEmbedder` sends — pages as `{"content": [{"type":
//! "image_url", ...}]}` with `input_type: "document"`, queries as strings with
//! `input_type: "query"` — and answers every one of them with **the same unit
//! vector**. That is deliberate and it is the worst case for scope: every page
//! in the index, whoever owns it, is a perfect match for every query, so the
//! page index returns its whole candidate list and only the scope filter stands
//! between a caller and another tenant's pages. A leak cannot hide behind a
//! ranking that happened not to reach the foreign page. The leak matrix relies
//! on this; a retrieval-quality run must not.

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone)]
struct StubState {
    dim: usize,
}

#[derive(Deserialize)]
struct EmbeddingRequest {
    #[serde(default)]
    input: Option<Value>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:9111".to_string());
    let dim: usize = args.next().and_then(|d| d.parse().ok()).unwrap_or(8);

    let app = Router::new()
        .route("/embeddings", post(embed))
        .route("/page-embeddings", post(embed_pages))
        .route("/health", axum::routing::get(|| async { "ok" }))
        .with_state(StubState { dim });

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("stub embeddings listening on {addr} (dim {dim})");
    axum::serve(listener, app).await?;
    Ok(())
}

/// One embedding per input, in input order.
async fn embed(State(state): State<StubState>, Json(body): Json<EmbeddingRequest>) -> Json<Value> {
    let texts: Vec<String> = match body.input {
        Some(Value::String(s)) => vec![s],
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    };

    let data: Vec<Value> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            json!({
                "object": "embedding",
                "index": index,
                "embedding": embedding_for(text, state.dim),
            })
        })
        .collect();

    Json(json!({
        "object": "list",
        "data": data,
        "model": "stub",
        "usage": { "prompt_tokens": 1, "total_tokens": 1 },
    }))
}

#[derive(Deserialize)]
struct PageEmbeddingRequest {
    #[serde(default)]
    input: Vec<Value>,
    #[serde(default)]
    input_type: Option<String>,
}

/// One vector per input, all identical: see the module docs for why.
///
/// A malformed request is refused rather than answered, so a change to the
/// page embedder's wire shape fails the stack loudly instead of embedding
/// whatever arrived: a page must be an `image_url` part carrying a data URL, a
/// query must be a string, and `input_type` must say which one it is.
async fn embed_pages(
    State(state): State<StubState>,
    Json(body): Json<PageEmbeddingRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, String)> {
    let bad = |msg: &str| (axum::http::StatusCode::BAD_REQUEST, msg.to_string());
    match body.input_type.as_deref() {
        Some("document") => {
            if !body.input.iter().all(is_image_input) {
                return Err(bad("input_type=document wants image_url data-URL parts"));
            }
        }
        Some("query") => {
            if !body.input.iter().all(Value::is_string) {
                return Err(bad("input_type=query wants plain strings"));
            }
        }
        _ => return Err(bad("input_type must be `document` or `query`")),
    }
    let vector = page_vector(state.dim);
    let data: Vec<Value> = (0..body.input.len())
        .map(|index| json!({"object": "embedding", "index": index, "embedding": vector}))
        .collect();
    Ok(Json(json!({
        "object": "list",
        "data": data,
        "model": "stub-page",
        "usage": { "prompt_tokens": 1, "total_tokens": 1 },
    })))
}

fn is_image_input(item: &Value) -> bool {
    item.get("content")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            !parts.is_empty()
                && parts.iter().all(|p| {
                    p.get("type").and_then(Value::as_str) == Some("image_url")
                        && p.pointer("/image_url/url")
                            .and_then(Value::as_str)
                            .is_some_and(|u| u.starts_with("data:image/"))
                })
        })
}

/// The one page vector: unit length, every component equal.
fn page_vector(dim: usize) -> Vec<f32> {
    let dim = dim.max(1);
    vec![1.0 / (dim as f32).sqrt(); dim]
}

/// A deterministic pseudo-embedding of `dim` floats in [0, 1).
fn embedding_for(text: &str, dim: usize) -> Vec<f32> {
    let bytes = text.as_bytes();
    let len = bytes.len().max(1);
    (0..dim)
        .map(|i| {
            // An empty text has no byte to read; it embeds as if it were one 0.
            let byte = bytes.get(i % len).copied().unwrap_or(0);
            // `|| 1` keeps the all-zero text from embedding to all zeros, which
            // would make cosine similarity undefined.
            let value = u32::from(byte).max(1) % 100;
            value as f32 / 100.0
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_always_embeds_to_the_same_vector() {
        assert_eq!(embedding_for("Kota", 8), embedding_for("Kota", 8));
    }

    #[test]
    fn the_requested_width_is_honoured() {
        assert_eq!(embedding_for("Kota", 8).len(), 8);
        assert_eq!(embedding_for("Kota", 4096).len(), 4096);
    }

    #[test]
    fn values_stay_in_range_and_never_vanish() {
        for value in embedding_for("", 16) {
            assert!((0.0..1.0).contains(&value), "{value}");
            assert!(value > 0.0, "an all-zero vector has no direction");
        }
    }

    #[test]
    fn every_page_and_every_query_get_the_same_unit_vector() {
        let v = page_vector(8);
        assert_eq!(v.len(), 8);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "{norm}");
    }

    #[test]
    fn a_page_input_is_an_image_url_data_part() {
        let page = json!({"content": [{"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,AAAA"}}]});
        assert!(is_image_input(&page));
        assert!(!is_image_input(&json!("a query")));
        assert!(!is_image_input(
            &json!({"content": [{"type": "text", "text": "x"}]})
        ));
        assert!(!is_image_input(
            &json!({"content": [{"type": "image_url", "image_url": {"url": "https://x/y.jpg"}}]})
        ));
    }

    #[test]
    fn different_texts_embed_differently() {
        assert_ne!(embedding_for("Kota", 16), embedding_for("Pengunjung", 16));
    }
}
