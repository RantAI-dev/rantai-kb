# Retrieval evaluation — vmm-cap-ti-pr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T17:57:31.343654338+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.946 [0.931–0.963] | 0.990 [0.978–0.998] | 0.935 [0.916–0.956] | 0.890 [0.858–0.925] | 0.105 [0.102–0.107] |
| page | 318 | 0.706 [0.677–0.737] | 0.730 [0.694–0.763] | 0.810 [0.776–0.847] | 0.717 [0.670–0.767] | 0.285 [0.265–0.308] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.946 [0.931–0.963] | 0.990 [0.978–0.998] | 0.935 [0.916–0.956] | 0.890 [0.858–0.925] | 0.105 [0.102–0.107] |
| page (answered only) | 318 | 0.706 [0.677–0.737] | 0.730 [0.694–0.763] | 0.810 [0.776–0.847] | 0.717 [0.670–0.767] | 0.285 [0.265–0.308] |

Latency over 318 answered queries (ms): mean 1838.8, p50 1818.6, p95 2243.8, p99 2580.7

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 1816.6, p95 2241.9, p99 2578.8. Client-measured: p50 1818.6, p95 2243.8, p99 2580.7 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 11.4 | 11.1 | 15.0 | 18.5 |
| context | 318 | 0.2 | 0.2 | 0.3 | 0.3 |
| detail.engineTotal | 318 | 1835.6 | 1815.5 | 2240.8 | 2577.7 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 11.4 | 11.1 | 15.0 | 18.5 |
| detail.pageArm | 318 | 11.1 | 10.7 | 14.6 | 16.6 |
| detail.queryEmbed | 318 | 0.6 | 0.5 | 0.8 | 0.9 |
| detail.scopeCheck | 318 | 1.3 | 1.1 | 3.1 | 3.9 |
| detail.vectorArm | 318 | 11.4 | 11.1 | 15.0 | 18.5 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 9.8 | 9.3 | 17.4 | 19.1 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 17.2 | 16.4 | 34.8 | 41.2 |
| pages | 318 | 1021.5 | 988.9 | 1373.8 | 1684.6 |
| rerank | 318 | 773.6 | 783.0 | 832.3 | 1113.0 |
| scopeBuild | 318 | 0.9 | 0.8 | 1.2 | 2.1 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.4 |
| total | 318 | 1836.8 | 1816.6 | 2241.9 | 2578.8 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
