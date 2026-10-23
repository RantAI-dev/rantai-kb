# Retrieval evaluation — new

- dataset: `miracl-id-200`
- harness commit: `f0265bd-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T03:43:37.300997771+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.782 [0.750–0.813] | 0.945 [0.925–0.964] | 0.794 [0.750–0.835] | 0.690 [0.625–0.750] | 0.294 [0.267–0.321] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.782 [0.750–0.813] | 0.945 [0.925–0.964] | 0.794 [0.750–0.835] | 0.690 [0.625–0.750] | 0.294 [0.267–0.321] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 136.8, p50 109.5, p95 265.4, p99 597.0

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 107.7, p95 257.5, p99 584.4. Client-measured: p50 109.5, p95 265.4, p99 597.0 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 51.9 | 37.6 | 109.9 | 341.1 |
| context | 200 | 0.3 | 0.1 | 0.4 | 3.7 |
| detail.engineTotal | 200 | 120.7 | 100.2 | 229.1 | 515.2 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 51.9 | 37.6 | 109.9 | 341.1 |
| detail.queryEmbed | 200 | 57.2 | 54.5 | 89.7 | 94.8 |
| detail.scopeCheck | 200 | 2.3 | 1.5 | 3.9 | 26.7 |
| detail.vectorArm | 200 | 51.9 | 37.6 | 109.9 | 341.1 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.4 | 1.0 | 2.2 | 7.8 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 7.3 | 4.3 | 14.3 | 85.6 |
| rerank | 200 | 0.1 | 0.0 | 0.1 | 1.4 |
| scopeBuild | 200 | 11.4 | 7.2 | 35.0 | 81.1 |
| serialise | 200 | 1.2 | 0.1 | 1.5 | 15.2 |
| total | 200 | 133.4 | 107.7 | 257.5 | 584.4 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
