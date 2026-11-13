# Retrieval evaluation — ing5-qwen3vl2b-standalone

- dataset: `images-v1`
- harness commit: `14b887a1`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-13T15:08:35.186664356+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 350
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.904 [0.876–0.934] | 0.985 [0.965–1.000] | 0.877 [0.844–0.913] | 0.800 [0.745–0.855] | 0.099 [0.097–0.100] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.904 [0.876–0.934] | 0.985 [0.965–1.000] | 0.877 [0.844–0.913] | 0.800 [0.745–0.855] | 0.099 [0.097–0.100] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 15.9, p50 15.7, p95 18.7, p99 20.8

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 14.1, p95 17.1, p99 19.2. Client-measured: p50 15.7, p95 18.7, p99 20.8 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 5.1 | 5.0 | 6.4 | 7.2 |
| context | 200 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 200 | 12.1 | 11.9 | 14.4 | 15.6 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.globalArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 5.1 | 5.0 | 6.4 | 7.2 |
| detail.pageArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 200 | 0.2 | 0.1 | 0.2 | 0.4 |
| detail.scopeCheck | 200 | 1.6 | 1.6 | 2.1 | 2.6 |
| detail.vectorArm | 200 | 5.1 | 5.0 | 6.4 | 7.2 |
| detail.visionRerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.8 | 1.8 | 2.4 | 2.7 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 3.2 | 3.1 | 4.2 | 5.0 |
| pages | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 200 | 1.9 | 1.8 | 2.9 | 3.4 |
| serialise | 200 | 0.1 | 0.1 | 0.2 | 0.2 |
| total | 200 | 14.3 | 14.1 | 17.1 | 19.2 |

Query embeddings: 200 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
