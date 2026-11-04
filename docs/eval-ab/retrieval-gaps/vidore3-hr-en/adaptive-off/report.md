# Retrieval evaluation — adaptive-off

- dataset: `vidore3-hr-en`
- harness commit: `9b9cf7fe-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-04T14:26:41.219906094+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.947 [0.934–0.964] | 0.996 [0.988–1.000] | 0.934 [0.915–0.956] | 0.887 [0.855–0.921] | 0.106 [0.103–0.108] |
| page | 318 | 0.711 [0.682–0.742] | 0.734 [0.698–0.768] | 0.809 [0.777–0.844] | 0.711 [0.664–0.761] | 0.287 [0.267–0.310] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.947 [0.934–0.964] | 0.996 [0.988–1.000] | 0.934 [0.915–0.956] | 0.887 [0.855–0.921] | 0.106 [0.103–0.108] |
| page (answered only) | 318 | 0.711 [0.682–0.742] | 0.734 [0.698–0.768] | 0.809 [0.777–0.844] | 0.711 [0.664–0.761] | 0.287 [0.267–0.310] |

Latency over 318 answered queries (ms): mean 48.1, p50 48.4, p95 62.7, p99 80.5

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 46.3, p95 60.5, p99 75.5. Client-measured: p50 48.4, p95 62.7, p99 80.5 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 7.4 | 7.0 | 10.3 | 11.5 |
| context | 318 | 0.3 | 0.3 | 0.4 | 0.5 |
| detail.engineTotal | 318 | 44.2 | 44.7 | 59.1 | 73.1 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 7.4 | 7.0 | 10.3 | 11.5 |
| detail.pageArm | 318 | 10.0 | 10.1 | 11.8 | 13.2 |
| detail.queryEmbed | 318 | 0.2 | 0.2 | 0.3 | 0.4 |
| detail.scopeCheck | 318 | 1.8 | 1.1 | 3.7 | 4.7 |
| detail.vectorArm | 318 | 7.4 | 7.0 | 10.3 | 11.5 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.3 | 1.3 | 1.6 | 2.6 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 24.0 | 24.7 | 37.5 | 44.5 |
| pages | 318 | 8.6 | 8.2 | 11.2 | 12.6 |
| rerank | 318 | 0.4 | 0.4 | 0.5 | 2.5 |
| scopeBuild | 318 | 0.8 | 0.8 | 1.0 | 1.6 |
| serialise | 318 | 0.5 | 0.5 | 0.6 | 0.8 |
| total | 318 | 45.8 | 46.3 | 60.5 | 75.5 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
