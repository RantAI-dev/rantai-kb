# Retrieval evaluation — base-repeat

- dataset: `vidore3-hr-en`
- harness commit: `2e64bc9-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T17:29:01.383435731+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.917 [0.895–0.938] | 0.982 [0.968–0.994] | 0.900 [0.873–0.926] | 0.843 [0.802–0.884] | 0.104 [0.101–0.107] |
| page | 318 | 0.572 [0.536–0.605] | 0.625 [0.584–0.665] | 0.705 [0.662–0.748] | 0.585 [0.531–0.642] | 0.225 [0.207–0.243] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.917 [0.895–0.938] | 0.982 [0.968–0.994] | 0.900 [0.873–0.926] | 0.843 [0.802–0.884] | 0.104 [0.101–0.107] |
| page (answered only) | 318 | 0.572 [0.536–0.605] | 0.625 [0.584–0.665] | 0.705 [0.662–0.748] | 0.585 [0.531–0.642] | 0.225 [0.207–0.243] |

Latency over 318 answered queries (ms): mean 54.7, p50 52.0, p95 77.9, p99 102.1

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 50.2, p95 75.7, p99 99.4. Client-measured: p50 52.0, p95 77.9, p99 102.1 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 30.4 | 29.8 | 38.2 | 45.3 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 318 | 51.4 | 48.8 | 73.6 | 97.6 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 30.4 | 29.8 | 38.2 | 45.3 |
| detail.queryEmbed | 318 | 0.7 | 0.6 | 1.1 | 1.9 |
| detail.scopeCheck | 318 | 1.3 | 1.1 | 3.2 | 5.5 |
| detail.vectorArm | 318 | 30.4 | 29.8 | 38.2 | 45.3 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.4 | 1.2 | 3.3 | 5.1 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 17.4 | 15.7 | 34.1 | 50.1 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 1.1 | 0.9 | 2.3 | 3.7 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.3 |
| total | 318 | 52.7 | 50.2 | 75.7 | 99.4 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
