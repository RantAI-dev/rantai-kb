# Retrieval evaluation — md

- dataset: `vidore3-hr-en-md`
- harness commit: `b0394eb`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T06:53:10.743325366+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 1110
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.479 [0.443–0.514] | 0.515 [0.471–0.559] | 0.623 [0.577–0.667] | 0.500 [0.443–0.557] | 0.187 [0.172–0.205] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.479 [0.443–0.514] | 0.515 [0.471–0.559] | 0.623 [0.577–0.667] | 0.500 [0.443–0.557] | 0.187 [0.172–0.205] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 318 answered queries (ms): mean 128.4, p50 134.7, p95 155.1, p99 169.3

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 132.6, p95 153.0, p99 167.0. Client-measured: p50 134.7, p95 155.1, p99 169.3 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 31.1 | 31.1 | 43.2 | 48.9 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.3 |
| detail.engineTotal | 318 | 122.0 | 128.4 | 148.2 | 161.4 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 31.1 | 31.1 | 43.2 | 48.9 |
| detail.queryEmbed | 318 | 80.8 | 90.5 | 94.3 | 96.0 |
| detail.scopeCheck | 318 | 1.7 | 1.3 | 3.7 | 5.8 |
| detail.vectorArm | 318 | 31.1 | 31.1 | 43.2 | 48.9 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.2 | 1.0 | 2.3 | 3.0 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 7.0 | 6.9 | 11.0 | 14.4 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 4.0 | 3.7 | 5.9 | 7.6 |
| serialise | 318 | 0.2 | 0.2 | 0.2 | 0.3 |
| total | 318 | 126.3 | 132.6 | 153.0 | 167.0 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
