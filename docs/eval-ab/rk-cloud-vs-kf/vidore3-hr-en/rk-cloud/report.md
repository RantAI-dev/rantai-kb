# Retrieval evaluation — rk-cloud

- dataset: `vidore3-hr-en`
- harness commit: `ead4e48-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-31T01:57:26.693062422+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.946 [0.932–0.963] | 0.994 [0.986–1.000] | 0.934 [0.914–0.956] | 0.887 [0.855–0.921] | 0.105 [0.103–0.108] |
| page | 318 | 0.710 [0.682–0.741] | 0.733 [0.696–0.768] | 0.810 [0.778–0.844] | 0.711 [0.664–0.761] | 0.287 [0.266–0.310] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.946 [0.932–0.963] | 0.994 [0.986–1.000] | 0.934 [0.914–0.956] | 0.887 [0.855–0.921] | 0.105 [0.103–0.108] |
| page (answered only) | 318 | 0.710 [0.682–0.741] | 0.733 [0.696–0.768] | 0.810 [0.778–0.844] | 0.711 [0.664–0.761] | 0.287 [0.266–0.310] |

Latency over 318 answered queries (ms): mean 613.5, p50 590.8, p95 1632.9, p99 1846.3

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 588.5, p95 1630.5, p99 1843.4. Client-measured: p50 590.8, p95 1632.9, p99 1846.3 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 7.8 | 7.2 | 11.1 | 14.5 |
| context | 318 | 0.3 | 0.3 | 0.4 | 0.5 |
| detail.engineTotal | 318 | 609.5 | 586.9 | 1629.0 | 1841.8 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 7.8 | 7.2 | 11.1 | 14.5 |
| detail.pageArm | 318 | 10.4 | 10.1 | 13.6 | 16.3 |
| detail.queryEmbed | 318 | 0.6 | 0.6 | 0.8 | 0.9 |
| detail.scopeCheck | 318 | 2.2 | 1.9 | 4.0 | 4.8 |
| detail.vectorArm | 318 | 7.8 | 7.2 | 11.1 | 14.5 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.5 | 1.4 | 2.1 | 3.6 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 26.1 | 26.3 | 41.3 | 46.9 |
| pages | 318 | 233.4 | 9.6 | 947.8 | 1221.6 |
| rerank | 318 | 337.5 | 532.6 | 595.2 | 881.4 |
| scopeBuild | 318 | 0.9 | 0.8 | 1.5 | 2.0 |
| serialise | 318 | 0.5 | 0.5 | 0.6 | 0.8 |
| total | 318 | 611.1 | 588.5 | 1630.5 | 1843.4 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
