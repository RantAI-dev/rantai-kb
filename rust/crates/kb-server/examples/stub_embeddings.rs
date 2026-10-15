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

/// A deterministic pseudo-embedding of `dim` floats in [0, 1).
fn embedding_for(text: &str, dim: usize) -> Vec<f32> {
    let bytes = text.as_bytes();
    let len = bytes.len().max(1);
    (0..dim)
        .map(|i| {
            let byte = bytes[i % len];
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
    fn different_texts_embed_differently() {
        assert_ne!(embedding_for("Kota", 16), embedding_for("Pengunjung", 16));
    }
}
