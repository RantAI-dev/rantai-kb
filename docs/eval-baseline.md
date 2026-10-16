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

## 5. What to run next

| Target (plan) | Today | Gap |
|---|---|---|
| right page ranked first, ≥ 87% over ≥ 200 labelled questions | no labelled set exists | build `eval/id-internal/`, then measure |
| ViDoRe v3 pipeline leaderboard, top 3 | MIRACL-id-50 measured with real embeddings (§4), ViDoRe not run | drop a ViDoRe v3 subset into `kb-eval` and run it with a real model profile |
| cross-tenant hits over the red-team matrix: 0 | **0 over 240 probes**, 1 arm partial, 1 arm untested | close the figure arm with a layout extractor |
| `/v1/search` p95 warm, local profile < 300 ms | 354 ms with an 8-dim stub, 6 documents | re-measure with real embeddings on a real corpus |
