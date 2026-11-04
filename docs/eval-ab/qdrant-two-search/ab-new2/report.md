# Retrieval evaluation — ab-new2

- dataset: `miracl-id-200`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-28T13:57:45.466777822+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.792 [0.761–0.821] | 0.939 [0.913–0.960] | 0.806 [0.760–0.845] | 0.700 [0.635–0.760] | 0.293 [0.265–0.321] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.792 [0.761–0.821] | 0.939 [0.913–0.960] | 0.806 [0.760–0.845] | 0.700 [0.635–0.760] | 0.293 [0.265–0.321] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 62.1, p50 58.0, p95 94.4, p99 113.9

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 56.3, p95 92.8, p99 112.1. Client-measured: p50 58.0, p95 94.4, p99 113.9 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 20.4 | 19.8 | 27.8 | 36.0 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 200 | 53.4 | 49.2 | 86.1 | 102.8 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 20.4 | 19.8 | 27.8 | 36.0 |
| detail.pageArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 200 | 26.2 | 22.7 | 51.0 | 76.0 |
| detail.scopeCheck | 200 | 1.6 | 1.4 | 3.2 | 4.1 |
| detail.vectorArm | 200 | 20.4 | 19.8 | 27.8 | 36.0 |
| detail.visionRerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.0 | 0.9 | 1.3 | 1.4 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 4.0 | 3.6 | 6.8 | 8.5 |
| pages | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 200 | 6.9 | 6.5 | 9.4 | 12.4 |
| serialise | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| total | 200 | 60.4 | 56.3 | 92.8 | 112.1 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
