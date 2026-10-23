# ViDoRe v3 HR, live in kb-server: 0.705 page nDCG@10 (2026-10-23 night)

**Result.** With text+image page vectors and page-level reranking, kb-server scores **0.705
page nDCG@10** on ViDoRe v3 HR (318 English queries, 14 PDFs, 1,110 pages), against 0.645 for
the same stack without pages: **+0.060 [+0.041, +0.079]** paired. The offline replay predicted
0.704–0.705 (`../vidore3-hr-en-image`), so the Rust engine reproduces it.

| Arm (same binary, same session) | Page nDCG@10 | Recall@10 | MRR@10 | Table (112) | Chart (104) | Degraded |
|---|---:|---:|---:|---:|---:|---:|
| `voy-text`: 8B text + BM25 + voyage rerank-2.5 (top 20 chunks) | 0.645 | 0.648 | 0.789 | 0.622 | 0.585 | 0 |
| **`voy-ti-pr`**: + text+image page arm + page rerank | **0.705** | **0.730** | **0.804** | **0.711** | **0.660** | 0 |

Published on the same queries (ViDoRe v3 paper, arXiv 2601.08620):

| System | HR nDCG@10 |
|---|---:|
| Best retriever, ColEmbed-3B-v2 (Table 9; non-commercial licence) | 0.654 |
| Best commercially usable retriever in Table 9, ColNomic-7B | 0.629 |
| Jina-v4 text retrieval + zerank-2 reranker (Table 2, multilingual setting) | 0.665 |
| **rantai-kb, `voy-ti-pr`** | **0.705** |

Caveats on the comparison: Table 9 rows are retrievers without rerankers; Table 2's reranker
row is the multilingual setting, not English-only; our unit is a chunk scored by its page.

## With Qwen3's query instruction (`voy-qp-*`)

Qwen3-Embedding expects an instruction on the query side. Setting
`KB_EMBEDDING_QUERY_PREFIX="Instruct: Given a web search query, retrieve relevant passages that
answer the query<newline>Query: "` (passage vectors unchanged, reused from the cache):

| Arm | Page nDCG@10 | Δ [95% CI] |
|---|---:|---|
| `voy-qp-text` (text only) | 0.657 | +0.012 [+0.001, +0.024] vs `voy-text` |
| **`voy-qp-ti-pr`** (full stack) | **0.708** | +0.003 [+0.000, +0.006] vs `voy-ti-pr`; +0.051 [+0.032, +0.069] vs `voy-qp-text` |

`voy-qp-ti-pr` by type: Table 0.713, Chart 0.664; 0 degraded. **Best live score: 0.708.**
Cost of the two `voy-qp` arms: $0.49.

## Configuration of `voy-ti-pr`

- Text: `qwen/qwen3-embedding-8b` (OpenRouter, dim 4096) + BM25, k=60 fusion, 20 chunks,
  chunk rerank `voyageai/rerank-2.5` on the top 20 (Cohere-shaped adapter at
  `https://openrouter.ai/api/v1/rerank`).
- Pages: `KB_PAGE_EMBEDDING_MODEL=voyageai/voyage-multimodal-3.5` (dim 1024), each page
  embedded as its text layer (≤ 6,000 chars) + its pdfium render (96 dpi JPEG); text/page fusion
  w=0.5, k=5; page rerank (`KB_PAGE_RERANK`, default on) on the top 30 fused pages, 3,000 chars of
  page text, blend w=0.7, k=1.
- Commit `ac19ea2` (binary built from it), `rust/scripts/eval-ab.sh` via `/tmp/kbtools/live2.sh`
  (`RERANK_MODEL=voyageai/rerank-2.5 PREFIX=voy`).

## Latency and cost

- Search p50 / p95: 1,307 / 1,758 ms with pages; 581 / 5,951 ms text only. The text arm's long
  tail is first-time query embeddings and rerank calls to hosted APIs; both arms pay network
  round trips to OpenRouter, so these are cloud-tier numbers, not local ones.
- Cost of both arms: **$1.17** (page embeddings for 1,110 pages ≈ $0.69, the rest reranking).

## Notes from the night

- **bge on the LAN GPU died at 19:27** mid-run; that run was discarded, not reported.
- **OpenRouter's free Nemotron reranker is too rate-limited for an eval** (about 3 searches a
  minute after retries). The adapter now retries 429/5xx with backoff (`137087f`, `ac19ea2`);
  a run on it had 0 degraded searches but was abandoned for time. Offline, Nemotron and
  voyage rerank-2.5 give the same page-rerank gain (0.705 vs 0.704).
- `e8b-bge` here is the same-session 8B text + bge run (0.606) the offline replay used.
