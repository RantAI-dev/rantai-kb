# Retrieval evaluation — ab-base

- dataset: `miracl-id-200`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-28T13:45:39.871142364+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.791 [0.762–0.822] | 0.939 [0.914–0.961] | 0.808 [0.765–0.847] | 0.695 [0.630–0.755] | 0.292 [0.264–0.320] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.791 [0.762–0.822] | 0.939 [0.914–0.961] | 0.808 [0.765–0.847] | 0.695 [0.630–0.755] | 0.292 [0.264–0.320] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 125.5, p50 118.9, p95 179.0, p99 209.3

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 116.4, p95 176.1, p99 204.9. Client-measured: p50 118.9, p95 179.0, p99 209.3 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 59.8 | 56.4 | 85.5 | 101.8 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 200 | 114.2 | 106.2 | 166.6 | 196.8 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 59.8 | 56.4 | 85.5 | 101.8 |
| detail.pageArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 200 | 44.1 | 33.1 | 77.9 | 100.9 |
| detail.scopeCheck | 200 | 2.0 | 1.5 | 4.5 | 6.5 |
| detail.vectorArm | 200 | 59.8 | 56.4 | 85.5 | 101.8 |
| detail.visionRerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.5 | 1.0 | 4.0 | 4.9 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 6.6 | 6.3 | 10.5 | 14.7 |
| pages | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 200 | 8.6 | 8.3 | 11.7 | 13.2 |
| serialise | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| total | 200 | 122.9 | 116.4 | 176.1 | 204.9 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
