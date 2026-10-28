# Retrieval evaluation — vmm-vis-ti-pr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T12:19:15.021662952+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 58, noted: 0)
- degradations reported by the server: vision_rerank_unavailable

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.946 [0.928–0.963] | 0.988 [0.976–0.997] | 0.936 [0.915–0.957] | 0.896 [0.862–0.931] | 0.105 [0.102–0.108] |
| page | 318 | 0.720 [0.693–0.751] | 0.741 [0.708–0.774] | 0.807 [0.775–0.842] | 0.704 [0.657–0.755] | 0.292 [0.270–0.317] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.946 [0.928–0.963] | 0.988 [0.976–0.997] | 0.936 [0.915–0.957] | 0.896 [0.862–0.931] | 0.105 [0.102–0.108] |
| page (answered only) | 318 | 0.720 [0.693–0.751] | 0.741 [0.708–0.774] | 0.807 [0.775–0.842] | 0.704 [0.657–0.755] | 0.292 [0.270–0.317] |

Latency over 318 answered queries (ms): mean 7610.0, p50 7555.3, p95 9329.4, p99 10451.4

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 7551.3, p95 9326.3, p99 10449.7. Client-measured: p50 7555.3, p95 9329.4, p99 10451.4 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 21.7 | 12.6 | 54.8 | 87.9 |
| context | 318 | 0.7 | 0.2 | 2.6 | 9.0 |
| detail.engineTotal | 318 | 7602.9 | 7550.3 | 9323.5 | 10448.5 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 21.7 | 12.6 | 54.8 | 87.9 |
| detail.pageArm | 318 | 27.6 | 15.0 | 58.9 | 96.8 |
| detail.queryEmbed | 318 | 1.9 | 1.0 | 3.5 | 6.8 |
| detail.scopeCheck | 318 | 3.0 | 1.2 | 6.0 | 42.1 |
| detail.vectorArm | 318 | 21.7 | 12.6 | 54.8 | 87.9 |
| detail.visionRerank | 318 | 6789.3 | 6684.5 | 8001.7 | 8002.6 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.7 | 1.3 | 3.6 | 7.2 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 34.5 | 18.8 | 111.0 | 235.6 |
| pages | 318 | 7171.0 | 7039.1 | 8721.8 | 9791.8 |
| rerank | 318 | 367.0 | 521.8 | 595.6 | 1149.0 |
| scopeBuild | 318 | 1.3 | 0.9 | 3.1 | 7.9 |
| serialise | 318 | 0.9 | 0.2 | 5.1 | 7.1 |
| total | 318 | 7605.4 | 7551.3 | 9326.3 | 10449.7 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
