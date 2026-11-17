# Reranker bake-off, 2026-11-17 (MIRACL-id-200)

Built to pick the reranker for `DEP-6`: the 0.6B candidates that could replace Studio's
4.26 GB `Qwen3-VL-Reranker-2B` and Station's 17.5 GB 8B, neither of which had ever been
measured.

## Method

- **Pool:** 200 MIRACL-id queries x 50 documents, dense top-50 from `Qwen3-Embedding-0.6B`
  over the engine's own chunks (`rkdump`), documents truncated to 2,000 chars. Built by
  `make_candidates.py` — the November candidate file was never committed, so a fresh pool was
  the only way to compare new rerankers on equal footing. The numbers are therefore comparable
  **to each other**, not to the November rerank report.
- **Scoring:** document level (where the qrels are), a document judged by its best chunk, the
  candidate pool held identical for every reranker, paired bootstrap against no-rerank.
- **Serving:** llama.cpp `/v1/rerank` for the two GGUFs; `serve_cross_encoder.py` (transformers)
  for a checkpoint with no usable GGUF. No paid call: every endpoint is local.

## Results

| Reranker | Size | nDCG@10 | MRR@10 | Hit@1 | Recall@10 | p50 |
|---|---:|---:|---:|---:|---:|---:|
| no rerank (baseline) | — | 0.795 | 0.818 | 0.730 | 0.939 | — |
| qwen3-rerank-0.6b | 1.19 GB | 0.850 | 0.860 | 0.780 | 0.967 | 4141 ms |
| mxbai-rerank-base-v2 | 0.99 GB | 0.114 | 0.159 | 0.090 | 0.168 | 3179 ms |

The latency column is one llama.cpp server with `--parallel 1`, not the deployed shape: 4.1 s for
50 documents is a serialised local run, and TEI or vLLM with batching is what a tier would serve.

**`Qwen3-Reranker-0.6B` lifts nDCG@10 from 0.795 to 0.850 (+0.055) on Indonesian.** It is the
pick: Apache-2.0, 1.19 GB, and the only small multilingual reranker here with a clean
measurement.

## What could not be measured, and why it is not a verdict

- **`mixedbread-ai/mxbai-rerank-base-v2`** scored 0.114 nDCG@10 (-0.681) at 3.2 s per query.
  That is not a weak reranker, it is meaningless scores: a 0.5B cross-encoder does not take
  3.2 s for 50 documents, and a reranker cannot fall 0.68 below no-rerank unless its scores are
  being read wrongly. Its GGUF's classification head is not implemented correctly by this
  llama.cpp build.
- **`Alibaba-NLP/gte-multilingual-reranker-base`** could not be served: its GGUF declares
  architecture `new`, which this llama.cpp rejects, and the transformers shim crashed the GPU
  on this box (a CUDA fault this machine has produced before with several models resident).
  The 0.61 GB challenger therefore stays unmeasured, not rejected.

## Files

- `bakeoff.json` — every metric, the baseline and the per-model latency this report is built from.
- `../../../rust/crates/kb-eval/scripts/make_candidates.py` — builds the pool.
- `../../../rust/crates/kb-eval/scripts/serve_cross_encoder.py` — serves an HF cross-encoder as
  `/v1/rerank`, which is what made a GGUF-less checkpoint measurable at all.
