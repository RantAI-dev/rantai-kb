# Retrieval evaluation — pages20-bge20

- dataset: `vidore3-hr-en`
- harness commit: `64498f7-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T08:52:43.188106948+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.919 [0.896–0.940] | 0.972 [0.953–0.988] | 0.904 [0.877–0.929] | 0.849 [0.811–0.887] | 0.103 [0.100–0.105] |
| page | 318 | 0.522 [0.486–0.558] | 0.559 [0.518–0.599] | 0.658 [0.615–0.700] | 0.538 [0.481–0.591] | 0.205 [0.189–0.224] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.919 [0.896–0.940] | 0.972 [0.953–0.988] | 0.904 [0.877–0.929] | 0.849 [0.811–0.887] | 0.103 [0.100–0.105] |
| page (answered only) | 318 | 0.522 [0.486–0.558] | 0.559 [0.518–0.599] | 0.658 [0.615–0.700] | 0.538 [0.481–0.591] | 0.205 [0.189–0.224] |

Latency over 318 answered queries (ms): mean 2386.5, p50 2134.9, p95 4552.8, p99 7047.0

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2133.0, p95 4550.5, p99 7039.8. Client-measured: p50 2134.9, p95 4552.8, p99 7047.0 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 22.2 | 19.6 | 39.1 | 64.2 |
| context | 318 | 0.3 | 0.1 | 0.8 | 3.1 |
| detail.engineTotal | 318 | 2379.8 | 2131.8 | 4549.0 | 7037.8 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 22.2 | 19.6 | 39.1 | 64.2 |
| detail.queryEmbed | 318 | 83.9 | 91.1 | 101.3 | 122.7 |
| detail.scopeCheck | 318 | 1.5 | 1.1 | 3.5 | 6.8 |
| detail.vectorArm | 318 | 22.2 | 19.6 | 39.1 | 64.2 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 2.1 | 1.7 | 4.4 | 8.4 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 32.0 | 27.1 | 67.3 | 89.6 |
| rerank | 318 | 2237.4 | 1984.8 | 4386.4 | 6902.3 |
| scopeBuild | 318 | 1.5 | 1.1 | 3.5 | 5.8 |
| serialise | 318 | 0.6 | 0.2 | 1.4 | 6.7 |
| total | 318 | 2382.3 | 2133.0 | 4550.5 | 7039.8 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
