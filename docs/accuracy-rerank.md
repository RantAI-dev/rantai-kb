# Rantai-kb's "right passage first": the fix, the measurement, and the reranker bake-off

Started 2026-10-22. Two questions, answered in order:

1. **Can the engine put the right passage first more often?** (`miracl-id-200`,
   200 queries, 2,933 passages, 640 qrels.) The ablation of 2026-10-22 said
   the cause was the fusion, plus a second, smaller issue: the embedded text
   had no title in it. Both were fixed in the Rust engine and re-measured live.
2. **Would a reranker add anything on top?** A bake-off over the fixed
   engine's own candidate list: hosted rerankers over all 200 queries, local
   CPU models over a 20-query sample.

Everything below is measured, not estimated. The predicted numbers come from
the offline model (`kf_vplane_ablate.py`), the live numbers from a running
`kb-server` (release build) measured over HTTP by `kb-eval`.

---

## 1. What changed in the engine

| Change | Where | Why |
|---|---|---|
| The fused order and `scores.fused` are **Qdrant's own RRF** (`1/(2+rank)`, 0-based) instead of a re-sort of Qdrant's list with `k = 60` | `rust/crates/kb-store-qdrant/src/index.rs`; the constant and the formula live in `rust/crates/kb-core/src/fusion.rs` (`RRF_K`, `rrf_contribution`) | a large `k` flattens the difference between rank 1 and rank 3, so a passage both arms like barely beats one a single arm likes |
| The in-memory reference index, the SurrealDB adapter and the test fakes compute **the same** arithmetic | `kb-engine/src/in_memory_index.rs`, `kb-store-surreal/src/retrieval_index.rs`, `kb-engine/src/fakes.rs` | the port has one fusion; an adapter that fuses in process and the store that fuses server-side have to agree on the *numbers*, not just the order |
| A chunk is embedded as **`"{title}\n\n{content}"`**, with the title from the upload, else the document's own H1, else the filename stem, and the chunk's **heading path** after the title | `kb-core/src/title.rs`, `kb-engine/src/ingest.rs` | the embedding is the only thing the dense arm sees of a chunk; a chunk with no idea what document it belongs to cannot be found by a question about that document |
| The chunker carries the heading path (and no longer builds it from the heading's `#` run, which left every heading name empty) | `kb-core/src/chunk/document.rs`, `kb-core/src/chunk/smart.rs` | the heading path is what the embedding text puts after the title |
| `kb-eval` uploads the dataset's title with the document | `kb-eval/src/dataset.rs`, `kb-eval/src/runner.rs` | MIRACL's corpus files are Wikipedia passages that start with `# Title`; the file name is a docid (`1000580_1`) and is not a title |

The keyword arm's text is **unchanged**: the ablation showed a title in the
sparse arm did not help.

---

## 2. Part A: predicted vs live

### 2.1 The prediction (offline, before the live run)

`kf_vplane_ablate.py` rebuilds the engine's ranking offline from the same
vectors, so each design difference can be switched on alone. The rows below
are from `docs/kf-vplane-eval/ablation/ablation.md` (10,000 bootstrap
resamples, seed 42).

| Config | nDCG@10 | MRR@10 | Hit@1 | Recall@10 |
|---|---:|---:|---:|---:|
| KF fixed (reproduced) | 0.813 | 0.821 | 0.720 | 0.964 |
| rantai-kb as it was (reproduced, live tie-break) | 0.802 | 0.801 | 0.690 | 0.964 |
| rantai-kb, fused the way Qdrant fuses (k=2) | 0.809 | 0.817 | 0.715 | 0.966 |
| **the new engine: k=2 + the new embedding text, pool 50** | **0.818** | **0.831** | **0.740** | **0.959** |
| the new embedding text with the old k=60 re-sort (the control) | 0.801 | 0.797 | 0.685 | 0.955 |

The control matters: **the title alone is worth nothing** (0.801 vs 0.802).
The fusion is the cause; the title only pays once the fusion is fixed. The
live engine's own pool is 20 candidates per arm, and that row is
0.815 / 0.831 / 0.740 / 0.951 — the pool is worth 0.002.

### 2.2 The live run

*(filled in from the run below)*

### 2.3 What the live run can and cannot show

The offline model and the live engine differ in three places, and only one of
them is a bug:

- **The tie-break.** The live engine breaks a structural RRF tie (ranks (0,1)
  and (1,0) sum to the same score) on the document id, which is a UUID. The
  offline model's "order" tie-break is the dense arm's insertion order. The
  ablation measured the tie-break as innocent (0.809 either way) but it is not
  zero.
- **Approximate search.** The dense arm scans exactly below 20,000 points, and
  this corpus is 3,593 chunks, so the live dense arm is exact too.
- **Extraction.** The live run extracts the uploaded `.md` file; the offline
  model uses the same file, read directly.

---

## 3. Part B: the reranker bake-off

### 3.1 What was reranked

The candidate list is the new engine's own fused list, at chunk granularity,
30 chunks per query, **full chunk text** (not truncated) — the same text the
engine hands its own reranker (`chunk.content`). The list is dumped by
`kf_vplane_ablate.py` (`candidates.jsonl`) from the best measured config
("new: k=2 + emb text + pool 50, live tie-break").

The baseline is that same list in fused order, deduplicated to documents
exactly as the engine's own run file is. Every reranker is therefore measured
against **the same pool**, and the delta is the reranker's doing and nothing
else.

Scoring is at the **document** level (the level the qrels are written at):
nDCG@10, MRR@10, Hit@1, Recall@10, with a paired bootstrap over queries
(10,000 resamples, seed 42) against no rerank.

### 3.2 The candidates, their licences, and what they cost

| Candidate | Where | Licence | Price |
|---|---|---|---|
| `voyageai/rerank-3` | OpenRouter's rerank router (`POST https://openrouter.ai/api/v1/rerank`) | proprietary hosted API (Voyage AI / MongoDB); no weights distributed | $0.05 / 1M input tokens (measured) |
| `nvidia/nemotron-3.5-rerank-8b:free` | the same endpoint | NVIDIA Open Model License (hosted by NVIDIA) | free, but 20 requests a minute per key |
| `qwen/qwen3-reranker-8b` | the same endpoint | **Apache-2.0** (Qwen3) | $0.20 / 1M input tokens (measured) |
| `Qwen/Qwen3-Reranker-0.6B` | this box, CPU, torch | **Apache-2.0** (Qwen3) | free |
| `BAAI/bge-reranker-v2-m3` | this box, CPU, Text Embeddings Inference | MIT (BAAI) | free |

`jina-reranker-v3.5` was **not** used: its weights are CC-BY-NC-4.0, i.e.
non-commercial only, and this is not a licence a service can ship under
without asking.

Cohere's `rerank-v3.5` / `rerank-4-fast` are on the same endpoint and are
**out of budget**: they are priced per *search unit* ($0.001 and $0.002 per
query-document pair), so 200 queries × 30 documents is $6.00 and $12.00
respectively, against a $0.50 budget for the whole task. The number is
measured, not quoted: a probe call returns `usage.search_units` and
`usage.cost`.

### 3.3 Results

*(filled in from the runs below)*

### 3.4 The spend

*(filled in)*

---

## 4. Every command

*(filled in)*

---

## 5. What was not done

*(filled in)*
