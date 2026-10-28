# Retrieval evaluation — lite-e5s-hr-384

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T19:24:46.303783189+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.888 [0.862–0.910] | 0.980 [0.964–0.992] | 0.863 [0.831–0.892] | 0.783 [0.733–0.827] | 0.103 [0.101–0.106] |
| page | 318 | 0.509 [0.472–0.542] | 0.556 [0.515–0.598] | 0.640 [0.595–0.682] | 0.525 [0.465–0.579] | 0.210 [0.192–0.229] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.888 [0.862–0.910] | 0.980 [0.964–0.992] | 0.863 [0.831–0.892] | 0.783 [0.733–0.827] | 0.103 [0.101–0.106] |
| page (answered only) | 318 | 0.509 [0.472–0.542] | 0.556 [0.515–0.598] | 0.640 [0.595–0.682] | 0.525 [0.465–0.579] | 0.210 [0.192–0.229] |

Latency over 318 answered queries (ms): mean 50.7, p50 50.2, p95 63.9, p99 70.9

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 48.5, p95 62.3, p99 69.0. Client-measured: p50 50.2, p95 63.9, p99 70.9 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 8.1 | 7.6 | 11.4 | 13.7 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 318 | 47.8 | 47.5 | 61.2 | 67.6 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 8.1 | 7.6 | 11.4 | 13.7 |
| detail.pageArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 318 | 22.7 | 22.3 | 30.3 | 35.5 |
| detail.scopeCheck | 318 | 1.1 | 1.0 | 2.2 | 4.1 |
| detail.vectorArm | 318 | 8.1 | 7.6 | 11.4 | 13.7 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.1 | 1.0 | 1.4 | 1.7 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 14.6 | 14.4 | 26.1 | 30.5 |
| pages | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 0.8 | 0.8 | 1.2 | 2.1 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.3 |
| total | 318 | 49.0 | 48.5 | 62.3 | 69.0 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
