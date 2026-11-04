# Retrieval evaluation — voy-qp-text

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T22:43:02.423234305+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.933 [0.913–0.953] | 0.982 [0.967–0.994] | 0.919 [0.895–0.943] | 0.871 [0.836–0.906] | 0.104 [0.101–0.106] |
| page | 318 | 0.657 [0.624–0.691] | 0.667 [0.629–0.703] | 0.794 [0.758–0.831] | 0.704 [0.654–0.758] | 0.244 [0.226–0.263] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.933 [0.913–0.953] | 0.982 [0.967–0.994] | 0.919 [0.895–0.943] | 0.871 [0.836–0.906] | 0.104 [0.101–0.106] |
| page (answered only) | 318 | 0.657 [0.624–0.691] | 0.667 [0.629–0.703] | 0.794 [0.758–0.831] | 0.704 [0.654–0.758] | 0.244 [0.226–0.263] |

Latency over 318 answered queries (ms): mean 2089.3, p50 1736.5, p95 3975.7, p99 6727.8

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 1733.4, p95 3971.3, p99 6726.0. Client-measured: p50 1736.5, p95 3975.7, p99 6727.8 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 131.9 | 45.9 | 577.3 | 1628.3 |
| context | 318 | 0.2 | 0.1 | 0.3 | 0.9 |
| detail.engineTotal | 318 | 2084.6 | 1732.0 | 3969.5 | 6724.5 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 131.9 | 45.9 | 577.3 | 1628.3 |
| detail.queryEmbed | 318 | 1389.7 | 1089.2 | 3076.5 | 6151.3 |
| detail.scopeCheck | 318 | 1.6 | 1.2 | 3.4 | 5.2 |
| detail.vectorArm | 318 | 131.9 | 45.9 | 577.3 | 1628.3 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 2.1 | 1.6 | 4.8 | 7.6 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 31.0 | 26.1 | 70.5 | 87.0 |
| rerank | 318 | 527.9 | 522.2 | 561.3 | 648.4 |
| scopeBuild | 318 | 1.6 | 1.3 | 3.3 | 6.5 |
| serialise | 318 | 0.2 | 0.2 | 0.4 | 0.9 |
| total | 318 | 2086.5 | 1733.4 | 3971.3 | 6726.0 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
