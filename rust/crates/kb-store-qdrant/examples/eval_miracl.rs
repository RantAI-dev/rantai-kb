//! Evaluation of Qdrant RetrievalIndex on BEIR / MIRACL datasets.
//!
//! Loads a dataset, ingests sparse (and optional dense) representations,
//! runs retrieval, writes a TREC run file, and measures search latency.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use kb_core::sparse::{self, encode_document, encode_document_avgdl};
use kb_engine::fakes::make_fake_scope;
use kb_eval::dataset::Dataset;
use kb_ports::{
    ChunkId, ChunkType, DocumentId, Fusion, HybridQuery, IndexUnit, KeywordQuery, RetrievalIndex,
    TenantId,
};
use kb_store_qdrant::{QdrantIndex, QdrantSettings, create_collection, drop_collection};

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64) * (p / 100.0)).floor() as usize;
    let idx = idx.min(sorted.len() - 1);
    sorted[idx]
}

/// Simple deterministic pseudo-random normalized vector generator
fn pseudo_random_vector(seed: &mut u64, dim: usize) -> Vec<f32> {
    let mut vec = Vec::with_capacity(dim);
    let mut norm_sq = 0.0f32;
    for _ in 0..dim {
        // Xorshift64
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        let val = ((*seed as f32) / (u64::MAX as f32)) * 2.0 - 1.0;
        vec.push(val);
        norm_sq += val * val;
    }
    let norm = norm_sq.sqrt().max(1e-9);
    for v in &mut vec {
        *v /= norm;
    }
    vec
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let dataset_dir = args
        .iter()
        .position(|a| a == "--dataset")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rust/crates/kb-eval/datasets/miracl-id-200"));

    let out_path = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("eval-out/qdrant-sparse.trec"));

    let use_real_avgdl = args.iter().any(|a| a == "--use-real-avgdl");
    let benchmark_dense = args.iter().any(|a| a == "--benchmark-dense");

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }

    println!("=== Loading dataset from {} ===", dataset_dir.display());
    let dataset =
        Dataset::load(&dataset_dir).map_err(|e| format!("failed to load dataset: {e}"))?;
    println!(
        "Loaded {} queries and {} corpus files",
        dataset.queries.len(),
        dataset.corpus.len()
    );

    // Compute corpus avgdl in tokens
    let mut total_tokens = 0usize;
    let mut doc_texts = Vec::with_capacity(dataset.corpus.len());
    for f in &dataset.corpus {
        let text = fs::read_to_string(&f.path)?;
        let tokens = sparse::tokenize(&text);
        total_tokens += tokens.len();
        doc_texts.push((f.key.clone(), text));
    }
    let real_avgdl = (total_tokens as f32) / (dataset.corpus.len() as f32);
    println!(
        "Corpus total tokens: {}, measured real avgdl: {:.3} (default is {})",
        total_tokens,
        real_avgdl,
        sparse::BM25_AVGDL
    );

    let tenant = TenantId::new_unchecked("miracl");
    let all_doc_ids: Vec<DocumentId> = dataset
        .corpus
        .iter()
        .map(|f| DocumentId::new_unchecked(&f.key))
        .collect();

    // Setup Qdrant collection for sparse
    let mut settings = QdrantSettings::from_env();
    settings.dense_dims = None; // sparse only
    let collection = if use_real_avgdl {
        "chunks__miracl_eval__sparse_avgdl".to_string()
    } else {
        "chunks__miracl_eval__sparse".to_string()
    };
    let client = settings.client()?;

    println!("Creating collection `{collection}` in Qdrant...");
    let _ = drop_collection(&client, &collection).await;
    create_collection(&client, &settings, &collection).await?;

    let index = QdrantIndex::new_with_collection(settings.clone(), collection.clone())?;

    println!(
        "Encoding and ingesting {} documents (use_real_avgdl={})...",
        doc_texts.len(),
        use_real_avgdl
    );
    let start_ingest = Instant::now();
    let mut units = Vec::with_capacity(doc_texts.len());
    for (key, content) in &doc_texts {
        let sparse_vec = if use_real_avgdl {
            encode_document_avgdl(content, real_avgdl)
        } else {
            encode_document(content)
        };
        let doc_id = DocumentId::new_unchecked(key);
        units.push(IndexUnit {
            id: ChunkId::new_unchecked(format!("{key}_0")),
            document_id: doc_id,
            tenant: tenant.clone(),
            knowledge_base_id: None,
            category: None,
            chunk_index: 0,
            page: None,
            chunk_type: ChunkType::Text,
            content: content.clone(),
            contextual_prefix: None,
            metadata: serde_json::Value::Null,
            dense: Vec::new(),
            sparse: sparse_vec,
        });
    }

    index.upsert(units).await?;
    println!(
        "Ingest completed in {:.2}s",
        start_ingest.elapsed().as_secs_f64()
    );

    // Run queries
    println!("Running {} sparse-only queries...", dataset.queries.len());
    let scope_all = make_fake_scope(tenant.clone(), all_doc_ids.clone());

    let mut trec_file = File::create(&out_path)?;
    let mut latencies_scope_all = Vec::with_capacity(dataset.queries.len());

    for query in &dataset.queries {
        let keyword_query = KeywordQuery::from_text(&query.text);
        let hybrid_query = HybridQuery {
            dense: None,
            sparse: Some(keyword_query),
            per_arm_k: 100,
            k: 100,
            fusion: Fusion::None,
        };

        let start_q = Instant::now();
        let candidates = index.search(&hybrid_query, &scope_all).await?;
        let elapsed_ms = start_q.elapsed().as_secs_f64() * 1000.0;
        latencies_scope_all.push(elapsed_ms);

        for (rank, cand) in candidates.iter().enumerate() {
            let score = cand.scores.sparse.unwrap_or(cand.scores.fused);
            writeln!(
                trec_file,
                "{} Q0 {} {} {:.6} qdrant-sparse",
                query.id,
                cand.document_id,
                rank + 1,
                score
            )?;
        }
    }
    trec_file.flush()?;
    println!("Wrote TREC run to {}", out_path.display());

    latencies_scope_all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50_all = percentile(&latencies_scope_all, 50.0);
    let p95_all = percentile(&latencies_scope_all, 95.0);
    println!(
        "Sparse latency (scope = all {} docs): p50 = {:.2} ms, p95 = {:.2} ms",
        all_doc_ids.len(),
        p50_all,
        p95_all
    );

    // Timing with 10-doc scope
    println!("Timing sparse search with 10-doc scope...");
    let mut latencies_scope_10 = Vec::with_capacity(dataset.queries.len());
    for (i, query) in dataset.queries.iter().enumerate() {
        // Pick 10 docs: gold docs first, then fill from all_doc_ids
        let qrels = dataset.qrels_for(&query.id);
        let mut ten_docs: Vec<DocumentId> = qrels
            .iter()
            .map(|q| DocumentId::new_unchecked(&q.corpus_id))
            .collect();
        let mut fill_idx = (i * 7) % all_doc_ids.len();
        while ten_docs.len() < 10 {
            let candidate_id = &all_doc_ids[fill_idx % all_doc_ids.len()];
            if !ten_docs.contains(candidate_id) {
                ten_docs.push(candidate_id.clone());
            }
            fill_idx += 1;
        }
        ten_docs.truncate(10);
        let scope_10 = make_fake_scope(tenant.clone(), ten_docs);

        let keyword_query = KeywordQuery::from_text(&query.text);
        let hybrid_query = HybridQuery {
            dense: None,
            sparse: Some(keyword_query),
            per_arm_k: 10,
            k: 10,
            fusion: Fusion::None,
        };

        let start_q = Instant::now();
        let _ = index.search(&hybrid_query, &scope_10).await?;
        let elapsed_ms = start_q.elapsed().as_secs_f64() * 1000.0;
        latencies_scope_10.push(elapsed_ms);
    }

    latencies_scope_10.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50_10 = percentile(&latencies_scope_10, 50.0);
    let p95_10 = percentile(&latencies_scope_10, 95.0);
    println!(
        "Sparse latency (scope = 10 docs): p50 = {:.2} ms, p95 = {:.2} ms",
        p50_10, p95_10
    );

    // Optional dense benchmark with 1024-dim random vectors
    if benchmark_dense {
        println!("\n=== Running Dense (1024-dim cosine HNSW) Benchmark ===");
        let dense_collection = "chunks__miracl_dense_eval__1024".to_string();
        let mut dense_settings = settings.clone();
        dense_settings.dense_dims = Some(1024);

        let _ = drop_collection(&client, &dense_collection).await;
        create_collection(&client, &dense_settings, &dense_collection).await?;
        let dense_index =
            QdrantIndex::new_with_collection(dense_settings, dense_collection.clone())?;

        let mut rng_seed = 42424242u64;
        let mut dense_units = Vec::with_capacity(dataset.corpus.len());
        for f in &dataset.corpus {
            let vec = pseudo_random_vector(&mut rng_seed, 1024);
            let doc_id = DocumentId::new_unchecked(&f.key);
            dense_units.push(IndexUnit {
                id: ChunkId::new_unchecked(format!("{}_0", f.key)),
                document_id: doc_id,
                tenant: tenant.clone(),
                knowledge_base_id: None,
                category: None,
                chunk_index: 0,
                page: None,
                chunk_type: ChunkType::Text,
                content: String::new(),
                contextual_prefix: None,
                metadata: serde_json::Value::Null,
                dense: vec,
                sparse: sparse::SparseVector::empty(),
            });
        }
        println!("Ingesting 1024-d points into `{dense_collection}`...");
        dense_index.upsert(dense_units).await?;

        // Dense search with all 2,933 docs
        let mut dense_latencies_all = Vec::with_capacity(dataset.queries.len());
        for _ in 0..dataset.queries.len() {
            let q_vec = pseudo_random_vector(&mut rng_seed, 1024);
            let query = HybridQuery {
                dense: Some(q_vec),
                sparse: None,
                per_arm_k: 10,
                k: 10,
                fusion: Fusion::None,
            };
            let start_q = Instant::now();
            let _ = dense_index.search(&query, &scope_all).await?;
            let elapsed_ms = start_q.elapsed().as_secs_f64() * 1000.0;
            dense_latencies_all.push(elapsed_ms);
        }
        dense_latencies_all.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let dense_p50_all = percentile(&dense_latencies_all, 50.0);
        let dense_p95_all = percentile(&dense_latencies_all, 95.0);
        println!(
            "Dense 1024-d latency (scope = all {} docs): p50 = {:.2} ms, p95 = {:.2} ms",
            all_doc_ids.len(),
            dense_p50_all,
            dense_p95_all
        );

        // Dense search with 10 docs
        let mut dense_latencies_10 = Vec::with_capacity(dataset.queries.len());
        for (i, _) in dataset.queries.iter().enumerate() {
            let q_vec = pseudo_random_vector(&mut rng_seed, 1024);
            let query = HybridQuery {
                dense: Some(q_vec),
                sparse: None,
                per_arm_k: 10,
                k: 10,
                fusion: Fusion::None,
            };
            let mut ten_docs: Vec<DocumentId> = Vec::with_capacity(10);
            let mut fill_idx = (i * 7) % all_doc_ids.len();
            while ten_docs.len() < 10 {
                ten_docs.push(all_doc_ids[fill_idx % all_doc_ids.len()].clone());
                fill_idx += 1;
            }
            let scope_10 = make_fake_scope(tenant.clone(), ten_docs);

            let start_q = Instant::now();
            let _ = dense_index.search(&query, &scope_10).await?;
            let elapsed_ms = start_q.elapsed().as_secs_f64() * 1000.0;
            dense_latencies_10.push(elapsed_ms);
        }
        dense_latencies_10.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let dense_p50_10 = percentile(&dense_latencies_10, 50.0);
        let dense_p95_10 = percentile(&dense_latencies_10, 95.0);
        println!(
            "Dense 1024-d latency (scope = 10 docs): p50 = {:.2} ms, p95 = {:.2} ms",
            dense_p50_10, dense_p95_10
        );

        // Clean up dense collection
        let _ = drop_collection(&client, &dense_collection).await;
    }

    Ok(())
}
