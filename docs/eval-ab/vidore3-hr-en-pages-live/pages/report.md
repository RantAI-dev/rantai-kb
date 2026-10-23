# Retrieval evaluation — pages

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T17:47:59.154155462+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.919 [0.897–0.941] | 0.977 [0.961–0.991] | 0.906 [0.879–0.931] | 0.849 [0.808–0.890] | 0.103 [0.101–0.106] |
| page | 318 | 0.625 [0.593–0.656] | 0.669 [0.630–0.707] | 0.744 [0.706–0.785] | 0.626 [0.572–0.679] | 0.250 [0.232–0.271] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.919 [0.897–0.941] | 0.977 [0.961–0.991] | 0.906 [0.879–0.931] | 0.849 [0.808–0.890] | 0.103 [0.101–0.106] |
| page (answered only) | 318 | 0.625 [0.593–0.656] | 0.669 [0.630–0.707] | 0.744 [0.706–0.785] | 0.626 [0.572–0.679] | 0.250 [0.232–0.271] |

Latency over 318 answered queries (ms): mean 824.3, p50 817.3, p95 865.1, p99 1030.1

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 815.2, p95 863.3, p99 1026.5. Client-measured: p50 817.3, p95 865.1, p99 1030.1 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 33.1 | 31.9 | 42.6 | 49.7 |
| context | 318 | 0.2 | 0.1 | 0.2 | 0.6 |
| detail.engineTotal | 318 | 820.7 | 813.8 | 861.8 | 1025.2 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 33.1 | 31.9 | 42.6 | 49.7 |
| detail.queryEmbed | 318 | 0.7 | 0.6 | 1.1 | 3.0 |
| detail.scopeCheck | 318 | 1.2 | 1.1 | 2.7 | 3.5 |
| detail.vectorArm | 318 | 33.1 | 31.9 | 42.6 | 49.7 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.5 | 1.3 | 3.2 | 4.4 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 19.2 | 16.5 | 42.1 | 56.1 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 318 | 1.1 | 0.9 | 1.9 | 3.6 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.8 |
| total | 318 | 822.1 | 815.2 | 863.3 | 1026.5 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
