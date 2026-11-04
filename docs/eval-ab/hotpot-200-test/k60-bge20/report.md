# Retrieval evaluation — k60-bge20

- dataset: `hotpot-200-test`
- harness commit: `4c01a7d-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T05:22:47.014016404+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 995
- queries: 100 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 100 | 0.928 [0.902–0.952] | 0.945 [0.910–0.970] | 0.990 [0.975–1.000] | 0.980 [0.950–1.000] | 0.189 [0.182–0.194] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 100 | 0.928 [0.902–0.952] | 0.945 [0.910–0.970] | 0.990 [0.975–1.000] | 0.980 [0.950–1.000] | 0.189 [0.182–0.194] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 100 answered queries (ms): mean 1516.9, p50 1479.1, p95 1986.6, p99 2250.0

### Server-side stage timings (ms)

Server-side total over 100 responses: p50 1474.4, p95 1982.8, p99 2245.6. Client-measured: p50 1479.1, p95 1986.6, p99 2250.0 over 100 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 100 | 24.0 | 15.7 | 38.6 | 217.0 |
| context | 100 | 1.3 | 0.1 | 0.7 | 40.6 |
| detail.engineTotal | 100 | 1505.9 | 1471.6 | 1979.9 | 2236.4 |
| detail.entityArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 100 | 24.0 | 15.7 | 38.6 | 217.0 |
| detail.queryEmbed | 100 | 82.2 | 89.8 | 95.1 | 113.1 |
| detail.scopeCheck | 100 | 1.7 | 1.3 | 3.4 | 6.9 |
| detail.vectorArm | 100 | 24.0 | 15.7 | 38.6 | 217.0 |
| enrich | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 100 | 1.7 | 0.8 | 3.1 | 8.4 |
| fusion | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 100 | 7.2 | 2.9 | 10.5 | 78.6 |
| rerank | 100 | 1386.4 | 1368.9 | 1757.8 | 1918.8 |
| scopeBuild | 100 | 4.2 | 3.1 | 8.1 | 35.3 |
| serialise | 100 | 0.2 | 0.1 | 1.0 | 3.3 |
| total | 100 | 1510.8 | 1474.4 | 1982.8 | 2245.6 |

Query embeddings: 0 cache hit(s), 100 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
