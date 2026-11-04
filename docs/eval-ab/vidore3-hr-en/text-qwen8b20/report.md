# Retrieval evaluation — text-qwen8b20

- dataset: `vidore3-hr-en`
- harness commit: `b0394eb`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T06:35:25.737339519+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 14
- queries: 318 (failed: 0, degraded: 3, noted: 0)
- degradations reported by the server: rerank_unavailable

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.912 [0.887–0.935] | 0.955 [0.932–0.976] | 0.901 [0.874–0.928] | 0.855 [0.818–0.893] | 0.101 [0.097–0.104] |
| page | 318 | 0.484 [0.449–0.520] | 0.538 [0.496–0.580] | 0.611 [0.568–0.653] | 0.456 [0.403–0.513] | 0.195 [0.179–0.212] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.912 [0.887–0.935] | 0.955 [0.932–0.976] | 0.901 [0.874–0.928] | 0.855 [0.818–0.893] | 0.101 [0.097–0.104] |
| page (answered only) | 318 | 0.484 [0.449–0.520] | 0.538 [0.496–0.580] | 0.611 [0.568–0.653] | 0.456 [0.403–0.513] | 0.195 [0.179–0.212] |

Latency over 318 answered queries (ms): mean 940.9, p50 826.0, p95 1526.1, p99 2012.8

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 821.9, p95 1513.3, p99 2007.7. Client-measured: p50 826.0, p95 1526.1, p99 2012.8 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 15.6 | 12.4 | 27.0 | 50.6 |
| context | 318 | 0.6 | 0.1 | 1.9 | 8.3 |
| detail.engineTotal | 318 | 932.8 | 820.5 | 1511.9 | 2003.9 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 15.6 | 12.4 | 27.0 | 50.6 |
| detail.queryEmbed | 318 | 102.8 | 111.9 | 120.3 | 140.1 |
| detail.scopeCheck | 318 | 1.6 | 1.2 | 2.3 | 7.2 |
| detail.vectorArm | 318 | 15.6 | 12.4 | 27.0 | 50.6 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.8 | 1.4 | 3.5 | 6.6 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 36.7 | 20.2 | 100.5 | 260.6 |
| rerank | 318 | 771.3 | 654.6 | 1328.9 | 1840.9 |
| scopeBuild | 318 | 1.6 | 0.9 | 4.1 | 16.3 |
| serialise | 318 | 0.8 | 0.1 | 3.6 | 6.3 |
| total | 318 | 935.4 | 821.9 | 1513.3 | 2007.7 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
