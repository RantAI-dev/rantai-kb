# Retrieval evaluation — miracl-rerank-off

- dataset: `miracl-id-50`
- harness commit: `a6664c5-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-17T23:10:31.670918037+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 564
- queries: 50 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 50 | 0.904 [0.861–0.941] | 0.979 [0.957–0.996] | 0.905 [0.850–0.960] | 0.820 [0.720–0.920] | 0.296 [0.238–0.356] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 50 | 0.904 [0.861–0.941] | 0.979 [0.957–0.996] | 0.905 [0.850–0.960] | 0.820 [0.720–0.920] | 0.296 [0.238–0.356] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 50 answered queries (ms): mean 321.7, p50 313.5, p95 401.4, p99 492.1

### Server-side stage timings (ms)

Server-side total over 50 responses: p50 310.1, p95 397.7, p99 488.4. Client-measured: p50 313.5, p95 401.4, p99 492.1 over 50 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 50 | 19.0 | 19.0 | 23.4 | 25.9 |
| context | 50 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 50 | 310.5 | 302.7 | 391.0 | 479.6 |
| detail.entityArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 50 | 19.0 | 19.0 | 23.4 | 25.9 |
| detail.queryEmbed | 50 | 277.5 | 264.0 | 358.4 | 441.6 |
| detail.scopeCheck | 50 | 2.6 | 2.3 | 3.7 | 6.7 |
| detail.vectorArm | 50 | 19.0 | 19.0 | 23.4 | 25.9 |
| enrich | 50 | 0.0 | 0.0 | 0.0 | 0.1 |
| expand | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 50 | 1.7 | 1.6 | 2.2 | 2.4 |
| fusion | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 50 | 8.8 | 8.6 | 12.2 | 13.6 |
| rerank | 50 | 0.1 | 0.1 | 0.1 | 0.1 |
| scopeBuild | 50 | 5.6 | 5.4 | 7.3 | 9.4 |
| serialise | 50 | 1.7 | 1.7 | 2.5 | 3.3 |
| total | 50 | 317.9 | 310.1 | 397.7 | 488.4 |

Query embeddings: 0 cache hit(s), 50 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
