# Page-image arm, offline, on ViDoRe v3 HR (2026-10-23)

**Result.** Adding a page-image arm (voyage-multimodal-3.5 over OpenRouter) to the
text pipeline, fused by weighted RRF, raises page nDCG@10 on ViDoRe v3 HR (318 English
queries, 1,110 pages) on held-out queries:

| Text pipeline | Text only | + images, equal RRF (k=60) | + images, tuned RRF (2-fold CV) |
|---|---:|---:|---:|
| local: Qwen3-Embedding-0.6B + BM25 + bge top 20 | 0.561 | 0.601 (+0.040 [+0.025, +0.056]) | **0.629** (+0.068 [+0.048, +0.088]) |
| Qwen3-Embedding-8B + BM25 + bge top 20 | 0.593 | 0.624 (+0.031 [+0.014, +0.049]) | **0.642** (+0.050 [+0.033, +0.066]) |

95% paired bootstrap CIs, 5,000 resamples. Published best on this set (ViDoRe v3 paper,
Table 9, image retriever ColEmbed-3B-v2): 0.654.

By content type (bge top 20 text runs):

| Run | All | Table (112) | Chart (104) |
|---|---:|---:|---:|
| local text | 0.561 | 0.495 | 0.508 |
| 8B text | 0.593 | 0.536 | 0.538 |
| images only | 0.575 | **0.589** | 0.569 |
| local text + images, equal RRF | 0.601 | 0.539 | 0.565 |
| 8B text + images, equal RRF | 0.624 | 0.576 | 0.586 |

## What this shows

- **Images alone beat our local text pipeline** (0.575 vs 0.561), and are strongest exactly
  where text is weakest: table questions, 0.589 vs 0.495.
- **Fusion must be top-heavy.** Both CV folds picked the same setting independently: image
  weight 0.50–0.55 and RRF **k = 5** (vs the k = 60 used between our text arms). With k = 60
  the fused run loses table questions relative to images alone.
- **Rebuilding table structure in text is not the lever.** ViDoRe's own markdown, which has
  real tables on 230 pages, scores no better on table questions than our flat pdfium text
  (0.420 vs 0.430 without reranker, 0.500 vs 0.506 with), see `../vidore3-hr-en-md`.

## Setup

- `rust/crates/kb-eval/scripts/page_image_arm.py`: pages rendered with `pdftoppm` at 96 dpi
  (JPEG q80); each page embedded once as an image (`input_type=document`), queries as text
  (`input_type=query`), cosine ranking, top 100 per query. Embeddings cached on disk under
  `~/.cache/kb-cache/page-image-arm`, so reruns are free.
- `rust/crates/kb-eval/scripts/fuse_cv.py`: weighted RRF of a text page run and the image run;
  w and k tuned on the even queries and tested on the odd ones, and vice versa.
- Text runs: `../vidore3-hr-en/final20-bge20` and `../vidore3-hr-en/emb8b-bge20`.
- Scores: `breakdown.py` (same nDCG@10 as `kb-eval`; it reproduces 0.561 and 0.593).
- **Cost:** $0.581 for 1,110 page images (~$0.0005 per page at 96 dpi) and 318 queries.

## Caveats

- Offline fusion of saved runs, not yet a live arm in kb-server.
- One domain (EU labour reports), English only.
- voyage-multimodal-3.5 is a paid API. The local equivalent (Qwen3-VL-Embedding-2B, 🟢
  Apache-2.0) has not been measured; it is the Desk/Studio-tier candidate in
  `docs/kb-tiers.md`.
- The fused list is not reranked. A text reranker over fused page candidates, or a
  multimodal reranker, is untested.

## Update, 2026-10-23 night: text + image pages, then a page-level reranker

**Text + image page vectors.** Each page embedded as its own text layer followed by its image
(`page_image_arm.py --page-text-dir`, $0.69 for 1,110 pages):

| Page embedding | Pages alone | + 8B text (CV) | + local text + bge (CV) | + 8B text + bge (CV) |
|---|---:|---:|---:|---:|
| image only | 0.575 | 0.625 | 0.629 | 0.648 |
| **text + image** | **0.605** | **0.642** | **0.636** | **0.655** |

(The 8B text runs here are same-session runs: `../vidore3-hr-en-furniture/base` without a
reranker and `../vidore3-hr-en-night/e8b-bge` with bge on the top 20.)

**Page-level reranking.** The fused list (8B text + bge, fused with text+image pages at w=0.5,
k=5 → 0.655, `fused-8bbge-textimage.page.trec`) reranked on each page's whole text (top 30
pages, 3,000 characters), blended with the fused order by weighted RRF; blend weight and k
chosen by 2-fold CV (`page_rerank.py`):

| Page reranker | Rerank only | CV blend (held-out) | Δ vs 0.655 [95% CI] | Cost |
|---|---:|---:|---|---:|
| nvidia/nemotron-3.5-rerank-8b:free | 0.697 | **0.705** | +0.049 [+0.031, +0.069] | $0 |
| voyageai/rerank-2.5 | 0.697 | **0.704** | +0.049 [+0.032, +0.066] | $0.29 |

Two independent rerankers agree. For reference: the best published retriever on this set
(ColEmbed-3B-v2) scores 0.654, and the paper's best pipeline with a reranker (Jina-v4 text +
zerank-2, Table 2, multilingual setting) 0.665. Live confirmation is in `../vidore3-hr-en-night`.
