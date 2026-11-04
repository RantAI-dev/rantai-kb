# rantai-kb cloud tier vs KF main

Run on 2026-10-31 with `run.sh` (in this folder), using kb-server built from `ead4e48`
(`~/kb-target-alt/release/kb-server`). There was one fresh kb-smoke stack per dataset.
OpenRouter spend for the whole run was $0.29 ($0.096 → $0.383 on the key). Most embeddings
came from the on-disk cache (`~/.cache/kb-cache`).

The cloud tier settings, written out because stack-up sets some roles explicitly:

- Embeddings: `voyageai/voyage-multimodal-3.5` (1024 dims, input_type on).
- Rerank: `voyageai/rerank-2.5` over the top 20 (cohere-shaped endpoint on OpenRouter).
- Page arm: the same voyage model embeds page images, with page rerank on.
- `rk-cloud-text` is the same server and index with the page arm switched off at query time
  (`eval-variants.sh`).

## Results (nDCG@10, every query answered, 0 failed)

| Dataset | Queries | KF main | rk-cloud-text | rk-cloud |
|---|---|---|---|---|
| MIRACL-id-200 (document) | 200 | 0.813 | — | **0.845** |
| ViDoRe v3 HR (page) | 318 | not run yet | 0.675 | **0.710** |
| ViDoRe v3 CS (page) | 215 | not run yet | 0.778 | **0.822** |

Paired bootstrap (`score_deltas.py`, 95% CI):

- **MIRACL, KF main → rk-cloud:**
  - nDCG@10 +0.032 [+0.006, +0.057];
  - MRR@10 +0.037 [+0.002, +0.072];
  - recall@5 +0.044 [+0.011, +0.078];
  - recall@10 −0.006 [−0.025, +0.012] (a tie).
- **HR, text-only → multimodal:** nDCG@10 +0.035 [+0.018, +0.052]; recall@10 +0.050 [+0.033, +0.068].
- **CS, text-only → multimodal:** nDCG@10 +0.044 [+0.030, +0.059]; recall@10 +0.068 [+0.049, +0.090].

These match the earlier cloud runs (HR 0.711 / 0.674, CS 0.828 / 0.779).

## Caveats

- **KF main is as shipped:** `~/kfv-main`, local `intfloat/multilingual-e5-large`, no reranker,
  not modified. `miracl-id-200/kf-main/run.trec` is a copy of that run. The comparison is
  therefore "rantai-kb cloud tier vs KF's default stack", not the same models on both sides.
- **No KF arm on ViDoRe yet.** It needs Docling and RapidOCR over 2,470 pages. That is
  deferred until the machine is free.
- **The first attempt of this run is discarded.** It was killed mid-query when the agent
  session ended, and about 40 of its queries had already fallen back after OpenRouter network
  errors. The rerun had one rerank fallback in the MIRACL server log.
- Document nDCG on ViDoRe (0.94–1.00) is near the ceiling and does not separate arms. Page
  nDCG is the number to read there.
