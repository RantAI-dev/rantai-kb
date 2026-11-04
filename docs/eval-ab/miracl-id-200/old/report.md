# Retrieval evaluation — old

- dataset: `miracl-id-200`
- harness commit: `f0265bd-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T03:31:25.655388405+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.800 [0.771–0.832] | 0.942 [0.920–0.963] | 0.822 [0.781–0.861] | 0.720 [0.660–0.780] | 0.293 [0.266–0.319] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.800 [0.771–0.832] | 0.942 [0.920–0.963] | 0.822 [0.781–0.861] | 0.720 [0.660–0.780] | 0.293 [0.266–0.319] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 115.4, p50 112.9, p95 152.6, p99 179.1

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 111.5, p95 147.0, p99 174.6. Client-measured: p50 112.9, p95 152.6, p99 179.1 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 40.9 | 40.0 | 58.7 | 72.1 |
| context | 200 | 0.1 | 0.1 | 0.2 | 0.6 |
| detail.engineTotal | 200 | 106.3 | 104.3 | 137.5 | 166.5 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 40.9 | 40.0 | 58.7 | 72.1 |
| detail.queryEmbed | 200 | 58.1 | 54.5 | 90.7 | 95.0 |
| detail.scopeCheck | 200 | 1.6 | 1.3 | 3.1 | 4.9 |
| detail.vectorArm | 200 | 40.9 | 40.0 | 58.7 | 72.1 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.0 | 0.8 | 1.4 | 3.2 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 4.4 | 3.9 | 7.1 | 10.0 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.3 |
| scopeBuild | 200 | 7.1 | 6.4 | 10.7 | 15.8 |
| serialise | 200 | 0.1 | 0.1 | 0.2 | 0.8 |
| total | 200 | 113.5 | 111.5 | 147.0 | 174.6 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
