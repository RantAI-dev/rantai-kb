# Retrieval evaluation — ing5-nemotron-free-standalone

- dataset: `images-v1-half`
- harness commit: `14b887a1`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-13T14:23:23.813323409+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 250
- queries: 100 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 100 | 0.886 [0.834–0.935] | 0.960 [0.920–0.990] | 0.862 [0.798–0.917] | 0.800 [0.710–0.870] | 0.096 [0.092–0.099] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 100 | 0.886 [0.834–0.935] | 0.960 [0.920–0.990] | 0.862 [0.798–0.917] | 0.800 [0.710–0.870] | 0.096 [0.092–0.099] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 100 answered queries (ms): mean 311.1, p50 295.8, p95 389.6, p99 495.0

### Server-side stage timings (ms)

Server-side total over 100 responses: p50 294.1, p95 388.0, p99 493.5. Client-measured: p50 295.8, p95 389.6, p99 495.0 over 100 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 100 | 7.1 | 6.7 | 9.4 | 13.7 |
| context | 100 | 0.1 | 0.1 | 0.1 | 0.2 |
| detail.engineTotal | 100 | 307.3 | 292.1 | 385.3 | 491.5 |
| detail.entityArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.globalArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 100 | 7.1 | 6.7 | 9.4 | 13.7 |
| detail.pageArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 100 | 288.8 | 273.0 | 368.0 | 467.0 |
| detail.scopeCheck | 100 | 2.3 | 2.0 | 3.9 | 5.2 |
| detail.vectorArm | 100 | 7.1 | 6.7 | 9.4 | 13.7 |
| detail.visionRerank | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 100 | 1.9 | 1.8 | 2.8 | 3.2 |
| fusion | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 100 | 7.0 | 6.7 | 11.6 | 12.8 |
| pages | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 100 | 2.0 | 1.8 | 3.7 | 4.6 |
| serialise | 100 | 0.1 | 0.1 | 0.1 | 0.1 |
| total | 100 | 309.5 | 294.1 | 388.0 | 493.5 |

Query embeddings: 0 cache hit(s), 100 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
