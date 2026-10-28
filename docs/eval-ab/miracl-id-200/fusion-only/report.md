# Retrieval evaluation — fusion-only

- dataset: `miracl-id-200`
- harness commit: `f0265bd-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T03:59:44.920455601+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.779 [0.748–0.810] | 0.945 [0.925–0.964] | 0.791 [0.747–0.833] | 0.685 [0.620–0.745] | 0.294 [0.267–0.321] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.779 [0.748–0.810] | 0.945 [0.925–0.964] | 0.791 [0.747–0.833] | 0.685 [0.620–0.745] | 0.294 [0.267–0.321] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 103.0, p50 98.5, p95 130.8, p99 144.6

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 97.1, p95 129.4, p99 143.1. Client-measured: p50 98.5, p95 130.8, p99 144.6 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 32.4 | 29.5 | 43.2 | 49.5 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 200 | 94.7 | 90.4 | 122.8 | 134.0 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 32.4 | 29.5 | 43.2 | 49.5 |
| detail.queryEmbed | 200 | 56.6 | 54.4 | 81.3 | 94.8 |
| detail.scopeCheck | 200 | 1.4 | 1.3 | 1.9 | 3.8 |
| detail.vectorArm | 200 | 32.4 | 29.5 | 43.2 | 49.5 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 0.8 | 0.8 | 1.0 | 1.2 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 3.2 | 3.0 | 4.4 | 9.4 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 200 | 6.7 | 6.4 | 8.5 | 11.8 |
| serialise | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| total | 200 | 101.5 | 97.1 | 129.4 | 143.1 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
