# Retrieval evaluation — text

- dataset: `vidore3-hr-en`
- harness commit: `b0394eb`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T06:15:56.762990127+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.880 [0.852–0.907] | 0.952 [0.929–0.975] | 0.861 [0.830–0.891] | 0.783 [0.736–0.827] | 0.100 [0.097–0.104] |
| page | 318 | 0.416 [0.383–0.451] | 0.482 [0.439–0.523] | 0.526 [0.480–0.568] | 0.377 [0.321–0.428] | 0.180 [0.164–0.197] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.880 [0.852–0.907] | 0.952 [0.929–0.975] | 0.861 [0.830–0.891] | 0.783 [0.736–0.827] | 0.100 [0.097–0.104] |
| page (answered only) | 318 | 0.416 [0.383–0.451] | 0.482 [0.439–0.523] | 0.526 [0.480–0.568] | 0.377 [0.321–0.428] | 0.180 [0.164–0.197] |

Latency over 318 answered queries (ms): mean 117.3, p50 122.2, p95 142.8, p99 150.5

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 119.9, p95 141.3, p99 148.4. Client-measured: p50 122.2, p95 142.8, p99 150.5 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 16.3 | 15.9 | 23.3 | 25.4 |
| context | 318 | 0.1 | 0.1 | 0.1 | 0.2 |
| detail.engineTotal | 318 | 113.9 | 118.4 | 139.4 | 147.3 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 16.3 | 15.9 | 23.3 | 25.4 |
| detail.queryEmbed | 318 | 80.3 | 90.1 | 93.0 | 94.2 |
| detail.scopeCheck | 318 | 1.4 | 1.1 | 2.7 | 3.7 |
| detail.vectorArm | 318 | 16.3 | 15.9 | 23.3 | 25.4 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.4 | 1.2 | 2.8 | 3.7 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 14.4 | 13.6 | 27.8 | 36.1 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 1.2 | 1.1 | 2.2 | 2.9 |
| serialise | 318 | 0.1 | 0.1 | 0.2 | 0.4 |
| total | 318 | 115.3 | 119.9 | 141.3 | 148.4 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
