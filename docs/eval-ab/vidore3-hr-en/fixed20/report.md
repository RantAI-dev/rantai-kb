# Retrieval evaluation — fixed20

- dataset: `vidore3-hr-en`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T10:05:19.850558294+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.885 [0.861–0.911] | 0.972 [0.953–0.988] | 0.861 [0.831–0.892] | 0.783 [0.736–0.827] | 0.103 [0.100–0.105] |
| page | 318 | 0.435 [0.398–0.468] | 0.500 [0.458–0.543] | 0.554 [0.509–0.601] | 0.418 [0.365–0.475] | 0.180 [0.164–0.197] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.885 [0.861–0.911] | 0.972 [0.953–0.988] | 0.861 [0.831–0.892] | 0.783 [0.736–0.827] | 0.103 [0.100–0.105] |
| page (answered only) | 318 | 0.435 [0.398–0.468] | 0.500 [0.458–0.543] | 0.554 [0.509–0.601] | 0.418 [0.365–0.475] | 0.180 [0.164–0.197] |

Latency over 318 answered queries (ms): mean 147.1, p50 145.9, p95 198.1, p99 227.2

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 142.0, p95 189.8, p99 219.5. Client-measured: p50 145.9, p95 198.1, p99 227.2 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 19.8 | 18.5 | 29.7 | 38.4 |
| context | 318 | 0.4 | 0.1 | 1.9 | 4.1 |
| detail.engineTotal | 318 | 141.3 | 140.5 | 187.5 | 217.4 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 19.8 | 18.5 | 29.7 | 38.4 |
| detail.queryEmbed | 318 | 82.8 | 91.6 | 97.8 | 103.2 |
| detail.scopeCheck | 318 | 2.2 | 1.4 | 5.0 | 8.1 |
| detail.vectorArm | 318 | 19.8 | 18.5 | 29.7 | 38.4 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 3.0 | 2.3 | 7.1 | 8.8 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 33.0 | 28.9 | 69.8 | 102.6 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 318 | 1.5 | 1.0 | 3.4 | 5.9 |
| serialise | 318 | 0.5 | 0.2 | 2.3 | 6.6 |
| total | 318 | 143.4 | 142.0 | 189.8 | 219.5 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
