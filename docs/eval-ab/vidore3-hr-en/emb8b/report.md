# Retrieval evaluation — emb8b

- dataset: `vidore3-hr-en`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T11:34:25.902063958+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.896 [0.869–0.921] | 0.974 [0.955–0.990] | 0.873 [0.842–0.904] | 0.802 [0.761–0.849] | 0.103 [0.100–0.106] |
| page | 318 | 0.545 [0.510–0.578] | 0.592 [0.553–0.630] | 0.677 [0.635–0.718] | 0.547 [0.494–0.601] | 0.213 [0.195–0.231] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.896 [0.869–0.921] | 0.974 [0.955–0.990] | 0.873 [0.842–0.904] | 0.802 [0.761–0.849] | 0.103 [0.100–0.106] |
| page (answered only) | 318 | 0.545 [0.510–0.578] | 0.592 [0.553–0.630] | 0.677 [0.635–0.718] | 0.547 [0.494–0.601] | 0.213 [0.195–0.231] |

Latency over 318 answered queries (ms): mean 2776.9, p50 1967.9, p95 8122.1, p99 14520.6

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 1965.8, p95 8104.6, p99 14517.9. Client-measured: p50 1967.9, p95 8122.1, p99 14520.6 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 38.3 | 35.2 | 53.7 | 99.3 |
| context | 318 | 0.6 | 0.2 | 2.7 | 5.0 |
| detail.engineTotal | 318 | 2771.0 | 1964.1 | 8101.1 | 14516.4 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 38.3 | 35.2 | 53.7 | 99.3 |
| detail.queryEmbed | 318 | 2693.8 | 1906.5 | 7917.5 | 14458.8 |
| detail.scopeCheck | 318 | 1.4 | 1.1 | 3.2 | 5.0 |
| detail.vectorArm | 318 | 38.3 | 35.2 | 53.7 | 99.3 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.7 | 1.4 | 3.4 | 5.1 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 34.6 | 26.0 | 94.9 | 139.2 |
| rerank | 318 | 0.1 | 0.0 | 0.3 | 1.5 |
| scopeBuild | 318 | 1.5 | 0.9 | 4.2 | 9.8 |
| serialise | 318 | 0.9 | 0.2 | 4.4 | 7.0 |
| total | 318 | 2773.6 | 1965.8 | 8104.6 | 14517.9 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
