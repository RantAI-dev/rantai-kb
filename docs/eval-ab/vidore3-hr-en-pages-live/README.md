# Page-image arm, live in kb-server, on ViDoRe v3 HR (2026-10-23)

**Result.** With the page-image arm on (`KB_PAGE_EMBEDDING_MODEL=voyageai/voyage-multimodal-3.5`),
page nDCG@10 on ViDoRe v3 HR (318 English queries, 14 PDFs, 1,110 pages) goes from
**0.572 to 0.625, +0.053 [+0.036, +0.070]** (paired bootstrap). The offline replay predicted
0.62–0.63 for this text pipeline (`../vidore3-hr-en-image`), so the Rust arm reproduces it.

| | `text` | `pages` | Δ [95% CI] |
|---|---:|---:|---|
| Page nDCG@10 | 0.572 | **0.625** | +0.053 [+0.036, +0.070] |
| Page Recall@10 | 0.626 | 0.669 | +0.043 [+0.024, +0.063] |
| Page MRR@10 | 0.702 | 0.744 | +0.042 [+0.015, +0.070] |
| Table questions (112) | 0.517 | 0.600 | |
| Chart questions (104) | 0.516 | 0.596 | |
| Document nDCG@10 | 0.915 | 0.919 | |
| Degraded / failed searches | 0 / 0 | 0 / 0 | |

## Setup

- Same binary for both arms (`2b5f8f5`, the page arm, plus later docs-only commits); same
  cached text vectors (Qwen3-Embedding-8B via OpenRouter, dim 4096), BM25, k=60 text fusion, 20
  chunks, **no reranker** (the LAN GPU was offline).
- `pages`: PDF pages rendered by pdfium at 96 dpi (JPEG q80), embedded as images; queries
  embedded as text; page index = its own Qdrant collection
  (`pages__chunks__voyageai-voyage-multimodal-3-5__1024`); fusion w=0.5, k=5.
- Runner: `rust/scripts/eval-ab.sh` with `KB_STACK_SERVER_ENV` setting the page arm and
  `KB_STACK_CACHE_DIR=~/.cache/kb-cache`.

## Latency and cost

- Search p50 / p95: **817 / 865 ms** with the page arm, 61 / 92 ms without. In this run the text
  query vectors were already in the disk cache and the page-arm query vectors were not, so the
  difference is one OpenRouter round trip per new query (~0.75 s). With both query embeddings
  uncached they run in parallel; a repeated query costs neither.
- Ingest: 491 s for 1,110 pages including the image embeddings (text vectors cached).
- Cost: **$0.57** (OpenRouter usage 1.308 → 1.882), almost all of it page images.

## Isolation

The leak matrix has a `page` row (`0679608`): 0 leaks over its probes on a live stack with the
arm on, and 224 leaks when the page path is deliberately unscoped (`docs/rust-cutover.md`).

## Caveats

- One domain (EU labour reports), English only, no reranker in either arm.
- Hosted model: the local tiers' candidate (Qwen3-VL-Embedding-2B) is not measured.
- The 0.75 s added latency is a hosted-embedder cost; a local embedder or a query-vector cache
  changes it.
