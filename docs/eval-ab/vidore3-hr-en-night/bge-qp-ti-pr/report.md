# Retrieval evaluation — bge-qp-ti-pr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T02:57:30.256784667+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.924 [0.901–0.943] | 0.979 [0.962–0.992] | 0.909 [0.881–0.932] | 0.852 [0.811–0.890] | 0.104 [0.101–0.107] |
| page | 318 | 0.628 [0.598–0.659] | 0.686 [0.649–0.720] | 0.731 [0.693–0.767] | 0.607 [0.553–0.657] | 0.267 [0.248–0.290] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.924 [0.901–0.943] | 0.979 [0.962–0.992] | 0.909 [0.881–0.932] | 0.852 [0.811–0.890] | 0.104 [0.101–0.107] |
| page (answered only) | 318 | 0.628 [0.598–0.659] | 0.686 [0.649–0.720] | 0.731 [0.693–0.767] | 0.607 [0.553–0.657] | 0.267 [0.248–0.290] |

Latency over 318 answered queries (ms): mean 12950.0, p50 12723.0, p95 16147.9, p99 19595.6

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 12706.4, p95 16139.2, p99 19577.8. Client-measured: p50 12723.0, p95 16147.9, p99 19595.6 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 538.3 | 181.5 | 2053.9 | 2810.8 |
| context | 318 | 2.2 | 0.2 | 10.0 | 17.5 |
| detail.engineTotal | 318 | 12922.0 | 12699.1 | 16130.4 | 19571.3 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 538.3 | 181.5 | 2053.9 | 2810.8 |
| detail.queryEmbed | 318 | 3.2 | 1.5 | 11.9 | 21.3 |
| detail.scopeCheck | 318 | 2.5 | 1.5 | 8.3 | 15.0 |
| detail.vectorArm | 318 | 538.3 | 181.5 | 2053.9 | 2810.8 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.2 |
| figures | 318 | 4.6 | 2.4 | 13.2 | 36.2 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 111.9 | 58.0 | 315.7 | 596.8 |
| rerank | 318 | 2131.8 | 2062.7 | 2746.4 | 3351.1 |
| scopeBuild | 318 | 2.9 | 1.6 | 7.5 | 17.6 |
| serialise | 318 | 3.4 | 0.3 | 14.3 | 37.7 |
| total | 318 | 12929.2 | 12706.4 | 16139.2 | 19577.8 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
