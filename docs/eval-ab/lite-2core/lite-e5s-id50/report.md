# Retrieval evaluation — lite-e5s-id50

- dataset: `miracl-id-50`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T15:16:29.690071326+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 1485
- queries: 50 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 50 | 0.802 [0.739–0.858] | 0.938 [0.886–0.985] | 0.823 [0.740–0.898] | 0.720 [0.600–0.840] | 0.286 [0.228–0.346] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 50 | 0.802 [0.739–0.858] | 0.938 [0.886–0.985] | 0.823 [0.740–0.898] | 0.720 [0.600–0.840] | 0.286 [0.228–0.346] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 50 answered queries (ms): mean 43.4, p50 42.0, p95 49.1, p99 63.8

### Server-side stage timings (ms)

Server-side total over 50 responses: p50 40.5, p95 47.0, p99 59.6. Client-measured: p50 42.0, p95 49.1, p99 63.8 over 50 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 50 | 16.3 | 15.4 | 20.9 | 27.8 |
| context | 50 | 0.1 | 0.1 | 0.1 | 0.2 |
| detail.engineTotal | 50 | 37.9 | 36.7 | 43.0 | 54.5 |
| detail.entityArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 50 | 16.3 | 15.4 | 20.9 | 27.8 |
| detail.pageArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 50 | 15.8 | 15.8 | 18.3 | 19.9 |
| detail.scopeCheck | 50 | 1.9 | 1.6 | 3.2 | 3.9 |
| detail.vectorArm | 50 | 16.3 | 15.4 | 20.9 | 27.8 |
| detail.visionRerank | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 50 | 0.8 | 0.8 | 0.9 | 1.1 |
| fusion | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 50 | 2.9 | 2.7 | 3.3 | 6.0 |
| pages | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 50 | 0.0 | 0.0 | 0.0 | 0.7 |
| scopeBuild | 50 | 3.8 | 3.7 | 4.4 | 5.2 |
| serialise | 50 | 0.1 | 0.1 | 0.1 | 0.3 |
| total | 50 | 41.8 | 40.5 | 47.0 | 59.6 |

Query embeddings: 0 cache hit(s), 50 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
