# Retrieval evaluation — e8b-bge

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T19:16:04.936729373+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.924 [0.903–0.945] | 0.979 [0.962–0.992] | 0.911 [0.885–0.934] | 0.855 [0.814–0.890] | 0.103 [0.101–0.106] |
| page | 318 | 0.606 [0.570–0.639] | 0.635 [0.594–0.673] | 0.744 [0.704–0.784] | 0.645 [0.591–0.695] | 0.231 [0.214–0.250] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.924 [0.903–0.945] | 0.979 [0.962–0.992] | 0.911 [0.885–0.934] | 0.855 [0.814–0.890] | 0.103 [0.101–0.106] |
| page (answered only) | 318 | 0.606 [0.570–0.639] | 0.635 [0.594–0.673] | 0.744 [0.704–0.784] | 0.645 [0.591–0.695] | 0.231 [0.214–0.250] |

Latency over 318 answered queries (ms): mean 3502.8, p50 2731.7, p95 6168.8, p99 6999.6

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2722.9, p95 6094.9, p99 6869.1. Client-measured: p50 2731.7, p95 6168.8, p99 6999.6 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 55.0 | 41.6 | 125.9 | 303.6 |
| context | 318 | 6.7 | 1.3 | 18.3 | 41.1 |
| detail.engineTotal | 318 | 3467.9 | 2718.5 | 6063.0 | 6811.5 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 55.0 | 41.6 | 125.9 | 303.6 |
| detail.queryEmbed | 318 | 2.8 | 0.9 | 9.8 | 24.8 |
| detail.scopeCheck | 318 | 3.1 | 1.7 | 8.0 | 14.6 |
| detail.vectorArm | 318 | 55.0 | 41.6 | 125.9 | 303.6 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.4 |
| figures | 318 | 5.9 | 2.5 | 14.9 | 51.4 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 157.6 | 58.0 | 596.0 | 1225.5 |
| rerank | 318 | 3234.9 | 2496.3 | 5513.2 | 6152.1 |
| scopeBuild | 318 | 4.7 | 1.7 | 17.4 | 52.7 |
| serialise | 318 | 5.3 | 1.4 | 17.5 | 63.3 |
| total | 318 | 3480.1 | 2722.9 | 6094.9 | 6869.1 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
