# Retrieval evaluation — k60

- dataset: `miracl-id-200`
- harness commit: `4c01a7d-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T04:56:16.155514288+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.793 [0.764–0.824] | 0.927 [0.903–0.949] | 0.824 [0.781–0.862] | 0.720 [0.655–0.780] | 0.285 [0.259–0.310] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.793 [0.764–0.824] | 0.927 [0.903–0.949] | 0.824 [0.781–0.862] | 0.720 [0.655–0.780] | 0.285 [0.259–0.310] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 98.3, p50 94.6, p95 130.0, p99 132.8

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 93.1, p95 128.5, p99 131.1. Client-measured: p50 94.6, p95 130.0, p99 132.8 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 30.1 | 28.8 | 39.7 | 45.4 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 200 | 90.1 | 86.5 | 121.0 | 124.2 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 30.1 | 28.8 | 39.7 | 45.4 |
| detail.queryEmbed | 200 | 55.6 | 53.1 | 75.5 | 90.5 |
| detail.scopeCheck | 200 | 1.5 | 1.4 | 2.5 | 3.1 |
| detail.vectorArm | 200 | 30.1 | 28.8 | 39.7 | 45.4 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 0.8 | 0.8 | 1.0 | 1.1 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 2.0 | 2.0 | 2.6 | 3.0 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 200 | 6.6 | 6.4 | 7.8 | 9.2 |
| serialise | 200 | 0.0 | 0.0 | 0.1 | 0.1 |
| total | 200 | 96.8 | 93.1 | 128.5 | 131.1 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
