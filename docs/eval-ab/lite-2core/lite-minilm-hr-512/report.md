# Retrieval evaluation — lite-minilm-hr-512

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-25T05:11:53.797923589+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.886 [0.860–0.911] | 0.972 [0.954–0.987] | 0.863 [0.832–0.893] | 0.786 [0.739–0.830] | 0.103 [0.100–0.106] |
| page | 318 | 0.517 [0.480–0.552] | 0.571 [0.527–0.612] | 0.647 [0.604–0.691] | 0.522 [0.465–0.582] | 0.212 [0.195–0.231] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.886 [0.860–0.911] | 0.972 [0.954–0.987] | 0.863 [0.832–0.893] | 0.786 [0.739–0.830] | 0.103 [0.100–0.106] |
| page (answered only) | 318 | 0.517 [0.480–0.552] | 0.571 [0.527–0.612] | 0.647 [0.604–0.691] | 0.522 [0.465–0.582] | 0.212 [0.195–0.231] |

Latency over 318 answered queries (ms): mean 43.5, p50 42.5, p95 59.8, p99 67.2

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 40.8, p95 58.0, p99 65.5. Client-measured: p50 42.5, p95 59.8, p99 67.2 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 9.1 | 8.2 | 14.3 | 21.1 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 318 | 40.6 | 39.7 | 56.8 | 64.5 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 9.1 | 8.2 | 14.3 | 21.1 |
| detail.pageArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 318 | 14.0 | 13.2 | 21.7 | 34.3 |
| detail.scopeCheck | 318 | 1.2 | 1.0 | 2.6 | 3.9 |
| detail.vectorArm | 318 | 9.1 | 8.2 | 14.3 | 21.1 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.1 | 1.1 | 1.5 | 1.9 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 15.0 | 14.8 | 28.1 | 32.5 |
| pages | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 0.9 | 0.8 | 1.3 | 2.5 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.3 |
| total | 318 | 41.8 | 40.8 | 58.0 | 65.5 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
