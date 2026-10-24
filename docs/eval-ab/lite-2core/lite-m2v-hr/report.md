# Retrieval evaluation — lite-m2v-hr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T19:26:02.650363401+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.869 [0.842–0.895] | 0.971 [0.953–0.987] | 0.839 [0.805–0.870] | 0.752 [0.704–0.799] | 0.103 [0.100–0.106] |
| page | 318 | 0.442 [0.408–0.476] | 0.510 [0.466–0.554] | 0.560 [0.514–0.604] | 0.425 [0.368–0.481] | 0.192 [0.175–0.210] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.869 [0.842–0.895] | 0.971 [0.953–0.987] | 0.839 [0.805–0.870] | 0.752 [0.704–0.799] | 0.103 [0.100–0.106] |
| page (answered only) | 318 | 0.442 [0.408–0.476] | 0.510 [0.466–0.554] | 0.560 [0.514–0.604] | 0.425 [0.368–0.481] | 0.192 [0.175–0.210] |

Latency over 318 answered queries (ms): mean 29.8, p50 29.7, p95 41.1, p99 43.8

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 28.0, p95 39.4, p99 42.1. Client-measured: p50 29.7, p95 41.1, p99 43.8 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 6.8 | 6.6 | 8.6 | 10.1 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 318 | 27.0 | 26.8 | 38.3 | 41.1 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 6.8 | 6.6 | 8.6 | 10.1 |
| detail.pageArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 318 | 3.2 | 3.1 | 4.0 | 5.9 |
| detail.scopeCheck | 318 | 1.0 | 0.9 | 1.3 | 2.4 |
| detail.vectorArm | 318 | 6.8 | 6.6 | 8.6 | 10.1 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.1 | 1.0 | 1.4 | 2.0 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 14.7 | 14.4 | 26.0 | 29.1 |
| pages | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 0.8 | 0.8 | 1.1 | 1.9 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.3 |
| total | 318 | 28.1 | 28.0 | 39.4 | 42.1 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
