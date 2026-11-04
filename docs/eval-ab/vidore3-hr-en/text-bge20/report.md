# Retrieval evaluation — text-bge20

- dataset: `vidore3-hr-en`
- harness commit: `b0394eb`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T06:29:55.954132055+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.912 [0.887–0.936] | 0.955 [0.933–0.976] | 0.902 [0.874–0.928] | 0.855 [0.818–0.893] | 0.101 [0.097–0.104] |
| page | 318 | 0.475 [0.439–0.512] | 0.524 [0.479–0.566] | 0.595 [0.551–0.640] | 0.447 [0.390–0.500] | 0.197 [0.181–0.216] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.912 [0.887–0.936] | 0.955 [0.933–0.976] | 0.902 [0.874–0.928] | 0.855 [0.818–0.893] | 0.101 [0.097–0.104] |
| page (answered only) | 318 | 0.475 [0.439–0.512] | 0.524 [0.479–0.566] | 0.595 [0.551–0.640] | 0.447 [0.390–0.500] | 0.197 [0.181–0.216] |

Latency over 318 answered queries (ms): mean 2627.0, p50 2327.1, p95 4492.9, p99 8383.6

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2320.4, p95 4443.2, p99 8353.1. Client-measured: p50 2327.1, p95 4492.9, p99 8383.6 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 20.6 | 13.1 | 49.5 | 104.4 |
| context | 318 | 3.9 | 0.1 | 8.9 | 32.6 |
| detail.engineTotal | 318 | 2594.3 | 2315.1 | 4370.3 | 8344.9 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 20.6 | 13.1 | 49.5 | 104.4 |
| detail.queryEmbed | 318 | 100.4 | 91.5 | 213.8 | 267.8 |
| detail.scopeCheck | 318 | 2.1 | 1.2 | 5.8 | 15.2 |
| detail.vectorArm | 318 | 20.6 | 13.1 | 49.5 | 104.4 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| figures | 318 | 2.9 | 1.5 | 10.0 | 26.5 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 71.7 | 25.7 | 286.3 | 500.1 |
| rerank | 318 | 2391.3 | 2127.4 | 4098.1 | 8076.0 |
| scopeBuild | 318 | 2.9 | 1.0 | 9.3 | 25.9 |
| serialise | 318 | 4.9 | 0.1 | 11.4 | 23.1 |
| total | 318 | 2606.0 | 2320.4 | 4443.2 | 8353.1 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
