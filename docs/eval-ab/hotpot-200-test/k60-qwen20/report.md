# Retrieval evaluation — k60-qwen20

- dataset: `hotpot-200-test`
- harness commit: `4c01a7d-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T05:27:17.653379404+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 995
- queries: 100 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 100 | 0.889 [0.860–0.919] | 0.940 [0.905–0.970] | 0.955 [0.922–0.983] | 0.920 [0.860–0.970] | 0.188 [0.181–0.194] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 100 | 0.889 [0.860–0.919] | 0.940 [0.905–0.970] | 0.955 [0.922–0.983] | 0.920 [0.860–0.970] | 0.188 [0.181–0.194] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 100 answered queries (ms): mean 2686.0, p50 2653.2, p95 3247.4, p99 3516.5

### Server-side stage timings (ms)

Server-side total over 100 responses: p50 2645.1, p95 3222.3, p99 3513.8. Client-measured: p50 2653.2, p95 3247.4, p99 3516.5 over 100 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 100 | 20.0 | 16.4 | 35.1 | 78.4 |
| context | 100 | 0.3 | 0.1 | 0.6 | 2.7 |
| detail.engineTotal | 100 | 2674.5 | 2629.3 | 3189.9 | 3509.9 |
| detail.entityArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 100 | 20.0 | 16.4 | 35.1 | 78.4 |
| detail.queryEmbed | 100 | 82.1 | 90.2 | 93.4 | 130.4 |
| detail.scopeCheck | 100 | 1.6 | 1.3 | 2.6 | 5.9 |
| detail.vectorArm | 100 | 20.0 | 16.4 | 35.1 | 78.4 |
| enrich | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 100 | 1.0 | 0.8 | 1.5 | 4.5 |
| fusion | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 100 | 7.6 | 3.0 | 5.9 | 166.6 |
| rerank | 100 | 2561.3 | 2547.1 | 3075.8 | 3398.3 |
| scopeBuild | 100 | 4.9 | 2.9 | 6.7 | 34.5 |
| serialise | 100 | 0.5 | 0.1 | 0.2 | 5.0 |
| total | 100 | 2680.6 | 2645.1 | 3222.3 | 3513.8 |

Query embeddings: 0 cache hit(s), 100 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
