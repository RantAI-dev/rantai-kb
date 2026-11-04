# Retrieval evaluation — final20

- dataset: `vidore3-hr-en`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T10:33:20.330925795+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.885 [0.861–0.911] | 0.972 [0.953–0.988] | 0.861 [0.831–0.892] | 0.783 [0.736–0.827] | 0.103 [0.100–0.105] |
| page | 318 | 0.468 [0.431–0.502] | 0.517 [0.472–0.559] | 0.607 [0.559–0.654] | 0.481 [0.428–0.541] | 0.186 [0.171–0.204] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.885 [0.861–0.911] | 0.972 [0.953–0.988] | 0.861 [0.831–0.892] | 0.783 [0.736–0.827] | 0.103 [0.100–0.105] |
| page (answered only) | 318 | 0.468 [0.431–0.502] | 0.517 [0.472–0.559] | 0.607 [0.559–0.654] | 0.481 [0.428–0.541] | 0.186 [0.171–0.204] |

Latency over 318 answered queries (ms): mean 128.4, p50 126.4, p95 181.3, p99 225.0

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 124.0, p95 172.7, p99 222.9. Client-measured: p50 126.4, p95 181.3, p99 225.0 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 10.8 | 9.9 | 16.6 | 20.9 |
| context | 318 | 0.3 | 0.1 | 0.9 | 3.0 |
| detail.engineTotal | 318 | 124.0 | 122.6 | 171.0 | 221.2 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 10.8 | 9.9 | 16.6 | 20.9 |
| detail.queryEmbed | 318 | 84.1 | 91.9 | 101.8 | 118.6 |
| detail.scopeCheck | 318 | 1.1 | 1.1 | 1.4 | 2.6 |
| detail.vectorArm | 318 | 10.8 | 9.9 | 16.6 | 20.9 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.3 | 1.2 | 2.1 | 3.0 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 26.0 | 19.4 | 65.5 | 92.9 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 318 | 1.0 | 0.9 | 1.6 | 2.5 |
| serialise | 318 | 0.6 | 0.2 | 2.6 | 6.0 |
| total | 318 | 125.6 | 124.0 | 172.7 | 222.9 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
