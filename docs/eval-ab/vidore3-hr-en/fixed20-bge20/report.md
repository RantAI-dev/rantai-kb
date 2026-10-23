# Retrieval evaluation — fixed20-bge20

- dataset: `vidore3-hr-en`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T10:17:58.334869893+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.919 [0.897–0.941] | 0.972 [0.953–0.988] | 0.905 [0.878–0.931] | 0.852 [0.811–0.890] | 0.103 [0.100–0.105] |
| page | 318 | 0.536 [0.500–0.568] | 0.577 [0.537–0.620] | 0.669 [0.626–0.712] | 0.541 [0.484–0.594] | 0.212 [0.195–0.231] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.919 [0.897–0.941] | 0.972 [0.953–0.988] | 0.905 [0.878–0.931] | 0.852 [0.811–0.890] | 0.103 [0.100–0.105] |
| page (answered only) | 318 | 0.536 [0.500–0.568] | 0.577 [0.537–0.620] | 0.669 [0.626–0.712] | 0.541 [0.484–0.594] | 0.212 [0.195–0.231] |

Latency over 318 answered queries (ms): mean 2379.2, p50 2116.0, p95 4531.3, p99 7229.0

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2114.4, p95 4529.5, p99 7226.6. Client-measured: p50 2116.0, p95 4531.3, p99 7229.0 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 11.6 | 10.2 | 18.9 | 24.6 |
| context | 318 | 0.3 | 0.1 | 1.3 | 3.3 |
| detail.engineTotal | 318 | 2375.1 | 2111.7 | 4528.3 | 7223.9 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 11.6 | 10.2 | 18.9 | 24.6 |
| detail.queryEmbed | 318 | 83.0 | 91.4 | 97.5 | 116.7 |
| detail.scopeCheck | 318 | 1.1 | 1.1 | 1.5 | 3.0 |
| detail.vectorArm | 318 | 11.6 | 10.2 | 18.9 | 24.6 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.4 | 1.2 | 3.0 | 3.9 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 23.1 | 19.6 | 51.3 | 80.2 |
| rerank | 318 | 2254.3 | 1989.9 | 4415.8 | 7095.1 |
| scopeBuild | 318 | 1.0 | 0.8 | 1.5 | 3.9 |
| serialise | 318 | 0.5 | 0.2 | 2.5 | 5.7 |
| total | 318 | 2376.7 | 2114.4 | 4529.5 | 7226.6 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
