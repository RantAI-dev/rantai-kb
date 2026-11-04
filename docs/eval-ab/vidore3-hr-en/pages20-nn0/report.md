# Retrieval evaluation — pages20-nn0

- dataset: `vidore3-hr-en`
- harness commit: `64498f7-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T08:53:37.779608140+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.878 [0.853–0.903] | 0.972 [0.953–0.988] | 0.851 [0.820–0.882] | 0.764 [0.717–0.811] | 0.103 [0.100–0.105] |
| page | 318 | 0.402 [0.363–0.440] | 0.420 [0.376–0.462] | 0.549 [0.501–0.599] | 0.450 [0.396–0.506] | 0.154 [0.138–0.171] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.878 [0.853–0.903] | 0.972 [0.953–0.988] | 0.851 [0.820–0.882] | 0.764 [0.717–0.811] | 0.103 [0.100–0.105] |
| page (answered only) | 318 | 0.402 [0.363–0.440] | 0.420 [0.376–0.462] | 0.549 [0.501–0.599] | 0.450 [0.396–0.506] | 0.154 [0.138–0.171] |

Latency over 318 answered queries (ms): mean 110.3, p50 116.1, p95 137.2, p99 148.7

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 114.3, p95 134.9, p99 145.3. Client-measured: p50 116.1, p95 137.2, p99 148.7 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 22.2 | 20.6 | 35.5 | 44.9 |
| context | 318 | 0.1 | 0.1 | 0.1 | 0.3 |
| detail.engineTotal | 318 | 106.6 | 112.7 | 132.6 | 143.9 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 22.2 | 20.6 | 35.5 | 44.9 |
| detail.queryEmbed | 318 | 80.7 | 90.5 | 94.0 | 96.5 |
| detail.scopeCheck | 318 | 1.5 | 1.2 | 3.2 | 4.0 |
| detail.vectorArm | 318 | 22.2 | 20.6 | 35.5 | 44.9 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.9 | 1.6 | 3.9 | 5.8 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 318 | 1.4 | 1.2 | 2.8 | 4.0 |
| serialise | 318 | 0.1 | 0.1 | 0.2 | 0.6 |
| total | 318 | 108.1 | 114.3 | 134.9 | 145.3 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
