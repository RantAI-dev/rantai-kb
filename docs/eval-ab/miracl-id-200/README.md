# A/B on miracl-id-200 with the local embedder (2026-10-23)

**Result: with Qwen3-Embedding-0.6B, the k=2 fusion fix (`f8c0639`) makes ranking worse,
not better.** nDCG@10 −0.021 and MRR@10 −0.031, both with 95% CIs below zero. The
title + heading-path embedding text (`61a076a`) has no measurable effect.

This contradicts the prediction in `1172f58` (nDCG@10 0.815, Hit@1 0.740). That prediction
came from an offline replay with **e5-large** vectors and was never checked live. The best
fusion constant depends on the embedder, so a single hard-coded k is the wrong design.

## Setup

- **Dataset:** `rust/crates/kb-eval/datasets/miracl-id-200` (200 queries, 2,933 passages,
  639 qrels).
- **Embedder:** Qwen3-Embedding-0.6B Q8_0 on llama.cpp (Vulkan, RX 590, LAN), dim 1024,
  with the Qwen3 query instruction. Checked against TEI fp32 on 32 MIRACL texts: cosine
  0.9997, with top-5 order identical on 6/8 queries and one adjacent swap on the other 2.
- **Store:** Qdrant v1.19.2. No reranker. Fresh stack per arm.
- **Eval client:** the `ff5b28d` client for every arm.
- **Command:**
  `KB_STACK_INGEST_CONCURRENCY=16 rust/scripts/eval-ab.sh old=<bin> fusion-only=<bin> new=<bin>`

| Arm | Server commit | Fusion | Embedding text |
|---|---|---|---|
| old | `5391845` | k=60 re-sort | content |
| fusion-only | `f8c0639` | Qdrant RRF k=2 | content |
| new | `ff5b28d` | Qdrant RRF k=2 | title > heading path + content |

## Results (document level, 95% bootstrap CI)

| Arm | nDCG@10 | Recall@10 | MRR@10 | Hit@1 |
|---|---:|---:|---:|---:|
| old | **0.800** [0.771–0.832] | 0.942 | **0.822** [0.781–0.861] | **0.720** |
| fusion-only | 0.779 [0.748–0.810] | 0.945 | 0.791 [0.747–0.833] | 0.685 |
| new | 0.782 [0.750–0.813] | 0.945 | 0.794 [0.750–0.835] | 0.690 |

Paired deltas (`score_deltas.py`, 10,000 resamples, seed 42):

| Comparison | nDCG@10 | MRR@10 | Recall@10 |
|---|---|---|---|
| fusion-only − old (k=2 fusion) | −0.021 [−0.038, −0.003] | −0.031 [−0.057, −0.004] | +0.003 [−0.008, +0.015] |
| new − fusion-only (embedding text) | +0.003 [−0.001, +0.008] | +0.003 [−0.006, +0.012] | 0.000 |
| new − old (both) | −0.018 [−0.035, −0.000] | −0.028 [−0.055, −0.002] | +0.003 [−0.008, +0.015] |

Recall@10 does not move. k=2 changes which relevant passage ends up first, not whether
one is found.

## Wall time per arm (ingest 2,933 files + 200 queries)

old 670 s, fusion-only 693 s, new 722 s. Before `ff5b28d` the same ingest ran at
1.4 files/s (≈35 min per arm): the eval client uploaded one file at a time.

## Caveats

- Run on the same 200 queries that informed the k=2 analysis; there is no held-out
  split for MIRACL yet.
- Only one embedder was run live. e5-large with k=2 has still not been run live.
