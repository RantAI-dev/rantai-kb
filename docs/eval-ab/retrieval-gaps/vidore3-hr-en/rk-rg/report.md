# Retrieval evaluation — rk-rg

- dataset: `vidore3-hr-en`
- harness commit: `9b9cf7fe-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-04T14:26:25.629373763+00:00
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

Latency over 318 answered queries (ms): mean 760.2, p50 611.8, p95 1564.5, p99 2255.7

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 609.6, p95 1562.2, p99 2253.7. Client-measured: p50 611.8, p95 1564.5, p99 2255.7 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 7.4 | 7.0 | 10.1 | 13.4 |
| context | 318 | 0.3 | 0.3 | 0.4 | 0.5 |
| detail.engineTotal | 318 | 756.4 | 608.3 | 1560.6 | 2252.3 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 7.4 | 7.0 | 10.1 | 13.4 |
| detail.pageArm | 318 | 10.5 | 10.3 | 13.0 | 14.4 |
| detail.queryEmbed | 318 | 0.6 | 0.6 | 0.8 | 0.9 |
| detail.scopeCheck | 318 | 2.1 | 1.4 | 4.1 | 5.3 |
| detail.vectorArm | 318 | 7.4 | 7.0 | 10.1 | 13.4 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.3 | 1.3 | 1.7 | 1.9 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 25.0 | 25.4 | 41.9 | 47.3 |
| pages | 318 | 248.1 | 9.9 | 957.4 | 1651.0 |
| rerank | 318 | 471.5 | 540.9 | 691.3 | 980.4 |
| scopeBuild | 318 | 0.9 | 0.8 | 1.2 | 1.9 |
| serialise | 318 | 0.5 | 0.5 | 0.7 | 0.8 |
| total | 318 | 758.0 | 609.6 | 1562.2 | 2253.7 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
