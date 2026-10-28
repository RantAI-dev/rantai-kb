# Retrieval evaluation — base

- dataset: `vidore3-hr-en`
- harness commit: `2e64bc9-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T17:28:26.605155717+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.916 [0.894–0.938] | 0.980 [0.964–0.993] | 0.900 [0.873–0.925] | 0.843 [0.805–0.884] | 0.103 [0.101–0.106] |
| page | 318 | 0.572 [0.537–0.605] | 0.625 [0.584–0.665] | 0.705 [0.664–0.750] | 0.585 [0.531–0.642] | 0.225 [0.207–0.243] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.916 [0.894–0.938] | 0.980 [0.964–0.993] | 0.900 [0.873–0.925] | 0.843 [0.805–0.884] | 0.103 [0.101–0.106] |
| page (answered only) | 318 | 0.572 [0.537–0.605] | 0.625 [0.584–0.665] | 0.705 [0.664–0.750] | 0.585 [0.531–0.642] | 0.225 [0.207–0.243] |

Latency over 318 answered queries (ms): mean 2591.3, p50 1634.9, p95 7260.0, p99 10809.6

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 1627.8, p95 7257.1, p99 10807.6. Client-measured: p50 1634.9, p95 7260.0, p99 10809.6 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 40.7 | 34.2 | 65.1 | 176.8 |
| context | 318 | 1.0 | 0.2 | 3.0 | 17.4 |
| detail.engineTotal | 318 | 2582.3 | 1616.7 | 7255.7 | 10806.3 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 40.7 | 34.2 | 65.1 | 176.8 |
| detail.queryEmbed | 318 | 2480.1 | 1510.0 | 7158.0 | 10312.4 |
| detail.scopeCheck | 318 | 1.8 | 1.2 | 4.2 | 6.8 |
| detail.vectorArm | 318 | 40.7 | 34.2 | 65.1 | 176.8 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 2.2 | 1.6 | 4.9 | 9.8 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 55.0 | 27.5 | 99.5 | 448.5 |
| rerank | 318 | 0.1 | 0.0 | 0.3 | 1.8 |
| scopeBuild | 318 | 2.2 | 1.0 | 3.7 | 14.7 |
| serialise | 318 | 2.3 | 0.2 | 5.8 | 15.8 |
| total | 318 | 2587.2 | 1627.8 | 7257.1 | 10807.6 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
