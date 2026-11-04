# Header/footer removal on ViDoRe v3 HR (2026-10-23)

**Result: no measurable effect.** Removing running headers, footers and page numbers before
chunking (`KB_STRIP_PAGE_FURNITURE=1`, `kb_core::furniture`, 1,933 lines removed across the 14
PDFs) changes page nDCG@10 by **−0.003 [−0.013, +0.006]** (paired bootstrap, 318 queries).
It stays off by default.

| Arm | Page nDCG@10 | Table (112) | Chart (104) | Wall time |
|---|---:|---:|---:|---:|
| `base`: furniture kept | 0.572 | 0.518 | 0.518 | 1,686 s |
| `base-repeat`: same, again | 0.572 (identical) | | | **24 s** |
| `strip`: furniture removed | 0.569 | 0.516 | 0.522 | 341 s |

Stack: Qdrant; Qwen3-Embedding-8B via OpenRouter (dim 4096); BM25; k=60 fusion; 20 chunks;
**no reranker** (the LAN GPU was offline); disk cache on (`KB_STACK_CACHE_DIR`).

## Other findings from this run

- **A real-embedder run is deterministic.** `base-repeat` reproduces `base` exactly. The
  run-to-run noise seen on the stub stack (`docs/local-models.md`) is specific to the stub's
  8-d vectors.
- **The disk cache works at scale.** `base-repeat` re-ingested the corpus and answered all
  queries in 24 s instead of 1,686 s, with zero OpenRouter calls. `strip` re-embedded only the
  chunks whose text changed.
- **Hosted embedding results drift between sessions.** `../vidore3-hr-en/emb8b` (11:34 the
  same day, same server code including the neighbour fix) scored 0.545; `base` scores 0.572,
  +0.027 [+0.011, +0.043] paired. Same model name, same dataset. The likely causes are
  outside our code: OpenRouter serving `qwen/qwen3-embedding-8b` from a different upstream
  provider, or a stack setting of that run not recorded with it. Not resolved. Rule: compare
  only runs from the same session, or runs whose vectors come from the same cache.

Cost: about $0.03 of OpenRouter embeddings for both ingests and the queries.
