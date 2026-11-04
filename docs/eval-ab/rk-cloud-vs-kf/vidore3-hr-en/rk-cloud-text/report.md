# Retrieval evaluation — rk-cloud-text

- dataset: `vidore3-hr-en`
- harness commit: `ead4e48-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-31T01:57:36.503618844+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.943 [0.925–0.961] | 0.988 [0.977–0.997] | 0.931 [0.910–0.953] | 0.887 [0.855–0.921] | 0.104 [0.102–0.107] |
| page | 318 | 0.675 [0.647–0.705] | 0.683 [0.647–0.718] | 0.808 [0.773–0.844] | 0.720 [0.667–0.774] | 0.250 [0.233–0.270] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.943 [0.925–0.961] | 0.988 [0.977–0.997] | 0.931 [0.910–0.953] | 0.887 [0.855–0.921] | 0.104 [0.102–0.107] |
| page (answered only) | 318 | 0.675 [0.647–0.705] | 0.683 [0.647–0.718] | 0.808 [0.773–0.844] | 0.720 [0.667–0.774] | 0.250 [0.233–0.270] |

Latency over 318 answered queries (ms): mean 25.0, p50 24.5, p95 38.1, p99 41.9

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 22.7, p95 36.4, p99 40.1. Client-measured: p50 24.5, p95 38.1, p99 41.9 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 6.2 | 6.0 | 7.9 | 9.0 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 318 | 22.2 | 21.6 | 35.3 | 39.2 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 6.2 | 6.0 | 7.9 | 9.0 |
| detail.pageArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.scopeCheck | 318 | 0.9 | 0.9 | 1.1 | 2.2 |
| detail.vectorArm | 318 | 6.2 | 6.0 | 7.9 | 9.0 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.1 | 1.0 | 1.4 | 1.7 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 13.3 | 12.8 | 26.5 | 30.4 |
| pages | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 0.3 | 0.3 | 0.4 | 0.5 |
| scopeBuild | 318 | 0.8 | 0.8 | 1.0 | 1.3 |
| serialise | 318 | 0.2 | 0.2 | 0.2 | 0.3 |
| total | 318 | 23.3 | 22.7 | 36.4 | 40.1 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
