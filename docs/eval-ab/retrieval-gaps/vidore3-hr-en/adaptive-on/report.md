# Retrieval evaluation — adaptive-on

- dataset: `vidore3-hr-en`
- harness commit: `9b9cf7fe-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-04T14:26:56.094318992+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.949 [0.934–0.965] | 0.996 [0.988–1.000] | 0.936 [0.917–0.957] | 0.890 [0.855–0.925] | 0.106 [0.103–0.108] |
| page | 318 | 0.710 [0.682–0.741] | 0.732 [0.696–0.767] | 0.812 [0.780–0.848] | 0.714 [0.667–0.764] | 0.286 [0.265–0.309] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.949 [0.934–0.965] | 0.996 [0.988–1.000] | 0.936 [0.917–0.957] | 0.890 [0.855–0.925] | 0.106 [0.103–0.108] |
| page (answered only) | 318 | 0.710 [0.682–0.741] | 0.732 [0.696–0.767] | 0.812 [0.780–0.848] | 0.714 [0.667–0.764] | 0.286 [0.265–0.309] |

Latency over 318 answered queries (ms): mean 46.2, p50 47.4, p95 61.2, p99 68.2

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 45.1, p95 58.9, p99 65.9. Client-measured: p50 47.4, p95 61.2, p99 68.2 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 7.3 | 6.9 | 10.3 | 11.4 |
| context | 318 | 0.3 | 0.3 | 0.4 | 0.5 |
| detail.engineTotal | 318 | 42.4 | 43.4 | 57.3 | 64.4 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 7.3 | 6.9 | 10.3 | 11.4 |
| detail.pageArm | 318 | 9.4 | 10.0 | 12.2 | 13.7 |
| detail.queryEmbed | 318 | 0.2 | 0.2 | 0.2 | 0.3 |
| detail.scopeCheck | 318 | 1.8 | 1.1 | 3.8 | 4.4 |
| detail.vectorArm | 318 | 7.3 | 6.9 | 10.3 | 11.4 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.3 | 1.3 | 1.7 | 1.9 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 22.9 | 23.0 | 36.7 | 42.2 |
| pages | 318 | 8.0 | 8.1 | 11.3 | 12.4 |
| rerank | 318 | 0.4 | 0.3 | 0.5 | 1.1 |
| scopeBuild | 318 | 0.8 | 0.8 | 1.0 | 1.3 |
| serialise | 318 | 0.5 | 0.5 | 0.7 | 0.8 |
| total | 318 | 43.9 | 45.1 | 58.9 | 65.9 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
