# Retrieval evaluation — on

- dataset: `dated-versions`
- harness commit: `9b9cf7fe-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-04T14:15:50.810432249+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 165
- queries: 55 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 55 | 0.993 [0.980–1.000] | 1.000 [1.000–1.000] | 0.991 [0.973–1.000] | 0.982 [0.945–1.000] | 0.100 [0.100–0.100] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 55 | 0.993 [0.980–1.000] | 1.000 [1.000–1.000] | 0.991 [0.973–1.000] | 0.982 [0.945–1.000] | 0.100 [0.100–0.100] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 55 answered queries (ms): mean 13.0, p50 12.8, p95 14.7, p99 16.0

### Server-side stage timings (ms)

Server-side total over 55 responses: p50 11.4, p95 13.2, p99 13.9. Client-measured: p50 12.8, p95 14.7, p99 16.0 over 55 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 55 | 3.9 | 3.9 | 4.6 | 5.1 |
| context | 55 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 55 | 8.5 | 8.4 | 9.9 | 10.3 |
| detail.entityArm | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 55 | 3.9 | 3.9 | 4.6 | 5.1 |
| detail.pageArm | 55 | 1.9 | 1.8 | 2.5 | 2.6 |
| detail.queryEmbed | 55 | 0.2 | 0.2 | 0.2 | 0.3 |
| detail.scopeCheck | 55 | 1.3 | 1.3 | 1.6 | 1.7 |
| detail.vectorArm | 55 | 3.9 | 3.9 | 4.6 | 5.1 |
| detail.visionRerank | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 55 | 0.8 | 0.8 | 1.0 | 1.4 |
| fusion | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 55 | 1.8 | 1.8 | 2.4 | 2.7 |
| pages | 55 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 55 | 0.3 | 0.3 | 0.3 | 0.4 |
| scopeBuild | 55 | 2.9 | 2.8 | 3.9 | 4.2 |
| serialise | 55 | 0.0 | 0.0 | 0.1 | 0.1 |
| total | 55 | 11.6 | 11.4 | 13.2 | 13.9 |

Query embeddings: 55 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
