# Retrieval evaluation — pdf20-bge20

- dataset: `vidore3-hr-en`
- harness commit: `b0394eb`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T07:46:42.463678765+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.918 [0.895–0.940] | 0.969 [0.950–0.987] | 0.905 [0.878–0.929] | 0.855 [0.818–0.893] | 0.102 [0.099–0.105] |
| page | 318 | 0.477 [0.440–0.512] | 0.527 [0.484–0.571] | 0.599 [0.556–0.643] | 0.453 [0.396–0.509] | 0.197 [0.181–0.216] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.918 [0.895–0.940] | 0.969 [0.950–0.987] | 0.905 [0.878–0.929] | 0.855 [0.818–0.893] | 0.102 [0.099–0.105] |
| page (answered only) | 318 | 0.477 [0.440–0.512] | 0.527 [0.484–0.571] | 0.599 [0.556–0.643] | 0.453 [0.396–0.509] | 0.197 [0.181–0.216] |

Latency over 318 answered queries (ms): mean 2511.0, p50 2251.4, p95 3637.4, p99 8282.2

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2249.7, p95 3635.5, p99 8280.2. Client-measured: p50 2251.4, p95 3637.4, p99 8282.2 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 10.5 | 10.1 | 12.9 | 16.2 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.3 |
| detail.engineTotal | 318 | 2508.1 | 2248.1 | 3634.5 | 8279.1 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 10.5 | 10.1 | 12.9 | 16.2 |
| detail.queryEmbed | 318 | 85.1 | 90.6 | 96.0 | 114.9 |
| detail.scopeCheck | 318 | 1.1 | 1.1 | 1.4 | 1.7 |
| detail.vectorArm | 318 | 10.5 | 10.1 | 12.9 | 16.2 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.2 | 1.1 | 1.8 | 3.2 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 17.6 | 17.8 | 30.4 | 37.5 |
| rerank | 318 | 2392.4 | 2129.2 | 3496.2 | 8157.9 |
| scopeBuild | 318 | 0.8 | 0.8 | 1.1 | 1.9 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.4 |
| total | 318 | 2509.2 | 2249.7 | 3635.5 | 8280.2 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
