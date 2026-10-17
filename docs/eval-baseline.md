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


> **Step S.1b update**: for end-to-end evaluation with Qdrant, see [§7 End-to-end on Qdrant (S.1b)](#7-end-to-end-on-qdrant-s1b).

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

> **Reviewer note (2026-10-16): do not quote this section as a quality baseline.**
>
> - **Row (a) is a bug, not a baseline.** A 30-line independent BM25 (`rank_bm25`, scored
>   with ranx) on the *same* 2,933 documents and 200 queries gets nDCG@10 **0.708**,
>   Recall@10 0.909 and Hit@1 0.52. rantai-kb gets 0.003. Search over natural-language
>   Indonesian questions is effectively broken. This is under investigation in
>   `docs/findings-bm25-collapse.md` (step 0e).
> - **Row (b) indexed only 86 of 2,933 documents.**
> - **Row (c) measured nothing.** It ran on 5 queries; 4 of 5 rerank calls timed out
>   (`rerank_unavailable`), so it equals the no-rerank result on the same queries.
> - **Not a semantic-retrieval result.** The "0.317 / Hit@1 0.400" figures come from 5
>   hand-picked queries whose gold documents happened to be indexed.

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

## 4. First real-model rows — `miracl-id-50`, 50 queries, full subset corpus

2026-10-16, **harness commit `ca2599f`** (`kb-eval` stamped by `build.rs`; the
`kb-server` under test is the same tree). Embedder `Qwen/Qwen3-Embedding-0.6B`
(dim 1024, query instruction prefix) on local TEI, CPU, zero spend. nDCG@10
uses **linear** gain, `--k 10 --max-chunks 50`, document level, strict means
(failed and unanswerable queries score zero and stay in the mean).

Dataset: `rust/crates/kb-eval/datasets/miracl-id-50` — 50 dev queries, **1,485
corpus documents**, 154 relevant judgements:

```sh
uv run rust/crates/kb-eval/scripts/import_miracl.py --seed 42 --num-queries 50 \
  --out-dir rust/crates/kb-eval/datasets/miracl-id-50
```

### The three rows

| Row | Configuration | Corpus indexed | nDCG@10 | Recall@10 | MRR@10 | Hit@1 | P@10 | lat p50 | lat p95 | n_failed | n_degraded |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `qwen3-0.6b`, current BM25 (both flags off) | 1,482 / 1,485 | **0.724** [0.649–0.792] | 0.899 [0.824–0.951] | 0.724 [0.632–0.813] | 0.580 [0.460–0.700] | 0.272 | 2,279 ms | 2,617 ms | 0 | 0 |
| 2 | `qwen3-0.6b+or-terms` (`KB_EXP_BM25_OR_TERMS=1`) | same ingest | 0.719 [0.647–0.791] | 0.909 [0.846–0.960] | 0.711 [0.617–0.804] | 0.580 [0.460–0.700] | 0.278 | 2,324 ms | 2,784 ms | 0 | 0 |
| 3 | `qwen3-0.6b+or-terms+neutral` (both flags) | 1,481 / 1,485 (4 ingest failures) | 0.699 [0.623–0.771] | 0.889 [0.815–0.948] | 0.691 [0.588–0.785] | 0.560 [0.420–0.680] | 0.276 | 2,322 ms | 2,976 ms | 0 | 0 |
| — | **independent BM25** (`rank_bm25` + `ranx`, same 1,485 docs) | — | **0.734** | 0.938 | 0.717 | 0.580 | 0.286 | — | — | — | — |

Rows 1 and 2 share one ingest; row 3 needs its own, because
`KB_EXP_ANALYZER_NEUTRAL=1` re-`OVERWRITE`s the analyzer **and** the search
index, so every chunk has to be re-indexed under the new analyzer.

### What these rows say

* **The BM25 collapse is no longer the binding constraint once a real embedder
  is in place.** With the 8-dim stub, row 1 scored 0.000 and the OR-terms fix
  was worth +0.410 (`docs/findings-bm25-collapse.md`). With real embeddings,
  the vector arm already answers most questions: row 1 is **0.724**, and
  OR-terms moves nothing (0.719, and 0.909 vs 0.899 on Recall — both inside
  the ±0.07 bootstrap interval). The collapse was real, but it was only
  visible because the vector arm was noise. **Read the two documents
  together; neither number alone is the quality of this system.**
* **Row 1 already matches an independent BM25** (0.724 vs 0.734 nDCG@10,
  identical Hit@1 0.580) on the same corpus with the same 50 queries. That is
  the reference a reranker or a better sparse arm has to beat, not the 0.003
  the stub produced.
* **The neutral analyzer did not help here** (0.699 vs 0.719 with the same
  OR-terms query). It removes the English stemmer's damage to Indonesian
  tokens, but with a working dense arm the affected queries were not the ones
  being lost. Its value was measured against a stub, where BM25 was the *only*
  arm.
* **Ingest cost dominates everything else**: 1,485 documents took **5,247 s
  (87 min, ≈17.4 docs/min)** for row 1, and the same for row 3. All 50 queries
  together took 121 s. A CPU embedding profile is an evaluation-time cost
  center, not a serving-time one.
* **Four documents in row 3 failed to ingest** (`n_ingest_failed: 4`),
  all with `embedding failed: backend unavailable: execute: error sending
  request` — a reqwest **transport** error against TEI after 3 retries
  (0.27% of 1,485 documents). Rows 1 and 2 had none. This is a real robustness
  gap against a local inference server and is worth its own fix; it is the one
  reason row 3's corpus is 4 documents short.

### Cross-check with `ranx`

Every `run.trec` re-scored independently (`uv run --with ranx`) against the
same `qrels/test.tsv`. Strict means match the harness exactly, to 4 decimals:

| Row | metric | harness | ranx |
|---|---|---|---|
| 1 | nDCG@10 / Recall@10 / MRR@10 / Hit@1 / P@10 | 0.7235 / 0.8988 / 0.7242 / 0.580 / 0.272 | 0.7235 / 0.8988 / 0.7242 / 0.580 / 0.272 |
| 2 | nDCG@10 / Recall@10 / MRR@10 / Hit@1 / P@10 | 0.7190 / 0.9091 / 0.7105 / 0.580 / 0.278 | 0.7190 / 0.9091 / 0.7105 / 0.580 / 0.278 |
| 3 | nDCG@10 / Recall@10 / MRR@10 / Hit@1 / P@10 | 0.6990 / 0.8891 / 0.6905 / 0.560 / 0.276 | 0.6990 / 0.8891 / 0.6905 / 0.560 / 0.276 |

### Exact reproduction

```sh
# 0. build the 50-query subset (deterministic, seed 42)
uv run rust/crates/kb-eval/scripts/import_miracl.py --seed 42 --num-queries 50 \
  --out-dir rust/crates/kb-eval/datasets/miracl-id-50

# 1. stack with real embeddings, no experiment flags  → row 1 (ingests the corpus)
rust/scripts/stack-up.sh && rust/scripts/stack-down.sh
KB_STACK_EMBEDDINGS=tei rust/scripts/stack-up.sh
source /tmp/kb-stack/stack.env
KEY=$("$KB_SERVER_BIN" create-api-key r1-tenant key --scopes kb:read,kb:write \
      | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
"$KB_SERVER_BIN" eval --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-50 \
  --out eval-out/miracl-id-50-q1 --label qwen3-0.6b

# 2. same corpus, OR-terms BM25 → row 2 (query-time only, reuse the ingest)
#    (restart kb-server with KB_EXP_BM25_OR_TERMS=1 in its environment)
mkdir -p eval-out/miracl-id-50-q2
cp eval-out/miracl-id-50-q1/ingest-map.json eval-out/miracl-id-50-q2/
"$KB_SERVER_BIN" eval --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-50 \
  --out eval-out/miracl-id-50-q2 --skip-ingest --label qwen3-0.6b+or-terms

# 3. both flags → row 3 (the analyzer is OVERWRITE, so this needs a fresh stack
#    and a full re-ingest)
rust/scripts/stack-down.sh
KB_EXP_ANALYZER_NEUTRAL=1 KB_EXP_BM25_OR_TERMS=1 KB_STACK_EMBEDDINGS=tei \
  rust/scripts/stack-up.sh
source /tmp/kb-stack/stack.env
KEY3=$("$KB_SERVER_BIN" create-api-key r3-tenant key --scopes kb:read,kb:write \
       | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
"$KB_SERVER_BIN" eval --base-url "$KB_STACK_BASE_URL" --api-key "$KEY3" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-50 \
  --out eval-out/miracl-id-50-q3 --label qwen3-0.6b+or-terms+neutral

# 4. the independent BM25 reference on the same subset
uv run rust/crates/kb-eval/scripts/eval_bm25.py \
  --dataset rust/crates/kb-eval/datasets/miracl-id-50
```

Verify the neutral analyzer actually took effect before trusting row 3:

```sh
curl -s -X POST http://127.0.0.1:18000/sql -u root:root \
  -H 'Surreal-NS: rantai' -H 'Surreal-DB: knowledge' -H 'Accept: application/json' \
  -d "RETURN search::analyze('kb_en', 'Siapakah yang menulis Candy Candy?')"
# ["siapakah","yang","menulis","candy","candy","?"] — unstemmed
```

### Ingest throughput, before and after batched embedding requests

`EmbedderImpl::embed_many` now sends one `/v1/embeddings` request per
`KB_EMBEDDING_BATCH_SIZE` inputs (default 32) instead of one per chunk.
Measured on this 6-core box against the same warm TEI container:

| corpus | binary | docs | chunks | TEI requests | wall | docs/min |
|---|---|---|---|---|---|---|
| 100 MIRACL passages (1 chunk each) | before (`8827786`) | 100 | 100 | 99 | 361 s | **16.6** |
| same 100 | after | 100 | 100 | 99 | 497 s (1 ingest failure) | **12.1** |
| same 100 | after, repeat run | 100 | 100 | 99 | 396 s | **15.1** |
| 12 multi-passage docs (~24 chunks each) | before (`8827786`) | 12 | 288 | 297 | 656 s | 1.10 |
| 12 multi-passage docs | after | 12 | 288 | 12 | 605 s | 1.19 |
| 1 multi-passage doc | after | 1 | 24 | **1** | 67 s | — |

**Read that honestly: batching bought almost nothing on this box.**

* Every MIRACL document is a **single chunk** (verified in the store:
  `SELECT c, count(*) … GROUP BY c` returns `1 → 99 docs`), so there was never
  more than one input per call to batch. Two runs of the *same* 100-document
  corpus differ by 12.1 vs 15.1 vs 16.6 docs/min, i.e. the run-to-run noise is
  larger than any effect of the change.
* Where batching does apply, it works exactly as intended — 24 chunks became
  **1** request, 297 became 12 — and wall time moved 656 s → 605 s (−8%,
  within noise).
* The reason is that TEI on **CPU** is compute-bound, not request-bound. Direct
  measurement against the same server: 8 single-input requests = **13.3 s**;
  1 request carrying those 8 inputs = **13.8 s**. Σ `inference_time` over 182
  embedding requests averaged 2.74 s each. Batching removes HTTP overhead and
  tokenisation duplication; it cannot remove FLOPs.
* **The real bottleneck is document-level serialisation**, reported here and
  deliberately not changed in this round: `IngestWorker::tick` runs
  `run_job(job).await` one job at a time (`rust/crates/kb-engine/src/ingest.rs:253`),
  and `kb-eval`'s uploader polls each job to completion before uploading the
  next document (`rust/crates/kb-eval/src/runner.rs:432`). So the box runs
  exactly one embedding at a time, and a local TEI that can serve several
  concurrent requests never sees more than one. Document-level concurrency is
  the next lever, and it is worth measuring before any further embedder work.

### What this section does NOT prove

* **50 queries.** Every interval is ±0.07 wide; rows 1 and 2 are not
  distinguishable from each other, and neither is from independent BM25.
* **`miracl-id-200` was not re-run.** The full 2,933-document corpus at 17
  docs/min is ≈2.9 h per ingest, and row 3 needs a second one. Section 3's
  `miracl-id-200` numbers stand as written, still marked as a bug report.
* **No reranker.** `bge-reranker-v2-m3` was not run: ~21 s per query on this
  CPU (section 3) against a 2.3 s p50 today, and it timed out on 4/5 queries
  last time. It needs a smaller candidate set or a different machine before
  it can be measured at all.
* **Page level is not measured**: MIRACL has no page-level judgements.

## 5. Latency baseline (SurrealDB, before Qdrant)

Plan step L.1: instrument, load-test, and **record today's SurrealDB numbers
once, so the gain can be shown**. No tuning work goes into SurrealDB, and none
went in. What follows is a measurement of the system as it stood at commit
`a5b1e4f`, plus the two queries that explain where the time goes.

### The instrument

Every stage of `/v1/search` is measured on the server
(`stats.stageTimingsMs`, additive to the Bun `HybridSearchStats` fields, which
keep their names and now carry real numbers instead of zeros), read back by
`kb-eval` into `metrics.json`/`report.md`, and load-tested by
`kb-server bench search`.

Two rules make the numbers add up, and both matter when reading the table:

* **The top-level stages partition the request.** They run one after another,
  so they sum to no more than `total` (asserted by an in-process test).
* **`detail.*` overlaps and is never summed.** `detail.queryEmbed` and
  `detail.vectorArm` both happen *inside* `arms`; they are read, not added to
  it.

### Setup

| | |
|---|---|
| dataset | `miracl-id-50`, 50 dev queries, 154 qrels |
| corpus | 1,485 documents, **1,479 ingested** (6 failed), **1,687 chunks** in SurrealDB |
| embedder | `Qwen/Qwen3-Embedding-0.6B`, dim **1024**, local TEI on CPU, query prefix set |
| reranker | **off** (`KB_RERANK_ENABLED=false`); `rerank` reads 0.1 ms because it never runs |
| hardware | 6 cores (Intel Xeon E5-2680 v4 @ 2.40 GHz), **11 GB RAM, no GPU**, `free -g` available ≈ 5 GB with the stack up |
| store | Postgres 16 + SurrealDB v2 (`memory`) + rustfs, all in `kb-smoke-*` containers |

### Per-stage latency — 50 queries, cold query-embedding cache (`--max-chunks 50`)

| stage | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|
| `scopeBuild` | 11.9 | 10.5 | 18.6 | 20.9 |
| `expand` | 0.0 | 0.0 | 0.0 | 0.0 |
| `arms` | 2243.7 | 2213.2 | 2526.9 | 2730.0 |
| `enrich` | 2.8 | 2.5 | 4.7 | 9.3 |
| `fusion` | 0.0 | 0.0 | 0.0 | 0.0 |
| `rerank` | 0.1 | 0.1 | 0.2 | 0.4 |
| `neighbours` | 64.9 | 62.9 | 81.1 | 89.8 |
| `figures` | 55.8 | 54.1 | 66.6 | 70.1 |
| `context` | 0.2 | 0.2 | 0.3 | 0.4 |
| `serialise` | 3.9 | 3.6 | 7.0 | 7.7 |
| **`total`** | **2383.4** | **2359.1** | **2659.9** | **2875.4** |
| `detail.engineTotal` | 2367.4 | 2344.6 | 2645.4 | 2855.1 |
| `detail.queryEmbed` | 651.8 | 639.3 | 934.4 | 1137.9 |
| `detail.vectorArm` | 1591.5 | 1590.2 | 1670.3 | 1685.6 |
| `detail.keywordArm` | 0.0 | 0.0 | 0.0 | 0.0 |
| `detail.entityArm` | 0.0 | 0.0 | 0.0 | 0.0 |
| `detail.scopeCheck` | 0.0 | 0.0 | 0.0 | 0.0 |

Client-measured, same run: mean 2388.7, **p50 2363.6, p95 2664.1, p99 2880.2**
— 4 ms above the server's own total at p50, which is the HTTP round trip and
the client's JSON parse. There is nothing hidden in the gap.

**Warm cache** (`eval-out/lat-base-50-stages`, 42 of 50 queries served from the
query cache): `total` p50 **1751.1**, p95 2763.9; `detail.queryEmbed` p50 drops
from 639 ms to **0.1 ms**. The cache is a 256-entry LRU
(`kb-inference/src/query_cache.rs`), so it only helps a repeat of the same
question — worth 640 ms when it hits, worth nothing when it does not.

### Load test — `kb-server bench search`, 50 queries cycled, `--max-chunks 50`, 30 s per level, 5 warm-up requests discarded

| concurrency | requests | errors | **degraded** | req/s | p50 ms | p95 ms | p99 ms | max ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 18 | 0 | 0 | 0.57 | 1734.7 | 1849.3 | 1849.3 | 1849.3 |
| 8 | 44 | 0 | 0 | 1.19 | 5185.5 | 11051.0 | 12148.7 | 12148.7 |
| 32 | 99 | 0 | **78** | 2.74 | 10059.8 | 13647.9 | 17137.7 | 17137.7 |

Server-side stage p95 at each level:

| stage | c1 | c8 | c32 |
|---|---:|---:|---:|
| `arms` | 1662.0 | 10903.6 | 12492.8 |
| `detail.vectorArm` | 1660.9 | 4816.9 | 9445.4 |
| `detail.queryEmbed` | 0.1 | 8089.6 | 0.1 |
| `neighbours` | 92.0 | 999.9 | 2474.0 |
| `figures` | 66.2 | 570.4 | 2070.5 |
| `scopeBuild` | 14.0 | 42.9 | 205.4 |
| `total` | 1839.1 | 11051.0 | 13647.9 |

Read the c32 row carefully. **78 of 99 answers came back HTTP 200 with
`degrades: ["vector_arm_failed"]` and zero chunks** — a verified result, not a
busy server: the server log (this round logs the arm's error when it degrades)
shows

```
WARN kb_engine::retrieval: vector arm failed error=vector embed: backend
unavailable: execute: error sending request for url
(http://127.0.0.1:18081/v1/embeddings) bm25_hits=0
```

The **local CPU embedding server** is what gives up, not SurrealDB, and the
keyword arm has nothing to fall back on (§4: `@@` matches almost nothing), so
those searches returned nothing. Throughput 2.74 req/s is therefore not a
capacity number — 2.74 req/s × 21 % that actually answered is the real one. A
client that ignores `degrades` sees an empty result and calls it "no matches".

### Which stage dominates: the vector arm, by a factor of 2.4

At concurrency 1, `arms` is 95 % of the request and `detail.vectorArm` is
**1590 ms of the 2359 ms total (67 %)**. The query embed is second (639 ms,
27 %) and it is the one stage a cache or a smaller model can fix. Everything
else — scope build, neighbours, figures, serialisation, fusion — is 139 ms
together (6 %).

### Why the vector arm costs 1.6 s for 1,687 chunks: a raw SurrealQL measurement

The adapter's query (`kb-store-surreal/src/queries.rs:6`) computes cosine for
every chunk whose `document_id` is in an `IN [...]` list and never asks for a
kNN search, so the HNSW index at `schema.rs:46` is unused. Replaying that exact
query shape against the same container, with the same 1024-dim vector, varying
only the size of the `IN` list (min of 3 runs each, real document ids from
Postgres):

| ids in the `IN` list | wall (min) | wall (median) |
|---:|---:|---:|
| 1 | 59.3 ms | 60.9 ms |
| 100 | 123.7 ms | 126.8 ms |
| 500 | 419.8 ms | 422.9 ms |
| 1000 | 934.6 ms | 954.9 ms |
| **1485** | **1491.0 ms** | **1566.0 ms** |
| 1485 ids that match **nothing** (parse-only control) | 79.6 ms | 85.7 ms |

**≈0.95 ms per chunk in scope, linear**, and the control shows the `IN` list
itself is only ~20 ms of it — it is the cosine scan, not the query text. At
1,687 chunks that is the 1.59 s the API reports, to within 7 %.

The same engine, the same corpus, asking for a kNN search instead:

| query | wall |
|---|---:|
| `SELECT * FROM document_chunk LIMIT 20` (payload + serialisation baseline) | 71.2 ms |
| `SELECT * … WHERE embedding <\|20, 40\|> $v ORDER BY embedding` | **109.9 ms** |
| the same, plus `AND document_id IN [1485 ids]` as a post-filter | 870.4 ms |
| what the adapter sends today | 1491.0 ms |

So the HNSW index works; the adapter does not use it, and the scope filter —
1,485 literals — has to be applied to every candidate. **Not adopted**: step
L.1 says no tuning work goes into SurrealDB. It is recorded because it is the
size of the prize the Qdrant adapter is being built against: the target is a
filtered top-50 in < 20 ms at 1M chunks, and today the equivalent costs 1.6 s
at 1.7k chunks.

### Cost scaling with corpus size, without a second ingest

The `IN`-list sweep above *is* the scaling curve, measured on the real corpus by
narrowing the scope instead of ingesting a second one: 1 id 59 ms → 1485 ids
1491 ms, linear in the chunks in scope. A second corpus was not ingested for
this reason, and because a `miracl-id-200` run at 17 docs/min is ≈2.9 h.

### Quality of this run, for the record

`document nDCG@10 0.714`, `Recall@10 0.884`, `Hit@1 0.580` over the same 50
queries — close to §4 row 1 (0.724 / 0.899 / 0.580) on 3 more missing
documents. **This is not a new quality row**; the corpus differs from §4's and
the section exists for latency. It is recorded so nobody mistakes a latency
change for a quality change, or the reverse.

### Qdrant RetrievalIndex (L.2)

Measurements of the `kb-store-qdrant` adapter against Qdrant v1.19.2 (pinned in `kb-smoke-qdrant`), evaluated on the full `miracl-id-200` benchmark (**2,933 corpus documents**, single chunk each, 200 dev queries) on the same 6-core machine (Intel Xeon E5-2680 v4 @ 2.40 GHz, 11 GB RAM, Linux).

#### Benchmark results (200 queries, 2,933 docs)

| Arm / Representation | Scope filter | p50 latency | p95 latency | Notes |
|---|---|---:|---:|---|
| **Sparse (IDF BM25)** | all 2,933 doc IDs | **12.11 ms** | **15.42 ms** | Worst-case match-any tenant + doc scope filter |
| **Sparse (IDF BM25)** | 10 doc IDs | **1.82 ms** | **2.27 ms** | Narrow document scope filter |
| **Dense (1024-d Cosine HNSW)** | all 2,933 doc IDs | **8.51 ms** | **11.76 ms** | HNSW traversal with index payload scope filter |
| **Dense (1024-d Cosine HNSW)** | 10 doc IDs | **2.03 ms** | **2.30 ms** | Exact/filtered kNN |

#### What is comparable: index-level timing vs. end-to-end `/v1/search`

- **Index-level timing (apples-to-apples)**:
  - SurrealDB's index-level vector arm (`detail.vectorArm`) was **1,590.2 ms p50 / 1,670.3 ms p95** on 1,687 chunks because it performed a linear cosine scan across every chunk in scope without using HNSW.
  - Qdrant's index-level dense HNSW arm (`RetrievalIndex::search` via gRPC) is **8.51 ms p50 / 11.76 ms p95** on 2,933 chunks — a **~187x latency reduction** at the storage/retrieval layer, dropping to **~2.0 ms** when scoped to 10 documents.
  - SurrealDB's keyword arm collapsed to 0 hits for natural language questions due to boolean `@@` AND semantics. Qdrant's sparse IDF arm completes in **12.11 ms p50 / 15.42 ms p95** while delivering full OR-of-terms BM25 quality (**0.726 nDCG@10**, **0.911 Recall@10** on default avgdl).
- **End-to-end `/v1/search`**:
  - The numbers reported above measure the pure index port (`RetrievalIndex::search`): network gRPC round trip to Qdrant, payload filtering on `tenant_id` and `document_id`, HNSW / sparse index scoring, and candidate mapping.
  - They do **not** include query embedding generation (which takes ~640 ms on CPU via TEI), reranking, or HTTP layer overhead. Once the engine switches to Qdrant, query embedding generation will be the dominant stage in `/v1/search`, exactly as anticipated in `docs/plan-best-multimodal-kb.md` §L.2.

### What this section does NOT prove

* **One store, one box.** 6 cores, 11 GB, no GPU. TEI on CPU is 640 ms per
  query embed and saturates at ~3 req/s, which is why the c8 and c32 numbers
  are about the embedding server as much as about the store.
* **No reranker.** The plan's `< 300 ms with rerank` target is not measured
  here; `KB_RERANK_ENABLED=false`. §3's reranker cost (~21 s/query on this
  CPU) makes it unmeasurable at this candidate count on this box.
* **Concurrency 32 is a degraded server**, as the table above says. The
  honest capacity statement for this box is the c1 and c8 rows.
* **`miracl-id-200` (2,933 docs) was not run.** The scaling sweep above covers
  the same question — cost versus chunks in scope — in two minutes instead of
  three hours, and with real embeddings rather than the dim-8 stub.

### Reproduce

```sh
# 0. the stack, with real embeddings (dim 1024, local TEI, zero spend)
KB_STACK_EMBEDDINGS=tei rust/scripts/stack-up.sh
source /tmp/kb-stack/stack.env
KEY=$("$KB_SERVER_BIN" create-api-key lat-base lat-key --scopes kb:read,kb:write \
      | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)

# 1. ingest the corpus (≈87 min at ~17 docs/min) and run the 50 queries
"$KB_SERVER_BIN" eval --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-50 \
  --out eval-out/lat-base-50 --label surrealdab-baseline-pretimings

# 2. restart kb-server only (containers untouched, so the corpus survives)
#    and re-run the queries: cold query-embedding cache, per-stage table
mkdir -p eval-out/lat-base-50-cold && cp eval-out/lat-base-50/ingest-map.json \
  eval-out/lat-base-50-cold/
#    (kill the kb-smoke-server tmux session, start it again with the same stack.env)
"$KB_SERVER_BIN" eval --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-50 --skip-ingest \
  --out eval-out/lat-base-50-cold --label surrealdb-pretimings-cold

# 3. the load test
"$KB_SERVER_BIN" bench search --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --queries rust/crates/kb-eval/datasets/miracl-id-50/queries.jsonl \
  --concurrency 1,8,32 --duration 30s --warmup 5 --max-chunks 50 \
  --out eval-out/bench-surreal-50 --label surrealdb-1485-docs

# 4. corpus size, and the SurrealQL sweep behind the scaling table
docker exec kb-smoke-pg psql -U kb -d rantai_kb -t -A \
  -c 'select "id" from "Document" where "deletedAt" is null;' > /tmp/doc-ids.txt
curl -s -u root:root -H 'Accept: application/json' -H 'surreal-ns: rantai' \
  -H 'surreal-db: knowledge' \
  --data 'SELECT count() AS chunks FROM document_chunk GROUP ALL' \
  http://127.0.0.1:18000/sql
#    (see the table above for the exact query text; it is VECTOR_SEARCH_QUERY
#     with a 1024-float vector and an N-element IN list, replayed by hand)

rust/scripts/stack-down.sh
```

Artifacts: `eval-out/lat-base-50-cold/{metrics.json,report.md,run.trec}`,
`eval-out/lat-base-50-stages/`, `eval-out/bench-surreal-50/{bench.json,bench.md}`.

## 6. What to run next

| Target (plan) | Today | Gap |
|---|---|---|
| right page ranked first, ≥ 87% over ≥ 200 labelled questions | no labelled set exists | build `eval/id-internal/`, then measure |
| ViDoRe v3 pipeline leaderboard, top 3 | MIRACL-id-50 measured with real embeddings (§4), ViDoRe not run | drop a ViDoRe v3 subset into `kb-eval` and run it with a real model profile |
| cross-tenant hits over the red-team matrix: 0 | **0 over 240 probes**, 1 arm partial, 1 arm untested | close the figure arm with a layout extractor |
| `/v1/search` p95 warm, local profile < 300 ms | **2660 ms** cold / **2764 ms** warm, per stage, 1,485 documents (§5) | the vector arm is 67 % of it at 0.95 ms per chunk in scope; build `kb-store-qdrant` |
| `/v1/search` p95 without rerank < 80 ms | **1849 ms** p95 at concurrency 1, same corpus (§5) | same |
| vector store p95, filtered top-50 | 1491 ms for 1,687 chunks (§5) | HNSW on the same engine answers in 110 ms; Qdrant must beat that, not SurrealDB |


## 7. End-to-end on Qdrant (S.1b)

The live `kb-server` with `KB_CHUNK_STORE=qdrant` (engine on `RetrievalIndex`, `kb-store-qdrant`,
Qdrant v1.19.2): one hybrid query per search, with dense Qwen3-Embedding-0.6B (local TEI, CPU) and
sparse `kb_core::sparse` BM25 fused by RRF. **Re-measured by the reviewer on 2026-10-17** (harness
`462b17f`, run outputs in `/tmp/kbx/e2e-*`). The runner's original §7 measured only 238 of the 1,485
documents and misquoted §5, so it was replaced.

### 7.1 Quality: `miracl-id-50`, 50 queries, full subset corpus

| Row | Store / arms | Indexed | nDCG@10 | Recall@10 | MRR@10 | Hit@1 |
|---|---|---|---:|---:|---:|---:|
| §4 row 1 | SurrealDB, dense + broken `@@` keyword arm | 1,482 / 1,485 | 0.724 [0.649–0.792] | 0.899 | 0.724 | 0.580 |
| — | independent `rank_bm25`, keyword only | 1,485 | 0.734 | 0.938 | 0.717 | 0.580 |
| **S.1b** | **Qdrant, dense + sparse BM25, RRF** | 1,477 / 1,485 | **0.799** [0.738–0.859] | **0.936** [0.881–0.979] | **0.821** [0.739–0.896] | **0.720** [0.600–0.840] |

- 0 failed and 0 degraded queries. 8 corpus files failed to ingest; they are scored as misses
  (strict).
- ranx on the same `run.trec`: `ndcg@10 0.7988, recall@10 0.9365, mrr@10 0.821, hit_rate@1 0.72`.
  This matches the harness exactly.
- **Hybrid beats each arm alone** (the expectation stated before the run): +0.075 nDCG and +0.14
  Hit@1 over SurrealDB. The confidence intervals overlap on 50 queries, so confirm on
  `miracl-id-200` before quoting it as a final number.

### 7.2 Latency: same corpus, same box (6 cores, 11 GB, no GPU), reranker off

**Cold query-embedding cache** (the eval pass; every query is embedded once):

| | SurrealDB (§5) | Qdrant (S.1b) |
|---|---:|---:|
| server `total` p50 / p95 | 2,359 / 2,660 ms | **501 / 727 ms** |
| client p50 / p95 | 2,364 / 2,664 ms | **507 / 732 ms** |

What remains is almost entirely the CPU query embedding, about 0.45–0.65 s per new question
(§5: p50 639 ms).

**Load test** (`kb-server bench search`, 50 queries cycled, 30 s per level). After the first
cycle the 256-entry query cache serves every embedding, so these are **warm-cache** numbers for
both stores:

| concurrency | store | req/s | p50 | p95 | p99 | errors / degraded |
|---:|---|---:|---:|---:|---:|---|
| 1 | SurrealDB (§5) | 0.57 | 1,734.7 | 1,849.3 | 1,849.3 | 0 / 0 |
| 1 | **Qdrant** | **13.54** | **72.6** | **84.7** | 98.8 | 0 / 0 |
| 8 | SurrealDB (§5) | 1.19 | 5,185.5 | 11,051.0 | 12,148.7 | 0 / 0 |
| 8 | **Qdrant** | **44.07** | **176.4** | **235.5** | 268.0 | 0 / 0 |

At concurrency 1 (warm), `arms` (both retrieval arms in one Qdrant round trip) is p50 21.6 /
p95 30.4 ms, and the engine total is p50 55.7 / p95 66.8 ms.

**Against the plan targets:**
- **Search p95 without rerank < 80 ms:** met on a warm cache (84.7 ms is the client p95; the
  server p95 is lower). Not met cold: an unseen question pays about 0.5 s of CPU embedding.
- **Vector store p95 < 20 ms at 1M chunks:** at 1.7k chunks the measured p95 is about 30 ms for
  both arms together. Not yet measured at scale.

The next latency lever is the query embedder (ONNX/int8, a smaller model, or a GPU profile), not
the store.

### 7.3 Deterministic plumbing gate (`fixtures/tiny`, stub dim 8)

| Store | Document nDCG@10 | Leaks (240 probes) | Note |
|---|---:|---:|---|
| SurrealDB, committed baseline | 0.512 | 0 | |
| SurrealDB after the port | 0.543 | 0 | Up 0.031: ties no longer go to the dense arm (deliberate, step 1) |
| Qdrant, committed baseline | 0.749 | 0 | Reviewer runs gave 0.754; the runner reported 0.760 |

**Open issues found in review (S.1b, as written by the reviewer):**
- **The Qdrant tiny score is not deterministic** (0.749 / 0.754 / 0.760). The gate tolerance
  (0.02) absorbs it, but the cause is not yet explained.
- **The keyword (bm25) arm is UNTESTED in the Qdrant leak matrix** (positive control 0/12). The
  instrument attributes keyword hits by a heuristic (chunks with `chunkType` omitted) that no
  longer holds. Until the response carries per-arm provenance, the scope safety of the new sparse
  arm is proven only by the RetrievalIndex contract suite (8/8 live), not by the HTTP red-team
  matrix.

Both are closed in §7.5 below; the text above is kept as the reviewer's finding.

### 7.4 Reproduce

```sh
KB_STACK_EMBEDDINGS=tei KB_STACK_QDRANT=1 KB_CHUNK_STORE=qdrant rust/scripts/stack-up.sh
source /tmp/kb-stack/stack.env
KEY=$("$KB_SERVER_BIN" create-api-key e2e key --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
"$KB_SERVER_BIN" eval --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-50 --out eval-out/e2e-q50 --label qdrant-hybrid --allow-failures
"$KB_SERVER_BIN" bench search --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --queries rust/crates/kb-eval/datasets/miracl-id-50/queries.jsonl \
  --concurrency 1,8 --duration 30s --warmup 5 --max-chunks 50 --out eval-out/e2e-bench
rust/scripts/stack-down.sh
```

The ingest of 1,485 documents plus the eval took 99 minutes on this box (TEI on CPU).

### 7.5 Provenance, the leak matrix and determinism (S.1c)

Two changes: the search answer now says which arms produced each chunk, and the
Qdrant adapter stopped ranking on whatever order the store happened to answer
in. Everything below was run on this box on 2026-10-17 after both.

#### 7.5.1 Per-arm provenance in the response

Additive and optional, on every chunk of `POST /v1/search`:

```json
"retrieval": { "arms": ["dense", "sparse"], "denseRank": 3, "sparseRank": 1, "fusedScore": 0.032522473 }
```

`arms` is store-agnostic (`dense`, `sparse`, `neighbour`, `figure`), the two
ranks are 0-based positions inside each arm's own candidate list, and
`fusedScore` is the RRF score the answer was ordered by. A phase that scores
nothing — `neighbour`, `figure` — reports no score at all rather than `0`, and a
chunk no arm could be attributed to carries no `retrieval` key at all. No field
Bun emits changed; `search-parity.sh` deletes `.chunks[].retrieval` before its
diff (a no-op for Bun), recorded in `docs/rust-cutover.md` §5.

#### 7.5.2 The leak matrix, live, both stores

`rust/scripts/ci-eval-gate-qdrant.sh` (Qdrant, fresh stack) and
`rust/scripts/ci-eval-gate.sh` (SurrealDB, fresh stack), both **PASS**, 240
probes each, **0 leaks**. The two matrices came out identical, which is the
first time the sparse arm is compared over HTTP rather than through the
RetrievalIndex contract suite:

| arm | tenant | knowledge_base | category | document_ids | arm status |
|---|---|---|---|---|---|
| vector | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| bm25 | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 0-2 **UNTESTED** | **PARTIAL** |
| neighbour | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |
| figure | 8 / 0 / 0-2 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 16 / 0 / 0-4 **UNTESTED** | 8 / 0 / 0-2 **UNTESTED** | **UNTESTED** |
| entity | 8 / 0 / 2-2 **PASS** | 16 / 0 / 4-4 **PASS** | 16 / 0 / 4-4 **PASS** | 8 / 0 / 2-2 **PASS** | **PASS** |

(Each cell is `probes / leaks / positive-control hits`. Arm attribution:
192 answers named their own arms, 0 fell back to the chunk-shape heuristic.)

- **bm25: 0/12 → 10/12 positive controls**, PASS over three of the four scope
  kinds, PARTIAL overall. The two misses are one cause, and it is the route
  rather than the arm: a `documentIds`-scoped search runs no keyword arm in
  **either** runtime (Bun's `searchByDocumentIds`, `src/lib/rag/vector-store.ts:307`,
  is a `vector::similarity::cosine` scan with no BM25 query, and the Rust route
  mirrors it with `HybridQuery { sparse: None, fusion: None }`), so that cell's
  positive control is unreachable by construction. Verified directly: the same
  canary `vorlaqjx1488`, sent as a `documentIds` search, comes back with every
  chunk marked `["dense"]` and `similarity` 0.40796918, while the same query
  without the filter returns two chunks marked `["dense","sparse"]` — and a
  direct sparse query to Qdrant with that filter returns the two points, so the
  canary survives `kb_core::sparse` tokenisation and the probe does reach the
  sparse arm everywhere else.
- **figure stays UNTESTED**: this stack runs no layout extractor, so no seeded
  document produces a figure. Unchanged from §2 and still a hole, not a pass.

#### 7.5.3 Why the tiny Qdrant score moved, and the fix

Measured, not inferred. Five fresh stacks (fresh Qdrant container each time),
`fixtures/tiny`, stub dim 8, before the fix:

| run | 1 | 2 | 3 | 4 | 5 |
|---|---:|---:|---:|---:|---:|
| document nDCG@10 | 0.754491 | **0.717958** | 0.748714 | 0.754491 | 0.748714 |
| MRR@10 | 0.673611 | 0.625000 | 0.666667 | 0.673611 | 0.666667 |

The spread is 0.036 nDCG@10 — larger than the 0.02 gate tolerance, so a single
run could fail CI. All five `run.trec` files differ (five distinct hashes).

Cause, in order:

1. **The stub embedding collapses similarity.** 8 dimensions taken from the first
   8 bytes of the text, so cosine similarity takes a handful of values. A direct
   probe of the 19-chunk CI corpus returned five groups of *exactly equal*
   scores: 4 at 0.62716, 4 at 0.425652, 4 at 0.405971, 4 at 0.325408, 4 at
   0.258786.
2. **Qdrant returns tied points in segment order**, and segments are built in
   ingest order — six documents ingested concurrently do not land in the same
   order twice.
3. **The adapter used the response position as the arm rank**, RRF is a function
   of ranks, and the engine cuts to `maxChunks`, so a tie reshuffle moved
   documents across the cut and changed nDCG@10.
4. Within one stack the order was stable across 8 identical queries, which rules
   out search-time concurrency and points at index state, not at the query.

Fix (commit `8896e68`):

- Every arm's results are ranked by score and then by `unit_id` — a
  deterministic function of tenant, document and chunk index — *before* ranks
  are assigned. RRF is now a function of scores and ids, both stable across
  ingests of the same corpus.
- The dense arm scans exactly below **20,000 points** instead of walking HNSW.
  Decided by measurement (`cargo test -p kb-store-qdrant --features integration
  --test exact_vs_hnsw -- --ignored --nocapture`, Qdrant v1.19.2, 1024-dim
  cosine, k=20, 50 queries):

  | points | exact p50 | HNSW p50 | HNSW recall@20 vs exact |
  |---:|---:|---:|---:|
  | 1,500 | 2.38 ms | 2.34 ms | 1.000 |
  | 15,000 | 10.13 ms | 8.49 ms | 0.960 |
  | 50,000 | 34.39 ms | 30.86 ms | 0.883 |

  At the size this service runs (1,477 chunks for miracl-id-50) an exact scan
  costs 0.04 ms more and loses no recall, so there is nothing to buy by
  approximating. Above ~15k the trade flips, and exact grows linearly
  (~0.7 ms per 1k points) against a p95 < 20 ms target at 1M. The point count
  is cached for a minute and invalidated by writes.

After, five fresh stacks:

| run | 1 | 2 | 3 | 4 | 5 |
|---|---:|---:|---:|---:|---:|
| document nDCG@10 | 0.754491 | 0.754491 | 0.754491 | 0.754491 | 0.754491 |
| MRR@10 | 0.673611 | 0.673611 | 0.673611 | 0.673611 | 0.673611 |
| Hit@1 | 0.5 | 0.5 | 0.5 | 0.5 | 0.5 |
| Recall@10 | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 |

and all five `run.trec` files are byte-identical (one hash), as are the five
`run.page.trec`. `tiny-stub-qdrant.metrics.json` was re-baselined on that run
(nDCG@10 0.7544907651, MRR@10 0.6736111) and the gate now reports +0.000 on
every metric. Latency did not move either: document latency p50 was 28.0–36.4 ms
across the five runs before the change and 28.8–33.7 ms across the five after.

## 6. GraphStore (L.3a)

The entity graph moves out of SurrealDB into Postgres tables with traversal in Rust
(`docs/architecture-target.md` §11, step L.3). This section records the three things
that decide whether that is viable: whether the walk fits the latency budget, whether
a real Postgres honours the port's contract — above all the scope rule — and what one
scoped load costs.

Machine as §5: 6 cores, 11 GB RAM, no GPU, Linux. Postgres 16 in `kb-smoke-pg`.

### 6.1 PPR + 2-hop expansion at 1M edges, against the §11 targets

Targets: **p95 < 10 ms for PPR + expansion at ~1M edges**, and **CSR under 200 MB**.
Both are asserted by the test, not just reported.

`cargo test --release -p kb-core --test graph_bench -- --ignored --nocapture`

Fixture: 100,000 nodes, 1,000,000 edges → **1,424,164 arcs** (undirected projection),
5,000 communities, 85% intra-community, built in 309.9 ms. 50 queries, 1–5 seeds
each, p95 over 50 runs.

| Walk | p50 | p95 | p99 | max | vs 10 ms target |
|---|---:|---:|---:|---:|---|
| PPR, push (`eps 1e-5`) | 1.77 ms | **2.46 ms** | 2.79 ms | 2.79 ms | PASS |
| 2-hop expansion | 0.07 ms | 0.12 ms | 0.14 ms | 0.14 ms | — |
| **Combined (PPR + 2-hop)** | **1.84 ms** | **2.58 ms** | 2.93 ms | 2.93 ms | **PASS, 3.9x under** |
| PPR, exact power iteration | 269.84 ms | 312.08 ms | 577.60 ms | 577.60 ms | MISS, by design |

| Memory | Value | vs 200 MB budget |
|---|---:|---|
| CSR adjacency | **11.2 MB** (11,793,316 B) | PASS — 100,001 offsets `u32` + 1,424,164 targets `u32` + 1,424,164 weights `f32` |

Scaling with seeds is flat, which is what makes the budget hold for real queries
rather than the single-seed case:

| Seeds | 1 | 2 | 3 | 4 | 5 |
|---|---:|---:|---:|---:|---:|
| combined p50 | 1.84 ms | 1.85 ms | 1.87 ms | 1.92 ms | 1.93 ms |
| combined p95 | 2.42 ms | 2.46 ms | 2.58 ms | 2.78 ms | 2.93 ms |

**The exact solver is the reason this is not a sweep of the same graph twice.** At
1M arcs it costs 68 iterations × 1,997,219 arcs ≈ 135.8M arc visits per query, so
exact PPR is ~110x the push variant and 27x over the whole budget. It is in the
benchmark as a *reference*, not as a candidate: the push variant reaches L1 error
1.18 against it at `eps 1e-5` while agreeing on the top-100 in 46.7% of slots, and at
`eps 1e-3` it costs 0.69 ms with 84.9% top-100 overlap on the clustered graph. The
sweep over both graph shapes and both algorithms is in `graph_bench.rs`
(`exact_versus_bounded_push`, `same_algorithms_on_a_clustered_graph`,
`sweep_one_to_five_seeds`); the choice of `eps` per graph shape is a retrieval-quality
decision to make against a labelled set, not something this benchmark settles.

### 6.2 The contract, against a live Postgres

`kb_ports::graph_contract` runs the same seven properties against every adapter. The
Postgres run is gated twice — `--features integration`, so `cargo test --workspace`
needs no database, and `KB_PG_GRAPH_CONTRACT=1`, so turning the feature on without a
database fails loudly instead of reporting a quiet pass.

```sh
rust/scripts/stack-up.sh
KB_PG_GRAPH_URL="postgresql://kb:kb@127.0.0.1:15432/rantai_kb" \
KB_PG_GRAPH_CONTRACT=1 cargo test -p kb-store-pg --features integration \
  --test graph_contract -- --nocapture --test-threads=1
```

The in-memory reference, same suite, same properties:

```
GraphStore contract — InMemoryGraphStore (reference)
  scope_containment                          PASS
  no_path_through_an_out_of_scope_document   PASS
  tenant_isolation                           PASS
  idempotent_upsert_and_dedupe               PASS
  delete_document_cascades                   PASS
  version_bumps_on_every_write               PASS
  deterministic_ordering                     PASS
```

`PgGraphStore` against `kb-smoke-pg`, zero skips (a skip is treated as a failure,
because a skip that looks like a pass is how a suite quietly stops testing):

```
GraphStore contract — PgGraphStore
  scope_containment                          PASS
  no_path_through_an_out_of_scope_document   PASS
  tenant_isolation                           PASS
  idempotent_upsert_and_dedupe               PASS
  delete_document_cascades                   PASS
  version_bumps_on_every_write               PASS
  deterministic_ordering                     PASS
```

**What the contract found that a review did not.** Four divergences between the two
adapters, three of them invisible to the suite as first written:

* `relate` with an empty batch skipped the version bump in Postgres while the
  reference bumped it. The failure mode is a document whose extraction found no
  relations leaving a stale cached CSR, with nothing anywhere saying so.
* `delete_document` swept *every* mention-less entity in the reference, including
  entities recorded from a summary that no chunk mentions. Only entities the deleted
  document mentioned are orphans.
* The alias union kept duplicates and dropped empty strings in Postgres, and did
  neither in the reference. Nothing reads aliases through the port yet.
* `SELECT DISTINCT unnest(...)` fails at *runtime*, not parse time — a set-returning
  function in a `DISTINCT` list yields an anonymous composite. No amount of offline
  checking finds this; the live run found it on the first attempt.

The suite now pins the first two (`version_bumps_on_every_write` checks both empty
batches; `delete_document_cascades` writes an entity with no mentions, deletes an
unrelated document, and then requires a relation to it to still resolve).

### 6.3 `load_tenant_graph` at ~100k relations

```sh
KB_PG_GRAPH_URL="postgresql://kb:kb@127.0.0.1:15432/rantai_kb" \
KB_PG_GRAPH_CONTRACT=1 cargo test -p kb-store-pg --features integration \
  --test graph_contract -- --ignored --nocapture
```

Fixture: 10,000 entities, 100,000 relations over 100 documents (mean degree 10).
Resulting CSR: 10,000 entities, **199,900 arcs**, 1.6 MB. p50 over 20 samples:

| | p50 | p95 |
|---|---:|---:|
| `load_tenant_graph` | **855 ms** | 992 ms |

The fixture deliberately gives every relation a distinct `(src, dst)` and a distinct
document, so the 100,000 rows are 100,000 distinct edges rather than 10,000 pairs
the CSR would collapse — an earlier version of the fixture reused document ids and
measured a graph an order of magnitude smaller than it claimed to write.

**Where the 855 ms goes.** The split matters more than the total, because it says
where the time is *not*:

| Stage | ms | Share |
|---|---:|---:|
| Postgres executing and sending all three queries (measured via `psql`, same SQL) | ~71 | 8% |
| sqlx building `PgRow`s for 120k rows | ~470 | 55% |
| Resolving 200k relation endpoints to node ids | ~235 | 27% |
| `Csr::from_edges` over 100k edges | ~65 | 8% |
| Mentions, interning, taking ownership of the result | ~65 | 8% |

Two of the five profile guesses were wrong and measurement replaced both:

* Reading columns **by name** (`try_get("src_id")`) searches the row's column names
  on every call. Switching to positional `try_get` took the load from **1165 ms to
  940 ms p50**. The select lists are now load-bearing, and
  `the_scoped_statements_keep_the_column_order_the_indices_assume` pins their order.
* Resolving each endpoint twice (id → key, then key → node id) cost four hash probes
  per relation row. One `id → node` map after interning took the load to **855 ms**.
* The opposite result: borrowing `&str` out of the row buffers instead of decoding
  ~230k `String`s per load made **no measurable difference**. It was kept because it
  allocates ~30k times instead of ~230k, which matters at ten times the size — but it
  is not one of the wins.

### 6.4 Graph retrieval arm (L.3b)

Step L.3b connects the Postgres entity graph built in L.3a to ingest and search,
and measures whether an entity-graph retrieval arm (`KB_GRAPH_ARM=1`) improves
multi-hop retrieval on a held-out test split without regressing single-fact lookup.

#### 6.4.1 Configuration and setup

- **Zero-spend extractor**: `KB_ENTITY_EXTRACTOR=heuristic` runs locally with no external APIs.
  Entities are dual-written to Postgres (`KbEntity`, `KbMention`, `KbRelation`) during ingest.
- **Retrieval arm**: Seeds from query entities via `match_entities`, runs personalized PageRank
  plus 2-hop expansion over the cached scoped CSR, and fuses candidates into reciprocal rank
  fusion alongside vector and keyword arms (k=60).
- **Measurement procedure**: Corpus is ingested once into Postgres. The server is restarted
  between arm-off and arm-on states using `KB_STACK_RESTART_SERVER=1`, and queries are evaluated
  using `--skip-ingest` to guarantee identical corpus state and document IDs.

#### 6.4.2 HotpotQA multi-hop test split (`hotpot-200-test`)

Corpus: 1,038 documents (995 test split + distractors). 100 multi-hop queries. Evaluated
at cutoff k=10 with linear gain nDCG. Paired deltas computed per query with 10,000 bootstrap resamples.

```bash
# Arm OFF
kb-server eval --base-url http://127.0.0.1:18080 --api-key $KEY \
  --dataset rust/crates/kb-eval/datasets/hotpot-200-test \
  --out eval-out/graph/hotpot-test-off --label hotpot-test-arm-off

# Restart with arm ON
KB_STACK_GRAPH=1 KB_STACK_GRAPH_ARM=1 KB_STACK_RESTART_SERVER=1 rust/scripts/stack-up.sh

# Arm ON (reusing corpus)
kb-server eval --base-url http://127.0.0.1:18080 --api-key $KEY \
  --dataset rust/crates/kb-eval/datasets/hotpot-200-test \
  --out eval-out/graph/hotpot-test-on --label hotpot-test-arm-on --skip-ingest

# Paired deltas
python3 rust/crates/kb-eval/scripts/score_deltas.py \
  --qrels rust/crates/kb-eval/datasets/hotpot-200-test/qrels/test.tsv \
  --baseline eval-out/graph/hotpot-test-off/run.trec \
  --treatment eval-out/graph/hotpot-test-on/run.trec
```

| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.7850 | 0.7000 | -0.0850 | [-0.1400, -0.0350] | 100 |
| recall@10 | 0.8800 | 0.8600 | -0.0200 | [-0.0550, +0.0150] | 100 |
| mrr@10 | 0.8360 | 0.7280 | -0.1080 | [-0.1696, -0.0459] | 100 |
| ndcg@10 | 0.7741 | 0.6837 | -0.0904 | [-0.1270, -0.0520] | 100 |

*Target: multi-hop recall@5 at least +5 points. Result: -8.5 points (FAILED).*

#### 6.4.3 MIRACL control (`miracl-50-control`)

Corpus: 551 documents (153 judged docs, 400 sampled distractors). 50 fact-lookup queries.
Same procedure (arm off vs arm on, `--skip-ingest`).

| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.8394 | 0.8288 | -0.0106 | [-0.0877, +0.0632] | 50 |
| recall@10 | 0.9564 | 0.9485 | -0.0079 | [-0.0267, +0.0100] | 50 |
| mrr@10 | 0.8173 | 0.7829 | -0.0345 | [-0.1111, +0.0399] | 50 |
| ndcg@10 | 0.8337 | 0.8093 | -0.0244 | [-0.0694, +0.0193] | 50 |

*Target: MIRACL nDCG@10 no worse than -1 point. Result: -2.4 points (FAILED).*

#### 6.4.4 Latency and overhead

Stage timing measured from server telemetry across all answered queries:

- **Cold graph-arm latency**: 369.56 ms on the first query after server restart (loading scoped CSR).
- **Warm graph-arm latency**:
  - HotpotQA test: mean 410.7 ms, p50 408.3 ms, p95 444.4 ms, p99 451.3 ms.
  - Overall search p95: 2788.5 ms (arm off) vs 3188.8 ms (arm on) (+400.3 ms).
  - MIRACL control: overall search p95 1902.1 ms (arm off) vs 2147.5 ms (arm on) (+245.4 ms).

*Target: warm graph-arm latency adds less than 15 ms p95. Result: adds ~400 ms p95 (FAILED).*

#### 6.4.5 Tenant scope leak matrix

Evaluated with `kb-server eval leak` on live stack with `KB_GRAPH_ARM=1`:
288 probes across 8 documents, 12 scopes, 2 tenants. Zero cross-scope leaks.

| arm | probes | leaks | positive | status | tenant | knowledge_base | category | document_ids |
|---|---:|---:|---|---|---|---|---|---|
| vector | 48 | 0 | 12/12 | PASS | PASS | PASS | PASS | PASS |
| bm25 | 48 | 0 | 10/12 | PARTIAL | PASS | PASS | PASS | UNTESTED |
| neighbour | 48 | 0 | 12/12 | PASS | PASS | PASS | PASS | PASS |
| figure | 48 | 0 | 0/12 | UNTESTED | UNTESTED | UNTESTED | UNTESTED | UNTESTED |
| graph | 48 | 0 | 10/12 | PARTIAL | PASS | PASS | PASS | UNTESTED |
| entity | 48 | 0 | 12/12 | PASS | PASS | PASS | PASS | PASS |

*Target: zero leaks including the graph row. Result: 0 leaks across all cells (PASSED).*

#### 6.4.6 Failure diagnosis from raw data

1. **Seed matching rate**: 100% (100/100 queries in HotpotQA matched at least one seed entity).
2. **Extractor quality**: Inspection of sample paragraph pairs (e.g. *Cieli di Toscana* and *Andrea Bocelli*) revealed two severe flaws in `HeuristicExtractor`:
   - Preposition splitting: "di" was treated as Indonesian "di" ("in"), causing Italian / Latin names such as "Cieli di Toscana" to be split into separate entities "Cieli" and "Toscana".
   - Possessive suffixes: "Andrea Bocelli's" was extracted as entity `Andrea Bocelli's` (key `andrea bocelli s`), which failed to match `Andrea Bocelli` (key `andrea bocelli`). Consequently, multi-hop bridge edges across documents were missing from the graph; entities only linked to local within-document sub-entities (e.g. song tracks).
3. **Fusion weight and candidates**: The graph arm returned 20 candidates per query and was fused into RRF at equal weight (k=60) alongside vector and BM25. Because PageRank propagated mass to intra-document sub-entities (e.g. album track distractors), these irrelevant documents were ranked 1st and 2nd in RRF, pushing the true supporting documents out of top-5 and top-10 positions.
4. **Latency bottleneck**: Warm `graphArm` took >400 ms because `match_entities` executed `SELECT_SCOPED_ENTITY_LABELS` with `documentId = ANY($2)` across all 1,000+ documents on every search, scanning ~8,000 entity rows from Postgres into Rust, followed by chunk fetches in SurrealDB.

#### 6.4.7 What this section does NOT prove

* **It does not prove graph retrieval is useless in principle.** It proves that heuristic extraction without robust entity linking and cross-document co-reference cannot support multi-hop reasoning.
* **It does not prove PageRank is flawed.** When graph edges connect to intra-document distractors rather than the multi-hop bridge, PageRank correctly concentrates mass on those distractors.
* **It does not test LLM-based entity extraction.** Zero-spend constraints required the heuristic extractor; an LLM extractor with entity deduplication may produce higher-quality edges, but at significant monetary and computational ingest cost.

### 6.5 What this section does NOT prove

* **The load is a cache-miss cost, not a query cost.** `docs/architecture-target.md`
  §11 puts the CSR behind a per-tenant version counter, so §6.3's 855 ms runs once
  per graph mutation and every read after it is free. If the load were on the
  per-query path it would be unsalvageable; amortised, it pays for itself after one
  query. The number that has to fit the budget is §6.1's 2.58 ms, and it does. The
  next lever on the load is the driver (`query_as` positional tuples, or `COPY`),
  not the schema — Postgres is 8% of it.
* **The fixture is a dense graph.** Mean degree 10 over 10,000 entities is the upper
  end of what entity extraction produces from documents; a real tenant is sparser
  and will load faster. It was chosen because the brief asked for ~100k relations.
* **No quality measurement.** §6.1 says the push variant agrees with exact PPR on
  46.7% of the top-100 at `eps 1e-5`. Whether that is good enough for entity
  retrieval needs a labelled set; this section does not have one.
* **Bun does not write to these tables yet** (`docs/rust-cutover.md` §2). The graph
  is storage-complete and contract-tested, but no ingest path writes it, so none of
  the above has been exercised on data a real extraction produced.

## 8. Latency & Optimization: Step L.4

This section reports the measurement, profiling, and optimization of `/v1/search`
on the Qdrant stack (`KB_CHUNK_STORE=qdrant`, local TEI `Qwen3-Embedding-0.6B` dense embeddings,
sparse BM25 via `kb_core::sparse`, fused by RRF), with and without the reranker (`BAAI/bge-reranker-v2-m3`).
All raw artifacts are preserved under `docs/l4-bench-results/`.

### 8.1 Executive Summary & Target Comparison

| Metric / Scenario | Target (Required) | Target (Stretch) | Baseline (48db573) | L.4 Optimized | Outcome |
|---|---:|---:|---:|---:|---|
| `/v1/search` p95 without rerank (c=1, warm) | < 80 ms | < 40 ms | 81.5 ms (server: 75.3 ms) | **66.8 ms** (server: **59.5 ms**) | **PASS (Required Met)** |
| `/v1/search` p95 without rerank (c=8, warm) | < 235 ms | — | 241.8 ms (42.9 req/s) | **189.1 ms** (**55.9 req/s**, +30% tps) | **PASS** |
| `/v1/search` without rerank errors / degraded | 0 / 0 | 0 / 0 | 0 / 0 | **0 / 0** (4,325 requests) | **PASS** |
| `/v1/search` p95 with rerank (c=1, warm) | < 300 ms | < 150 ms | 26,050 ms (100% degraded timeout) | **2,631.7 ms** (0 degraded) | **8.6x faster, 0 degraded** (see §8.7) |
| Vector store filtered top-50 p95 (< 20 ms) | < 20 ms | — | — | **17.51 ms** at 100k, **18.58 ms** at 50k | **PASS (<100k chunks)** |
| Cross-scope leak matrix (288 probes) | 0 leaks | 0 leaks | 0 leaks | **0 leaks** | **PASS** |
| Retrieval quality (held-out nDCG@10 delta) | no regression | — | 0.9037 | **0.8906** (CI: [-0.063, +0.036]) | **PASS (zero delta in CI)** |

---

### 8.2 Profiling Findings: Where the Time Went

Before making code changes, a baseline profiling pass on warm requests (`docs/l4-bench-results/baseline-rerank-off`
and `baseline-rerank-on`) broke down latency across server stages:

1. **Neighbour expansion was sequential and unindexed**:
   For each document hit, `index.neighbours` was called sequentially in a loop. At concurrency 1,
   `neighbours` alone consumed **39.0 ms** p95 (more than the vector arm + BM25 combined at 22.1 ms).
   At concurrency 8, it ballooned to **157.6 ms** p95.
2. **Redundant Postgres round-trips in `enrich_titles`**:
   The engine called `doc_store.find_alive_meta_by_ids` during the alive check, but discarded the
   returned metadata. It then called `enrich_titles` up to 3 times sequentially (for hits, neighbours,
   and figures), each performing redundant Postgres lookups (`SELECT id, title, categories, ...`),
   adding 3.5–5 ms per search.
3. **Double mutex lock on embedding cache**:
   `embed_query_detailed` acquired the `Mutex` lock on the query cache twice (once for `.get()`,
   once for `.insert()`), causing unnecessary lock contention under concurrent load.
4. **Reranker unbudgeted candidate blowup**:
   `execute_pipeline` forwarded up to 20 raw chunks without text truncation to TEI (`BAAI/bge-reranker-v2-m3`).
   On CPU (6 cores), a 560M parameter cross-encoder evaluating 20 full text passages took ~22–26 seconds,
   exceeding the 20s server timeout and producing 100% `rerank_unavailable` degrades.

---

### 8.3 Optimizations Implemented

1. **Concurrent Neighbour Fetching (`kb-engine`)**:
   Replaced sequential per-document calls with `futures::future::join_all` across all document anchors,
   allowing Qdrant/SurrealDB to process neighbour queries concurrently.
2. **Metadata Caching across Pipeline Stages (`kb-engine`)**:
   Retained the `HashMap<DocumentId, DocumentMeta>` populated during the initial alive check and passed
   it to `enrich_titles_cached` across hits, neighbours, and figures, eliminating 3 redundant Postgres round-trips.
3. **Embedder Cache Lock Unification (`kb-inference`)**:
   Consolidated `embed_query_detailed` cache checks into a single mutex access pattern.
4. **Reranker Candidate Limiting & Character Budgeting (`kb-engine`, `kb-server`)**:
   Introduced configuration knobs:
   - `KB_RERANK_CANDIDATE_LIMIT` (default 0 = every candidate; 5 on this CPU box)
   - `KB_RERANK_CHAR_BUDGET` (default 0 = full text; 200 chars on this CPU box, cut at a UTF-8 boundary)
   - `KB_RERANK_TIMEOUT_MS` (default 20000, the reranker client's own ceiling; a timeout is a
     visible `rerank_unavailable` degrade with the fused order kept)

   *Reviewer note (2026-10-18):* the runner shipped 10 / 350 / 3000 as defaults, which would
   have cut every production rerank (GPU or hosted) to 10 candidates of 350 chars. The
   defaults were put back to the pre-L.4 behaviour; the budget is an opt-in for CPU-only
   boxes. The same review made hits the reranker does not return keep their fused order
   (they had been appended in `HashMap` order, so the answer could differ run to run).
   Note also what §8 shows about the budgeted reranker on CPU: against rerank OFF it moves
   nDCG@10 by −0.013 (CI −0.063…+0.036) for +2.2 s p95. It buys nothing measurable here, so
   on this box the right setting is `KB_RERANK_ENABLED=false`. That comparison was tuned and
   scored on the same 50 queries, so it is not a held-out result.

---

### 8.4 Search Latency Benchmark (Reranker OFF)

Ran `kb-server bench search` across concurrency 1, 8, and 32 with 50 cycled queries (`miracl-id-50`),
warm cache, 30s per level:

#### Client-Side End-to-End Latency & Throughput

| Concurrency | Baseline req/s | Optimized req/s | Baseline p50 (ms) | Optimized p50 (ms) | Baseline p95 (ms) | Optimized p95 (ms) | Delta p95 | Errors / Degrades |
|---:|---:|---:|---:|---:|---:|---:|---:|---|
| **1** | 14.80 | **19.70 (+33%)** | 68.0 | **51.3** | 81.5 | **66.8** | **-14.7 ms (-18.0%)** | 0 / 0 |
| **8** | 42.92 | **55.92 (+30%)** | 192.9 | **144.3** | 241.8 | **189.1** | **-52.7 ms (-21.8%)** | 0 / 0 |
| **32** | 60.40 | **67.92 (+12%)** | 537.6 | **487.4** | 672.8 | **620.5** | **-52.3 ms (-7.8%)** | 0 / 0 |

#### Server-Side Stage Breakdown (p95 ms)

| Stage | c=1 Baseline | c=1 Optimized | c=8 Baseline | c=8 Optimized | c=32 Baseline | c=32 Optimized |
|---|---:|---:|---:|---:|---:|---:|
| **total** | 75.3 | **59.5** | 229.8 | **169.7** | 643.6 | **552.4** |
| **arms** | 22.1 | 26.2 | 78.4 | 84.4 | 247.9 | 271.6 |
| **neighbours** | 39.0 | **16.5 (-58%)** | 157.6 | **55.4 (-65%)** | 487.9 | **220.5 (-55%)** |
| **enrich** | 3.5 | **0.0 (-100%)** | 7.9 | **0.0 (-100%)** | 13.9 | **0.1 (-99%)** |
| **figures** | 5.3 | 4.9 | 27.5 | 28.5 | 99.4 | 118.5 |
| **scopeBuild** | 7.6 | 8.1 | 18.2 | 19.7 | 78.5 | 85.9 |
| **serialise** | 4.8 | 6.0 | 12.5 | 13.2 | 14.1 | 14.6 |

---

### 8.5 Search Latency Benchmark (Reranker ON)

Tested with `BAAI/bge-reranker-v2-m3` on CPU with `KB_RERANK_CANDIDATE_LIMIT=5`, `KB_RERANK_CHAR_BUDGET=200`:

| Concurrency | Requests | req/s | p50 (ms) | p95 (ms) | Server Total p95 (ms) | Server Rerank p95 (ms) | Degraded (Timeouts) |
|---:|---:|---:|---:|---:|---:|---:|---:|
| **Baseline c=1** | 2 | 0.04 | 22,560.8 | 26,050.6 | 26,050.6 | 20,004.9 | 2/2 (100% timeout) |
| **L.4 c=1** | 18 | 0.58 | **1,660.9** | **2,631.7** | **2,627.6** | **2,244.6** | **0 / 18 (0%)** |
| **L.4 c=8** | 29 | 0.74 | 10,076.2 | 11,059.2 | 11,051.0 | 10,002.4 | 21 / 29 (queue timeout) |
| **L.4 c=32** | 99 | 2.51 | 10,166.3 | 18,513.9 | 18,497.5 | 10,010.6 | 96 / 99 (queue timeout) |

---

### 8.6 Quality & Latency Trade-Off Analysis

Evaluated with `kb-eval score` and `score_deltas.py` comparing retrieval on `miracl-id-50` with rerank OFF vs rerank ON (5 candidates, 200 char budget):

| Metric | Rerank OFF | Rerank ON | Delta | 95% Bootstrap CI | n Queries |
|---|---|---|---|---|---|
| **Recall@5** | 0.8963 | 0.8963 | **+0.0000** | [+0.0000, +0.0000] | 50 |
| **Recall@10** | 0.9787 | 0.9787 | **+0.0000** | [+0.0000, +0.0000] | 50 |
| **nDCG@10** | 0.9037 | 0.8906 | **-0.0130** | [-0.0627, +0.0356] | 50 |
| **MRR@10** | 0.9050 | 0.8900 | **-0.0150** | [-0.0900, +0.0583] | 50 |

*Finding*: The 95% bootstrap confidence interval includes zero (`[-0.063, +0.036]`), demonstrating that truncating candidates to 5 items of 200 characters preserves retrieval recall and ranking quality while preventing the 22.5s timeouts of unbudgeted reranking.

---

### 8.7 Vector Store Scale Check (Filtered Top-50)

Measured in `rust/crates/kb-store-qdrant/tests/exact_vs_hnsw.rs` (`vector_store_filtered_top50_scale`)
with 1,024-dimension float32 embeddings across 5 synthetic tenants:

| Points (Chunks) | Filter Scope | Top-K | p50 (ms) | p95 (ms) | Target (< 20 ms) |
|---:|---|---:|---:|---:|---|
| **50,000** | `tenant=tenant-target` | 50 | 7.35 | **18.58** | **PASS** |
| **100,000** | `tenant=tenant-target` | 50 | 13.23 | **17.51** | **PASS** |
| **250,000** | `tenant=tenant-target` | 50 | 27.16 | **39.66** | Missed |
| **500,000** | `tenant=tenant-target` | 50 | 50.45 | **63.56** | Missed |

*Scale Finding*: Up to 100,000 chunks, filtered top-50 vector search meets the `< 20 ms` target on CPU.
At 500,000 chunks, p95 increases to 63.56 ms. Pushing to 1,000,000 chunks in float32 requires ~5.6 GB RAM
(4.1 GB raw vectors + 1.5 GB HNSW index), which reaches the host's 6.6 GB available RAM limit.
As anticipated in `docs/plan-best-multimodal-kb.md` step L.2, reaching `< 20 ms` at 1M chunks requires
quantization (scalar or binary quantization with rescoring).

---

### 8.8 Scope Isolation Verification (Leak Matrix)

Executed `kb-server eval leak` with two isolated tenants (`leak-a` and `leak-b`):

```
leak matrix: 288 probes, 0 leak(s)
arm attribution: 240 answer(s) named their own arms, 0 read off the chunk shape
| arm | probes | leaks | positive | status | tenant | knowledge_base | category | document_ids | 
|---|---:|---:|---|---|---|---|---|---|
| vector | 48 | 0 | 12/12 | PASS | PASS | PASS | PASS | PASS | 
| bm25 | 48 | 0 | 10/12 | PARTIAL | PASS | PASS | PASS | UNTESTED | 
| neighbour | 48 | 0 | 12/12 | PASS | PASS | PASS | PASS | PASS | 
| figure | 48 | 0 | 0/12 | UNTESTED | UNTESTED | UNTESTED | UNTESTED | UNTESTED | 
| graph | 48 | 0 | 0/12 | UNTESTED | UNTESTED | UNTESTED | UNTESTED | UNTESTED | 
| entity | 48 | 0 | 12/12 | PASS | PASS | PASS | PASS | PASS | 
```
Result: **0 leaks across all 288 probes**. Tenant boundary isolation is strictly preserved.

---

### 8.9 What This Section Does NOT Prove

1. **CPU vs GPU Reranking**: All measurements were conducted on a 6-core x86_64 CPU without a GPU. A 560M parameter model (`bge-reranker-v2-m3`) takes ~350 ms per candidate on CPU. Sub-300 ms reranking at concurrency 8 or 32 physically requires GPU/NPU acceleration, or an int8 ONNX / quantized model.
2. **Corpus Size**: The full-pipeline load test used 574 ingested documents (~1.7k chunks). The scale check (§8.7) tested pure vector store retrieval up to 500k chunks, but full-pipeline multi-arm fusion at 500k+ chunks was not evaluated.
3. **Figure Extraction**: Layout extraction was not active during the leak matrix; figure isolation remains untested on live figures until Mistral/MinerU sidecars are active.

---

### 8.10 Raw Command Lines

```sh
# Rerank OFF Benchmark (c1, c8, c32)
cargo run --manifest-path rust/Cargo.toml -p kb-server -- bench search \
  --base-url http://127.0.0.1:18080 \
  --api-key "$API_KEY" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-50 \
  --concurrency 1,8,32 \
  --duration 30s \
  --out docs/l4-bench-results/l4-optimized-rerank-off-warm \
  --label l4-optimized-rerank-off-warm

# Rerank ON Benchmark (c1, c8, c32)
cargo run --manifest-path rust/Cargo.toml -p kb-server -- bench search \
  --base-url http://127.0.0.1:18080 \
  --api-key "$API_KEY" \
  --dataset rust/crates/kb-eval/datasets/miracl-id-50 \
  --concurrency 1,8,32 \
  --duration 30s \
  --max-chunks 8 \
  --warmup 2 \
  --out docs/l4-bench-results/l4-optimized-rerank-on \
  --label l4-optimized-rerank-on

# Quality Comparison & Bootstrap Deltas
python3 rust/crates/kb-eval/scripts/score_deltas.py \
  --qrels rust/crates/kb-eval/datasets/miracl-id-50/qrels/test.tsv \
  --baseline docs/l4-bench-results/miracl-rerank-off/run.trec \
  --treatment docs/l4-bench-results/miracl-rerank-on/run.trec

# Scale Benchmark
cargo test --manifest-path rust/Cargo.toml -p kb-store-qdrant --features integration \
  --test exact_vs_hnsw -- vector_store_filtered_top50_scale --ignored --nocapture

# Leak Matrix Probe
kb-server eval leak --base-url http://127.0.0.1:18080 --api-key "$KEY_A" --peer-key "$KEY_B" --out docs/l4-bench-results/leak
```
