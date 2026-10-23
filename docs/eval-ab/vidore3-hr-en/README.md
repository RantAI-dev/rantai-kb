# ViDoRe v3 HR: real PDFs, page-level (2026-10-23)

**Result.** Three fixes to how rantai-kb handles PDF pages took page-level nDCG@10 on
ViDoRe v3 HR (318 English queries, 14 EU reports, 1,110 pages) from **0.477 to 0.561**
with the bge reranker: +0.085 [+0.061, +0.109]. MRR@10 went from 0.599 to 0.719. All
three fixes were bugs in the PDF path. None of them is a model change.

| Fix | Commit | What was wrong |
|---|---|---|
| Chunk PDFs page by page | `64498f7` | 19% of chunks were tagged with the wrong page, and 15% straddled a page break |
| Keyword hits keep their page | `28d8372` | `chunks[].page` was dropped for every keyword-only hit, copying Bun's shape |
| Neighbours follow the ranked hits | `86d1908` | Neighbour chunks were woven in ahead of later hits, sometimes ahead of the top hit |

## Setup

- **Data:** `rust/crates/kb-eval/datasets/vidore3-hr-en` (`import_vidore.py`, pinned
  revision). The corpus is the dataset's own PDFs, ingested through kb-server's
  extraction. Scoring is at page level (`<doc>#p<N>`) against ViDoRe's graded human qrels,
  with 5.4 relevant pages per query on average.
- **Stack:** Qdrant; Qwen3-Embedding-0.6B Q8 and bge-reranker-v2-m3 Q8 on the LAN RX 590
  (llama.cpp); k=60 fusion; 20 chunks requested per query; the reranker sees the top 20.

## Results (page nDCG@10; 95% CI where paired)

| Run | Pipeline | No reranker | bge top 20 |
|---|---|---:|---:|
| `pdf20` | before the fixes | 0.422 | 0.477 |
| `pages20` | + page-by-page chunking | 0.425 | 0.522 |
| `fixed20` | + keyword hits keep their page | 0.435 | 0.536 |
| `fixed20-nn0` | same, neighbours off | 0.468 | 0.559 |
| **`final20`** | + neighbours after the ranked hits | **0.468** | **0.561** |

Two runs isolate the cause, using the same queries on one-page documents:

| Run | Text | No reranker | bge top 20 |
|---|---|---:|---:|
| `../vidore3-hr-en-md` | ViDoRe's own per-page markdown | 0.479 | 0.549 |
| `../vidore3-hr-en-ours` | our extractor's text, one document per page | 0.477 | 0.566 |

Our text matches ViDoRe's own markdown, so extraction was never the problem; the losses
were in page handling. `final20` reaches the one-page-documents ceiling.

By query content type (bge top 20), before → after: Table 0.392 → 0.495, Chart
0.457 → 0.508, Text 0.482 → 0.562.

## Against the published numbers

ViDoRe v3 paper (arXiv 2601.08620), Table 9: English queries, HR dataset, nDCG@10, no
reranker.

| Retriever | HR |
|---|---:|
| text: Qwen3-Embedding-0.6B | 0.462 |
| text: BM25S | 0.496 |
| text: Qwen3-Embedding-8B | 0.523 |
| text: Jina-v4 | 0.588 |
| image: ColQwen2.5 (3B) | 0.592 |
| image: Jina-v4 (3B) | 0.646 |
| image: ColEmbed-3B-v2 (best) | 0.654 |
| **rantai-kb, local (0.6B + BM25 + bge reranker)** | **0.561** |

Caveats on this comparison:

- **Our unit is a chunk, scored by its page.** The paper's unit is the page.
- **Our row includes a reranker.** The paper's rows don't. The paper reports that a text
  reranker added +13.7 on HR to Jina-v4 text (multilingual setting), while an image
  reranker added nothing.

## Other runs here

- `text`, `text-bge20`, `text-qwen8b20`: 10 chunks per query, before the fixes.
  qwen3-reranker-8b (OpenRouter, $0.45) scored 0.484, against 0.475 for bge.
- Spend for this directory: $0.45 (OpenRouter usage 0.1945 → 0.6488). Everything else is
  local.

## Caveats

- These are 318 English queries from one domain (EU labour reports).
- `max_chunks` 20 here; the MIRACL and hotpot runs use 10.
