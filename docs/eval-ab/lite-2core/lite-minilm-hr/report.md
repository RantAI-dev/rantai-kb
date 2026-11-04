# Retrieval evaluation — lite-minilm-hr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-25T05:02:00.218183321+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.893 [0.870–0.918] | 0.970 [0.953–0.986] | 0.872 [0.844–0.901] | 0.802 [0.761–0.846] | 0.102 [0.099–0.105] |
| page | 318 | 0.534 [0.498–0.569] | 0.592 [0.549–0.633] | 0.655 [0.613–0.696] | 0.528 [0.475–0.582] | 0.218 [0.202–0.236] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.893 [0.870–0.918] | 0.970 [0.953–0.986] | 0.872 [0.844–0.901] | 0.802 [0.761–0.846] | 0.102 [0.099–0.105] |
| page (answered only) | 318 | 0.534 [0.498–0.569] | 0.592 [0.549–0.633] | 0.655 [0.613–0.696] | 0.528 [0.475–0.582] | 0.218 [0.202–0.236] |

Latency over 318 answered queries (ms): mean 40.2, p50 39.3, p95 54.9, p99 59.8

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 37.8, p95 52.7, p99 58.2. Client-measured: p50 39.3, p95 54.9, p99 59.8 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 8.4 | 8.0 | 11.5 | 16.7 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 318 | 37.5 | 36.7 | 51.3 | 57.1 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 8.4 | 8.0 | 11.5 | 16.7 |
| detail.pageArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 318 | 12.6 | 11.9 | 19.1 | 30.2 |
| detail.scopeCheck | 318 | 1.1 | 1.0 | 1.5 | 3.1 |
| detail.vectorArm | 318 | 8.4 | 8.0 | 11.5 | 16.7 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.0 | 1.0 | 1.3 | 2.1 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 14.2 | 14.2 | 26.6 | 28.9 |
| pages | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 0.8 | 0.7 | 1.2 | 2.0 |
| serialise | 318 | 0.2 | 0.2 | 0.2 | 0.3 |
| total | 318 | 38.6 | 37.8 | 52.7 | 58.2 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
