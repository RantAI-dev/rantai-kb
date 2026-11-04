# Retrieval evaluation — k60-bge10

- dataset: `miracl-id-200`
- harness commit: `4c01a7d-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T04:59:00.391098051+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.824 [0.797–0.853] | 0.927 [0.903–0.949] | 0.858 [0.821–0.894] | 0.775 [0.720–0.830] | 0.285 [0.259–0.310] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.824 [0.797–0.853] | 0.927 [0.903–0.949] | 0.858 [0.821–0.894] | 0.775 [0.720–0.830] | 0.285 [0.259–0.310] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 812.7, p50 811.7, p95 1016.2, p99 1113.6

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 810.3, p95 1014.8, p99 1112.2. Client-measured: p50 811.7, p95 1016.2, p99 1113.6 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 33.0 | 30.9 | 46.0 | 50.7 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 200 | 804.2 | 803.7 | 1008.2 | 1105.5 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 33.0 | 30.9 | 46.0 | 50.7 |
| detail.queryEmbed | 200 | 54.9 | 52.9 | 78.1 | 89.9 |
| detail.scopeCheck | 200 | 1.4 | 1.3 | 1.7 | 3.5 |
| detail.vectorArm | 200 | 33.0 | 30.9 | 46.0 | 50.7 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 0.8 | 0.8 | 1.0 | 1.2 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 2.3 | 2.2 | 3.3 | 4.1 |
| rerank | 200 | 711.5 | 709.7 | 919.2 | 992.6 |
| scopeBuild | 200 | 6.9 | 6.6 | 8.9 | 9.5 |
| serialise | 200 | 0.1 | 0.0 | 0.1 | 0.1 |
| total | 200 | 811.2 | 810.3 | 1014.8 | 1112.2 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
