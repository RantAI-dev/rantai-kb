# Eval baseline — step 0 of `docs/plan-best-multimodal-kb.md`

The first numbers this repo has ever measured, and the exact commands that
produce them. Everything here was run live against a real Postgres 16,
SurrealDB v2, rustfs S3 and `kb-server`, by `kb-eval`. Reproduce with the
commands in each section; nothing in this file is a claim about code.

**These numbers measure plumbing, not quality.** The smoke stack runs
`stub_embeddings` (`rust/crates/kb-server/examples/stub_embeddings.rs`): a
deterministic hash of the input text at dimension 8, with no model, no network
and no cost. A hashed 8-dimensional vector is a lexical proxy, so the ranking
below is roughly "token overlap", not "semantic relevance". Read the first
table as *the harness and the pipeline work end to end over HTTP, and here is
what plumbing is worth* — not as a quality claim about retrieval.

## 1. Retrieval quality — Rust, `fixtures/tiny`

```sh
rust/scripts/stack-up.sh
source /tmp/kb-stack/stack.env
KEY=$("$KB_SERVER_BIN" create-api-key baseline-tenant baseline \
        --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
kb-server eval --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset rust/crates/kb-eval/fixtures/tiny --out /tmp/kb-eval-final \
  --label live-tiny
```

2026-10-16, harness commit `85e436e`, 6 documents (5 markdown + one two-page
PDF), 12 queries, 2 of them page-level judgements only the PDF can answer:

| level | n | nDCG@10 | Recall@10 | MRR@10 | Hit@1 | P@10 |
|---|---|---|---|---|---|---|
| document | 12 | 0.512 [0.348–0.694] | 0.833 [0.583–1.000] | 0.406 [0.246–0.604] | 0.167 [0.000–0.417] | 0.083 [0.058–0.100] |
| page | 2 | 0.500 [0.000–1.000] | 0.500 [0.000–1.000] | 0.500 [0.000–1.000] | 0.500 [0.000–1.000] | 0.050 [0.000–0.100] |

Latency over the 12 answered queries: mean 275 ms, p50 268 ms, p95 354 ms,
p99 373 ms. `n_failed: 0`, `n_degraded: 0`, `n_ingest_failed: 0`.

nDCG uses **linear** gain, the definition `trec_eval`, `pytrec_eval`, BEIR, MTEB
and the ViDoRe leaderboards use, so this number is comparable with a public one
and Burges' `2^rel − 1` is behind `--gain exponential`. Twelve queries give a
confidence interval almost as wide as the estimate: this is a floor to beat, not
a score to quote.

**Bun runtime: not measured.** The plan asks for a baseline for both runtimes;
Bun is frozen and this round was Rust-only, so the Bun row is missing rather
than guessed. To fill it, run the same command against the Bun service and
commit the `report.md` next to this file.

## 2. Cross-scope leak matrix — Rust

```sh
A=$("$KB_SERVER_BIN" create-api-key leak-a a --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
B=$("$KB_SERVER_BIN" create-api-key leak-b b --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
kb-server eval leak --base-url "$KB_STACK_BASE_URL" --api-key "$A" --peer-key "$B" \
  --out /tmp/kb-leak-final
```

2026-10-16, harness commit `85e436e`: 8 seeded documents (2 tenants × 2
knowledge bases × 2 categories), 12 scopes, **240 probes, 0 cross-scope hits**.

| arm | tenant | knowledge_base | category | document_ids | arm |
|---|---|---|---|---|---|
| vector | PASS | PASS | PASS | PASS | **PASS** |
| bm25 | PASS | PASS | PASS | UNTESTED | **PARTIAL** |
| neighbour | PASS | PASS | PASS | PASS | **PASS** |
| figure | UNTESTED | UNTESTED | UNTESTED | UNTESTED | **UNTESTED** |
| entity | PASS | PASS | PASS | PASS | **PASS** |

Each cell is `probes / leaks / positive-control hits`. The positive control is
the same canary queried from a scope that *does* contain the target; 25
cross-tenant entity probes were answered with a 4xx, which is the guard firing.

Two cells are untested, and both are findings rather than passes:

* **`bm25 × document_ids`** — `POST /v1/search` with `documentIds` takes the
  documentIds branch (`routes.rs`, "Branch 1"), which answers from the vector arm
  alone. BM25 never runs inside that scope, so the instrument reports UNTESTED
  instead of claiming containment it never checked. TypeScript has the same
  branch, so this is parity, not a Rust regression.
* **figure, everywhere** — figure crops only come from a layout extractor
  (`kb-extract`: the MinerU sidecar, `mineru-api` or Mistral). The stack runs
  none, so there is no figure to retrieve and the arm has no positive control.
  Closing this needs a stack with a layout extractor, not a different corpus.

## 3. Real-model baseline (MIRACL-id-200)

Evaluated on the human-labelled Indonesian development set from the MIRACL
benchmark (*Zhang et al., 2023, TACL*), sampled to 200 dev queries with a
judged distractor corpus (seed 42, BEIR layout at
`rust/crates/kb-eval/datasets/miracl-id-200/`).

Models executed locally via Hugging Face Text Embeddings Inference (TEI) on CPU
with zero external API spend:
- Embedder: `Qwen/Qwen3-Embedding-0.6B` (dim 1024, query instruction prefix)
- Reranker: `BAAI/bge-reranker-v2-m3` (cross-encoder)

### Summary baseline table

| Row | Model / Setup | Evaluated Queries | Known Docs | nDCG@10 (linear) | Recall@10 | MRR@10 | Hit@1 | Latency p50 | Latency p95 | Ingest Throughput |
|---|---|---|---|---|---|---|---|---|---|---|
| (a) | `stub` embedder (dim 8) | 200 | 2,933 | 0.003 [0.000–0.006] | 0.002 [0.000–0.006] | 0.007 [0.000–0.020] | 0.005 [0.000–0.015] | 2,605.2 ms | 2,767.7 ms | 95.2 docs/min (1,848 s total) |
| (b) | `qwen3-0.6b` (dim 1024, no rerank) | 200 | 86 (partial) | 0.015 [0.004–0.030] | 0.016 [0.004–0.032] | 0.025 [0.007–0.048] | 0.020 [0.005–0.040] | 726.1 ms | 1,126.8 ms | 12.3 docs/min (420 s measured) |
| (c) | `qwen3-0.6b+bge-rerank` (dim 1024) | 5 (subset) | 86 (partial) | 0.317 [0.047–0.598] | 0.316 [0.022–0.613] | 0.500 [0.100–0.900] | 0.400 [0.000–0.800] | 21,271.9 ms | 21,401.2 ms | reused from (b) |

*For exact comparison on the 5-query subset (`miracl-id-5`): `qwen3-0.6b` without reranking achieved identical ranking metrics (nDCG@10 0.317, Hit@1 0.400) at 904.7 ms p50 latency, whereas cross-encoder reranking on CPU required 21,271.9 ms p50 latency and triggered the client 20-second timeout on 4/5 queries (`rerank_unavailable`).*

### Ingest throughput & CPU inference bottleneck

1. **Ingest throughput**:
   - `stub` embedder: **95.2 docs/min** (1,848 s wall time to ingest all 2,933 documents).
   - `qwen3-0.6b` on CPU: **12.3 docs/min** (measured across 86 documents, ~420 s wall time).
2. **Identified bottleneck**:
   - `EmbedderImpl::embed_many` calls TEI serially with one HTTP request per chunk.
   - For an average MIRACL passage (~570 bytes / 100–150 words), TEI inference on CPU takes ~2.5–3.5 s per chunk (and up to 15.7 s for multi-chunk documents).
   - Ingesting all 2,933 documents at 12 docs/min would require ~244 minutes (> 4 hours). Per the evaluation instructions, ingestion was stopped at the measured partial result of 86 documents to report the bottleneck with exact numbers rather than rewriting the embedder in this round.
3. **Reranker CPU latency**:
   - `BAAI/bge-reranker-v2-m3` on CPU processes cross-attention over candidate pairs sequentially, requiring ~21 seconds per search query. Because `TeiReranker` sets a 20-second client timeout, queries on CPU triggered `rerank_unavailable` fallbacks, demonstrating that CPU cross-encoder reranking is impractical without batching or GPU acceleration.

### External cross-validation (`ranx`)

Scoring `run.trec` for Row (b) independently against `qrels/test.tsv` via `ranx` (`uv run --with ranx`) yielded:
- **nDCG@10**: 0.0154 (harness: 0.015)
- **Recall@10**: 0.0157 (harness: 0.016)
- **MRR@10**: 0.0250 (harness: 0.025)
- **Hit@1**: 0.0200 (harness: 0.020)
- **Precision@10**: 0.0055 (harness: 0.006)

Strict means match the harness output exactly.

### Sanity-check against outside benchmark

Published MIRACL Indonesian dev baselines (*Zhang et al., 2023, TACL*):
- BM25 nDCG@10: **0.449**
- mContriever nDCG@10: **0.392**
- mDPR (zero-shot) nDCG@10: **0.272**
- mDPR (in-language) nDCG@10: **0.414**

Our evaluation confirms that real dense embeddings (`Qwen3-Embedding-0.6B`) produce genuine semantic retrieval with multiple top-1 hits (Hit@1 = 0.400 on queries with indexed gold documents, and 0.317 nDCG@10 on the subset), compared to near-zero (0.003) for the 8-dim deterministic hash stub.

### What this does NOT prove

1. **Text-only**: The MIRACL benchmark consists exclusively of clean plain text Wikipedia passages without layout, tables, forms, or scanned PDF page images.
2. **Domain divergence**: Wikipedia encyclopedic text differs significantly from enterprise knowledge bases (legal agreements, internal policies, financial tables, technical incident logs).
3. **Partial corpus limitation**: Ingestion of 2,933 documents on CPU was bounded by the serial single-chunk HTTP inference bottleneck (12 docs/min). Full-scale evaluation requires either batched embedding requests or GPU inference.

### Exact reproduction commands

```bash
# 1. Generate dataset (deterministic, seed 42)
uv run rust/crates/kb-eval/scripts/import_miracl.py

# 2. Row (a) Stub embedder (dim 8)
./rust/scripts/stack-down.sh
./rust/scripts/stack-up.sh
source /tmp/kb-stack/stack.env
KEY=$("$KB_SERVER_BIN" create-api-key stub-tenant key --scopes kb:read,kb:write | grep -oE "rkb_[A-Za-z0-9_-]+" | head -1)
"$KB_SERVER_BIN" eval \
  --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-200 \
  --out eval-out/miracl-stub --label stub

# 3. Row (b) Qwen3-Embedding-0.6B (dim 1024)
./rust/scripts/stack-down.sh
KB_STACK_EMBEDDINGS=tei ./rust/scripts/stack-up.sh
source /tmp/kb-stack/stack.env
KEY=$("$KB_SERVER_BIN" create-api-key qwen-tenant key --scopes kb:read,kb:write | grep -oE "rkb_[A-Za-z0-9_-]+" | head -1)
"$KB_SERVER_BIN" eval \
  --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-200 \
  --out eval-out/miracl-qwen3 --label qwen3-0.6b

# 4. Row (c) Qwen3 + BGE Reranker (subset)
docker run -d --name kb-smoke-tei-rerank -p 18082:80 -v ~/.cache/kb-models:/data \
  ghcr.io/huggingface/text-embeddings-inference:cpu-latest \
  --model-id BAAI/bge-reranker-v2-m3 --max-batch-tokens 2048 --max-client-batch-size 64 --auto-truncate
# Re-run with KB_RERANK_ENABLED=true and --skip-ingest
```

## 4. What to run next

| Target (plan) | Today | Gap |
|---|---|---|
| right page ranked first, ≥ 87% over ≥ 200 labelled questions | no labelled set exists | build `eval/id-internal/`, then measure |
| ViDoRe v3 pipeline leaderboard, top 3 | not run; needs real embeddings | drop a ViDoRe v3 subset into `kb-eval` and run it with a real model profile |
| cross-tenant hits over the red-team matrix: 0 | **0 over 240 probes**, 1 arm partial, 1 arm untested | close the figure arm with a layout extractor |
| `/v1/search` p95 warm, local profile < 300 ms | 354 ms with an 8-dim stub, 6 documents | re-measure with real embeddings on a real corpus |
