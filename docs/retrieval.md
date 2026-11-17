# Retrieval

Per query, the pipeline is:

```
query
 ├─ query expansion → N paraphrases          (opt-in)
 ├─ dense vector search ┐
 ├─ BM25                ├─ run in parallel
 └─ entity arm          ┘
      ↓ resolve BM25 hits through the scoped document store
      ↓ Reciprocal Rank Fusion (k = 60)
      ↓ cross-encoder rerank                    (opt-in, tier dependent)
      ↓ neighbour-window expansion, ±1 adjacent chunk
      ↓ page-image arm and figure co-retrieval
      ↓ stable source numbering
```

Four parts are worth knowing before tuning anything.

**Hybrid is not an ensemble.** Dense embeddings are strong on paraphrase and weak on rare
tokens — part numbers, regulation codes, internal acronyms — because a vector averages
meaning and rare tokens sink. BM25 is strong exactly there and blind to paraphrase. The
failure modes are uncorrelated; that is the reason to run both.

**Fusion happens on ranks, not scores.** A cosine score and a BM25 score are not
commensurable, and any normalisation between them has to be re-tuned every time the
embedding model changes. RRF sidesteps that.

**BM25 hits are not trustworthy until they are joined.** The full-text index has no notion
of category, group or ownership. Every hit is resolved back to its parent document through
the scoped document store, so a keyword arm cannot leak chunks from a document the caller
cannot see.

**A relevance floor is available but off by default.** `minScore` on the request (or
`KB_MIN_SCORE` for the whole server) drops hits whose comparable score is below the
threshold: the reranker score when a reranker ran, otherwise the dense cosine similarity —
never the fused RRF score, which is rank-only. An exact-match / identifier query keeps a
hit the keyword arm matched and whose text literally contains the identifier. If the arm
that supplies the score fails, the floor is skipped and `floor_skipped` is reported, so a
failed arm never becomes a silent empty answer. Unset means the floor does not run and the
ranking is identical to a build without it.

**Neighbour-window expansion is cheap and load-bearing.** `KB_NEIGHBOR_WINDOW=1` pulls ±1
adjacent chunk around each hit, woven into reading order next to its anchor and sharing the
anchor's source card, so no new citation numbers appear. A retrieved table travels with its
explanation, and an explanation pulls in its table.

**Page images.** Scans, charts, slides and tables can also be embedded as whole pages. On
the measured cloud run the page arm adds +0.035 page nDCG@10 on ViDoRe HR and +0.044 on CS
over text-only; both CIs are above zero. The method and its evaluation are published in
[IKAT](https://github.com/RantAI-dev/ikat).

## Choosing a tier's arms

The tier preset decides which arms can run: Lite and Desk embed text only, so the page arm
is off; Studio and Station add a page embedder and a page reranker; Cloud runs everything
hosted. An explicit `KB_*` variable always beats the preset — see
[`kb-tiers.md`](kb-tiers.md#kb_tier-implemented-2026-10-24-ocr-and-qwen3-vl-roles-2026-10-30)
for the exact values each tier fills.

Default-off, because each adds a model call to the answer path: query expansion
(`KB_QUERY_EXPANSION_*`), contextual retrieval, reranking (on for Desk and up), citation
grounding, the vision figure gate, the relevance floor, the tree arm (`KB_TREE_ARM`) and
the global community arm (`KB_GLOBAL_ARM`, a measured loss).

## What a result carries

`chunks[]` carries `content`, `documentTitle`, `documentId`, `page`, `similarity`, the
`arm`s that found it and its `externalRef`; `sources` numbers them stably; `stats` reports
per-arm counts and `skippedArms`; `degrades` names every arm that failed. `GET /v1/search`
documentation is in [`api.md`](api.md) and the live contract at `/docs`.

Measured retrieval quality per tier: [`performance.md`](performance.md).
