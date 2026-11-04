# Retrieval evaluation — final-bge20

- dataset: `miracl-id-200`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T11:07:05.487686290+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.830 [0.804–0.857] | 0.939 [0.918–0.958] | 0.857 [0.820–0.892] | 0.770 [0.710–0.825] | 0.290 [0.263–0.316] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.830 [0.804–0.857] | 0.939 [0.918–0.958] | 0.857 [0.820–0.892] | 0.770 [0.710–0.825] | 0.290 [0.263–0.316] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 1483.4, p50 1489.1, p95 1833.9, p99 1906.2

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 1487.6, p95 1832.3, p99 1904.8. Client-measured: p50 1489.1, p95 1833.9, p99 1906.2 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 43.6 | 36.4 | 76.4 | 98.2 |
| context | 200 | 0.2 | 0.1 | 0.5 | 2.5 |
| detail.engineTotal | 200 | 1472.5 | 1479.7 | 1824.6 | 1896.5 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 43.6 | 36.4 | 76.4 | 98.2 |
| detail.queryEmbed | 200 | 57.6 | 54.5 | 81.9 | 96.7 |
| detail.scopeCheck | 200 | 1.5 | 1.3 | 2.5 | 4.6 |
| detail.vectorArm | 200 | 43.6 | 36.4 | 76.4 | 98.2 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.0 | 0.9 | 1.8 | 2.7 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 3.7 | 2.6 | 8.6 | 22.4 |
| rerank | 200 | 1364.6 | 1375.1 | 1691.2 | 1803.4 |
| scopeBuild | 200 | 8.3 | 6.5 | 18.4 | 34.3 |
| serialise | 200 | 0.1 | 0.1 | 0.7 | 1.2 |
| total | 200 | 1481.0 | 1487.6 | 1832.3 | 1904.8 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
