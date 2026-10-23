# ViDoRe v3 computer science, live in kb-server, text only: 0.785 (2026-10-23 night)

**Result.** On the ViDoRe v3 computer-science set (2 PDFs, 1,360 pages, 215 English queries,
dataset revision pinned in `import_vidore.py`), kb-server's text pipeline alone scores
**0.785 page nDCG@10** (MRR@10 0.930, Recall@10 0.775), 0 degraded, 0 failed.

| System (English, nDCG@10, ViDoRe v3 paper Table 9: retrievers, no reranker) | CS |
|---|---:|
| ColEmbed-3B-v2 (best; non-commercial licence) | 0.786 |
| ColNomic-7B | 0.782 |
| Qwen3-Embedding-8B, text | 0.735 |
| Jina-v4 text | 0.673 |
| **rantai-kb `cs-voy-text`: 8B text + BM25 + voyage rerank-2.5** | **0.785** |

Caveats: our row includes a reranker and theirs do not; our unit is a chunk scored by its
page. **The page-image arm was not run on this set**: its page embeddings (~$0.85) did not fit in
the night's remaining budget. On HR the arm added +0.060 on top of this exact text stack.

## Setup

Same binary and stack as `../vidore3-hr-en-night/voy-text`, plus `KB_MAX_FILE_BYTES=104857600`:
the 943-page *Introduction to Computer Science* PDF is 50.9 MB, over the 50 MB default
(made configurable in `cfe4245`). Cost about $0.05.
