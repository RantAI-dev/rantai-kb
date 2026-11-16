# Retrieval evaluation — off

- dataset: `dated-versions`
- harness commit: `9b9cf7fe-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-04T14:15:50.006309485+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 165
- queries: 55 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 55 | 0.510 [0.502–0.519] | 1.000 [1.000–1.000] | 0.345 [0.336–0.358] | 0.000 [0.000–0.000] | 0.100 [0.100–0.100] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 55 | 0.510 [0.502–0.519] | 1.000 [1.000–1.000] | 0.345 [0.336–0.358] | 0.000 [0.000–0.000] | 0.100 [0.100–0.100] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 55 answered queries (ms): mean 1443.2, p50 1342.0, p95 2130.5, p99 2915.7

### Server-side stage timings (ms)

Server-side total over 55 responses: p50 1340.5, p95 2129.0, p99 2914.1. Client-measured: p50 1342.0, p95 2130.5, p99 2915.7 over 55 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 55 | 5.0 | 4.7 | 6.2 | 10.1 |
| context | 55 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 55 | 1438.2 | 1336.5 | 2125.4 | 2910.5 |
| detail.entityArm | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 55 | 5.0 | 4.7 | 6.2 | 10.1 |
| detail.pageArm | 55 | 915.6 | 794.3 | 1682.5 | 2513.9 |
| detail.queryEmbed | 55 | 832.4 | 790.4 | 891.0 | 1791.7 |
| detail.scopeCheck | 55 | 1.7 | 1.6 | 3.1 | 3.4 |
| detail.vectorArm | 55 | 5.0 | 4.7 | 6.2 | 10.1 |
| detail.visionRerank | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 55 | 0.8 | 0.8 | 1.1 | 1.2 |
| fusion | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 55 | 2.3 | 2.1 | 3.4 | 5.8 |
| pages | 55 | 64.8 | 0.0 | 360.7 | 1208.6 |
| rerank | 55 | 531.1 | 527.6 | 586.0 | 601.3 |
| scopeBuild | 55 | 3.3 | 3.1 | 4.1 | 4.6 |
| serialise | 55 | 0.0 | 0.0 | 0.1 | 0.1 |
| total | 55 | 1441.6 | 1340.5 | 2129.0 | 2914.1 |

Query embeddings: 0 cache hit(s), 55 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
