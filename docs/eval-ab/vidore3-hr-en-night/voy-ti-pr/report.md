# Retrieval evaluation — voy-ti-pr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T21:53:08.753428008+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.938 [0.919–0.958] | 0.983 [0.969–0.995] | 0.926 [0.904–0.950] | 0.877 [0.843–0.915] | 0.104 [0.101–0.107] |
| page | 318 | 0.705 [0.677–0.736] | 0.730 [0.694–0.763] | 0.804 [0.772–0.839] | 0.701 [0.654–0.752] | 0.284 [0.263–0.306] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.938 [0.919–0.958] | 0.983 [0.969–0.995] | 0.926 [0.904–0.950] | 0.877 [0.843–0.915] | 0.104 [0.101–0.107] |
| page (answered only) | 318 | 0.705 [0.677–0.736] | 0.730 [0.694–0.763] | 0.804 [0.772–0.839] | 0.701 [0.654–0.752] | 0.284 [0.263–0.306] |

Latency over 318 answered queries (ms): mean 1370.8, p50 1307.4, p95 1757.6, p99 2271.2

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 1302.7, p95 1754.2, p99 2260.6. Client-measured: p50 1307.4, p95 1757.6, p99 2271.2 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 58.1 | 43.6 | 102.6 | 341.5 |
| context | 318 | 0.3 | 0.1 | 1.0 | 3.6 |
| detail.engineTotal | 318 | 1363.7 | 1301.5 | 1752.4 | 2259.0 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 58.1 | 43.6 | 102.6 | 341.5 |
| detail.queryEmbed | 318 | 1.2 | 0.8 | 3.6 | 8.3 |
| detail.scopeCheck | 318 | 1.6 | 1.2 | 3.3 | 5.8 |
| detail.vectorArm | 318 | 58.1 | 43.6 | 102.6 | 341.5 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 2.1 | 1.7 | 4.5 | 7.0 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 30.6 | 20.4 | 80.8 | 148.0 |
| rerank | 318 | 530.7 | 516.3 | 611.7 | 752.0 |
| scopeBuild | 318 | 1.4 | 1.2 | 2.7 | 4.4 |
| serialise | 318 | 0.6 | 0.2 | 1.8 | 9.8 |
| total | 318 | 1365.9 | 1302.7 | 1754.2 | 2260.6 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
