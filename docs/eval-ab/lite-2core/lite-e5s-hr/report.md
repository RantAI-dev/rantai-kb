# Retrieval evaluation — lite-e5s-hr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T18:48:23.254626204+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.895 [0.870–0.918] | 0.972 [0.955–0.987] | 0.875 [0.845–0.902] | 0.802 [0.758–0.843] | 0.102 [0.100–0.105] |
| page | 318 | 0.545 [0.509–0.580] | 0.598 [0.556–0.638] | 0.663 [0.624–0.706] | 0.538 [0.487–0.594] | 0.223 [0.205–0.242] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.895 [0.870–0.918] | 0.972 [0.955–0.987] | 0.875 [0.845–0.902] | 0.802 [0.758–0.843] | 0.102 [0.100–0.105] |
| page (answered only) | 318 | 0.545 [0.509–0.580] | 0.598 [0.556–0.638] | 0.663 [0.624–0.706] | 0.538 [0.487–0.594] | 0.223 [0.205–0.242] |

Latency over 318 answered queries (ms): mean 51.9, p50 51.0, p95 66.6, p99 73.9

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 49.4, p95 64.9, p99 72.2. Client-measured: p50 51.0, p95 66.6, p99 73.9 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 8.2 | 7.6 | 11.9 | 14.9 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 318 | 49.0 | 48.3 | 63.7 | 70.3 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 8.2 | 7.6 | 11.9 | 14.9 |
| detail.pageArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 318 | 23.9 | 23.4 | 32.8 | 36.7 |
| detail.scopeCheck | 318 | 1.1 | 1.0 | 1.7 | 4.1 |
| detail.vectorArm | 318 | 8.2 | 7.6 | 11.9 | 14.9 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.1 | 1.0 | 1.4 | 2.3 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 14.4 | 13.9 | 27.1 | 30.6 |
| pages | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 318 | 0.9 | 0.8 | 1.3 | 2.9 |
| serialise | 318 | 0.2 | 0.2 | 0.2 | 0.3 |
| total | 318 | 50.1 | 49.4 | 64.9 | 72.2 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
