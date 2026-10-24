# ViDoRe v3 computer science, live in kb-server: 0.828 with the page arm (2026-10-24)

**Result.** On the ViDoRe v3 computer-science set (2 PDFs, 1,360 pages, 215 English queries,
dataset revision pinned in `import_vidore.py`), the cloud default (voyage-multimodal-3.5 for
text chunks and page images, voyage rerank-2.5 for chunks and pages) scores **0.828 page
nDCG@10**, 0 failed, 1 degraded (one `rerank_unavailable`). The text pipeline alone scores 0.779.

| Run (page nDCG@10, 215 queries) | nDCG@10 | Recall@10 | MRR@10 |
|---|---:|---:|---:|
| `cs-voy-text`: qwen3-embedding-8b text + BM25 + voyage rerank (2026-10-23) | 0.785 | 0.775 | 0.930 |
| `cs-vmm-text`: voyage-multimodal-3.5 text + BM25 + voyage rerank | 0.779 | 0.770 | 0.922 |
| **`cs-vmm-ti-pr`: same + page-image arm + page rerank** | **0.828** | **0.847** | 0.918 |

Paired deltas (95% bootstrap CI, `score_deltas.py`):

| Change | nDCG@10 |
|---|---|
| page arm on top of the voyage text stack (`cs-vmm-text` → `cs-vmm-ti-pr`) | **+0.049 [+0.035, +0.065]** |
| voyage instead of qwen3-8b for chunks (`cs-voy-text` → `cs-vmm-text`) | −0.006 [−0.021, +0.006] |
| cloud default vs last night's text stack (`cs-voy-text` → `cs-vmm-ti-pr`) | +0.043 [+0.028, +0.059] |

By content type (`breakdown.py`; a query can have several types), text-only → with pages:
Infographic 0.735 → 0.820 (n=47), Chart 0.633 → 0.734 (n=10), Table 0.772 → 0.813 (n=77),
Text 0.777 → 0.825 (n=209). Visual pages gain most, as on HR.

| System (English, nDCG@10, ViDoRe v3 paper Table 9: retrievers, no reranker) | CS |
|---|---:|
| ColEmbed-3B-v2 (best; non-commercial licence) | 0.786 |
| ColNomic-7B | 0.782 |
| Qwen3-Embedding-8B, text | 0.735 |
| **rantai-kb `cs-vmm-ti-pr`** | **0.828** |

Caveats: our row includes a reranker and theirs do not; our unit is a chunk or a page scored by
its page. What this does show: the HR gain from the page arm (+0.037 on top of the same text
stack) holds on a second domain, and is larger here.

**One embedder.** On HR, voyage for chunks beat qwen3-8b by +0.017; here it is a tie. Neither
set argues for keeping two embedding models in the cloud tier.

## Setup

`../vidore3-hr-en-night/vmm-*` setup (commit `1f7d884`), plus `KB_MAX_FILE_BYTES=104857600`
(the 943-page *Introduction to Computer Science* PDF is 50.9 MB) and `KB_AB_TIMEOUT=3600`
(ingesting that PDF with page images takes over the 900 s client default).

The first attempt (2026-10-24 morning) was broken: page nDCG 0.317, 4 failed, 13 degraded.
Two bugs, both fixed in `1f7d884` with tests: pages with no text were sent to voyage rerank,
which rejects empty strings with a 400; and all 943 page vectors went to Qdrant in one upsert,
which timed out. The rerun above has none of these failures.

Cost: about $1.24 for the broken run (page embeddings, now cached). The rerun paid only for
reranking: the key's usage rose $0.35 over a window that also held failed vision-rerank test
calls, so the rerun cost at most that. `cs-vmm-text` was fully cached (34 s wall, no provider calls).
