# Retrieval evaluation — ab-new60

- dataset: `miracl-id-200`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-28T13:52:00.040490864+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.789 [0.758–0.820] | 0.939 [0.914–0.961] | 0.805 [0.761–0.845] | 0.695 [0.635–0.755] | 0.292 [0.264–0.320] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.789 [0.758–0.820] | 0.939 [0.914–0.961] | 0.805 [0.761–0.845] | 0.695 [0.635–0.755] | 0.292 [0.264–0.320] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 89.8, p50 80.7, p95 128.3, p99 149.6

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 78.6, p95 125.9, p99 147.0. Client-measured: p50 80.7, p95 128.3, p99 149.6 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 27.4 | 25.7 | 38.1 | 51.6 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.2 |
| detail.engineTotal | 200 | 79.0 | 69.8 | 115.9 | 133.8 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 27.4 | 25.7 | 38.1 | 51.6 |
| detail.pageArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 200 | 41.4 | 31.4 | 71.7 | 80.6 |
| detail.scopeCheck | 200 | 1.9 | 1.5 | 3.8 | 5.8 |
| detail.vectorArm | 200 | 27.4 | 25.7 | 38.1 | 51.6 |
| detail.visionRerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.4 | 1.1 | 2.9 | 4.3 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 6.8 | 6.0 | 12.9 | 14.1 |
| pages | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 200 | 8.5 | 8.0 | 12.2 | 14.4 |
| serialise | 200 | 0.1 | 0.1 | 0.1 | 0.2 |
| total | 200 | 87.7 | 78.6 | 125.9 | 147.0 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
