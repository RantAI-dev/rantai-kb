# Retrieval evaluation — pdf20

- dataset: `vidore3-hr-en`
- harness commit: `b0394eb`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T07:33:22.183523860+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.888 [0.863–0.913] | 0.966 [0.945–0.984] | 0.865 [0.836–0.895] | 0.786 [0.739–0.830] | 0.102 [0.099–0.105] |
| page | 318 | 0.422 [0.386–0.455] | 0.487 [0.443–0.527] | 0.535 [0.490–0.579] | 0.387 [0.330–0.440] | 0.183 [0.167–0.200] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.888 [0.863–0.913] | 0.966 [0.945–0.984] | 0.865 [0.836–0.895] | 0.786 [0.739–0.830] | 0.102 [0.099–0.105] |
| page (answered only) | 318 | 0.422 [0.386–0.455] | 0.487 [0.443–0.527] | 0.535 [0.490–0.579] | 0.387 [0.330–0.440] | 0.183 [0.167–0.200] |

Latency over 318 answered queries (ms): mean 112.6, p50 118.8, p95 134.6, p99 142.2

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 117.3, p95 132.9, p99 140.5. Client-measured: p50 118.8, p95 134.6, p99 142.2 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 10.2 | 9.9 | 12.8 | 15.7 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.3 |
| detail.engineTotal | 318 | 109.7 | 116.1 | 131.9 | 139.6 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 10.2 | 9.9 | 12.8 | 15.7 |
| detail.queryEmbed | 318 | 81.0 | 90.2 | 95.5 | 99.2 |
| detail.scopeCheck | 318 | 1.1 | 1.0 | 1.5 | 2.7 |
| detail.vectorArm | 318 | 10.2 | 9.9 | 12.8 | 15.7 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.0 | 1.0 | 1.3 | 1.6 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 16.1 | 16.2 | 28.1 | 33.4 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 0.9 | 0.8 | 1.3 | 2.1 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.4 |
| total | 318 | 110.9 | 117.3 | 132.9 | 140.5 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
