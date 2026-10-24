# Retrieval evaluation — bge-qp-nopr-ti-pr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T04:21:21.250718584+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.931 [0.912–0.952] | 0.982 [0.967–0.994] | 0.918 [0.894–0.943] | 0.865 [0.827–0.903] | 0.104 [0.101–0.107] |
| page | 318 | 0.661 [0.630–0.690] | 0.690 [0.653–0.726] | 0.777 [0.739–0.814] | 0.673 [0.623–0.726] | 0.263 [0.244–0.284] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.931 [0.912–0.952] | 0.982 [0.967–0.994] | 0.918 [0.894–0.943] | 0.865 [0.827–0.903] | 0.104 [0.101–0.107] |
| page (answered only) | 318 | 0.661 [0.630–0.690] | 0.690 [0.653–0.726] | 0.777 [0.739–0.814] | 0.673 [0.623–0.726] | 0.263 [0.244–0.284] |

Latency over 318 answered queries (ms): mean 1820.0, p50 2122.8, p95 2763.4, p99 3182.9

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2120.6, p95 2760.3, p99 3181.0. Client-measured: p50 2122.8, p95 2763.4, p99 3182.9 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 81.6 | 55.9 | 181.4 | 518.3 |
| context | 318 | 0.2 | 0.1 | 0.3 | 0.7 |
| detail.engineTotal | 318 | 1814.4 | 2118.0 | 2758.6 | 3178.9 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 81.6 | 55.9 | 181.4 | 518.3 |
| detail.queryEmbed | 318 | 1.3 | 1.0 | 2.2 | 2.9 |
| detail.scopeCheck | 318 | 2.0 | 1.4 | 5.4 | 7.4 |
| detail.vectorArm | 318 | 81.6 | 55.9 | 181.4 | 518.3 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 2.5 | 1.9 | 5.7 | 7.5 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 32.9 | 27.9 | 64.4 | 124.0 |
| rerank | 318 | 1688.6 | 2010.5 | 2637.7 | 2904.6 |
| scopeBuild | 318 | 2.0 | 1.5 | 4.8 | 7.9 |
| serialise | 318 | 0.2 | 0.2 | 0.4 | 1.2 |
| total | 318 | 1816.7 | 2120.6 | 2760.3 | 3181.0 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
