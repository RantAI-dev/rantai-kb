# Retrieval evaluation — pages20

- dataset: `vidore3-hr-en`
- harness commit: `64498f7-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T08:40:02.343006330+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.878 [0.853–0.903] | 0.972 [0.953–0.988] | 0.851 [0.820–0.882] | 0.764 [0.717–0.811] | 0.103 [0.100–0.105] |
| page | 318 | 0.425 [0.387–0.460] | 0.483 [0.439–0.526] | 0.544 [0.498–0.590] | 0.406 [0.352–0.459] | 0.175 [0.160–0.192] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.878 [0.853–0.903] | 0.972 [0.953–0.988] | 0.851 [0.820–0.882] | 0.764 [0.717–0.811] | 0.103 [0.100–0.105] |
| page (answered only) | 318 | 0.425 [0.387–0.460] | 0.483 [0.439–0.526] | 0.544 [0.498–0.590] | 0.406 [0.352–0.459] | 0.175 [0.160–0.192] |

Latency over 318 answered queries (ms): mean 150.6, p50 145.1, p95 225.8, p99 293.2

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 142.3, p95 222.8, p99 288.6. Client-measured: p50 145.1, p95 225.8, p99 293.2 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 23.0 | 20.8 | 39.3 | 57.2 |
| context | 318 | 0.2 | 0.2 | 0.3 | 0.8 |
| detail.engineTotal | 318 | 145.1 | 139.5 | 220.1 | 286.1 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 23.0 | 20.8 | 39.3 | 57.2 |
| detail.queryEmbed | 318 | 85.8 | 91.8 | 112.8 | 156.7 |
| detail.scopeCheck | 318 | 2.0 | 1.4 | 4.9 | 10.2 |
| detail.vectorArm | 318 | 23.0 | 20.8 | 39.3 | 57.2 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 2.4 | 1.8 | 5.1 | 8.6 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 31.6 | 27.6 | 70.7 | 102.9 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 318 | 1.8 | 1.3 | 4.1 | 5.9 |
| serialise | 318 | 0.4 | 0.2 | 0.7 | 2.7 |
| total | 318 | 147.3 | 142.3 | 222.8 | 288.6 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
