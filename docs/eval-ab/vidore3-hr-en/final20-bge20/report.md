# Retrieval evaluation — final20-bge20

- dataset: `vidore3-hr-en`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T10:45:51.091709912+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.919 [0.897–0.941] | 0.972 [0.953–0.988] | 0.905 [0.878–0.931] | 0.852 [0.811–0.890] | 0.103 [0.100–0.105] |
| page | 318 | 0.561 [0.523–0.596] | 0.569 [0.528–0.609] | 0.719 [0.676–0.760] | 0.626 [0.572–0.679] | 0.206 [0.189–0.224] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.919 [0.897–0.941] | 0.972 [0.953–0.988] | 0.905 [0.878–0.931] | 0.852 [0.811–0.890] | 0.103 [0.100–0.105] |
| page (answered only) | 318 | 0.561 [0.523–0.596] | 0.569 [0.528–0.609] | 0.719 [0.676–0.760] | 0.626 [0.572–0.679] | 0.206 [0.189–0.224] |

Latency over 318 answered queries (ms): mean 2355.2, p50 2109.8, p95 4526.9, p99 7045.2

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2108.2, p95 4524.4, p99 7043.5. Client-measured: p50 2109.8, p95 4526.9, p99 7045.2 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 10.5 | 9.9 | 13.8 | 17.0 |
| context | 318 | 0.2 | 0.1 | 0.3 | 1.2 |
| detail.engineTotal | 318 | 2352.0 | 2106.0 | 4523.3 | 7042.5 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 10.5 | 9.9 | 13.8 | 17.0 |
| detail.queryEmbed | 318 | 82.9 | 91.2 | 96.1 | 122.7 |
| detail.scopeCheck | 318 | 1.1 | 1.0 | 1.4 | 2.2 |
| detail.vectorArm | 318 | 10.5 | 9.9 | 13.8 | 17.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.2 | 1.1 | 1.9 | 3.2 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 18.3 | 17.6 | 33.6 | 47.4 |
| rerank | 318 | 2237.8 | 1990.5 | 4377.9 | 6929.7 |
| scopeBuild | 318 | 0.8 | 0.8 | 1.2 | 2.0 |
| serialise | 318 | 0.3 | 0.2 | 0.4 | 2.1 |
| total | 318 | 2353.2 | 2108.2 | 4524.4 | 7043.5 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
