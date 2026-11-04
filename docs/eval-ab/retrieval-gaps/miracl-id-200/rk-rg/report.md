# Retrieval evaluation — rk-rg

- dataset: `miracl-id-200`
- harness commit: `f76734f8-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-04T14:10:30.074372525+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.845 [0.817–0.873] | 0.957 [0.938–0.973] | 0.861 [0.821–0.899] | 0.785 [0.725–0.840] | 0.300 [0.271–0.329] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.845 [0.817–0.873] | 0.957 [0.938–0.973] | 0.861 [0.821–0.899] | 0.785 [0.725–0.840] | 0.300 [0.271–0.329] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 96.8, p50 52.0, p95 587.7, p99 739.6

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 50.4, p95 586.0, p99 738.1. Client-measured: p50 52.0, p95 587.7, p99 739.6 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 25.5 | 25.0 | 37.0 | 45.3 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.2 |
| detail.engineTotal | 200 | 86.7 | 42.2 | 576.9 | 731.5 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 25.5 | 25.0 | 37.0 | 45.3 |
| detail.pageArm | 200 | 15.9 | 14.9 | 28.4 | 32.2 |
| detail.queryEmbed | 200 | 2.7 | 1.4 | 7.8 | 9.2 |
| detail.scopeCheck | 200 | 1.9 | 1.5 | 4.4 | 6.0 |
| detail.vectorArm | 200 | 25.5 | 25.0 | 37.0 | 45.3 |
| detail.visionRerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.3 | 1.0 | 3.2 | 4.3 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 5.5 | 5.0 | 9.2 | 11.5 |
| pages | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 200 | 49.4 | 4.4 | 546.4 | 703.3 |
| scopeBuild | 200 | 8.0 | 7.4 | 11.8 | 13.6 |
| serialise | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| total | 200 | 94.9 | 50.4 | 586.0 | 738.1 |

Query embeddings: 200 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
