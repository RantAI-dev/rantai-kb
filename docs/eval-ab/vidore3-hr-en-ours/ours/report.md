# Retrieval evaluation — ours

- dataset: `vidore3-hr-en-ours`
- harness commit: `b0394eb`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T08:02:59.757269262+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 1090
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.477 [0.440–0.512] | 0.532 [0.487–0.573] | 0.614 [0.568–0.657] | 0.481 [0.428–0.535] | 0.189 [0.174–0.206] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.477 [0.440–0.512] | 0.532 [0.487–0.573] | 0.614 [0.568–0.657] | 0.481 [0.428–0.535] | 0.189 [0.174–0.206] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 318 answered queries (ms): mean 125.3, p50 125.3, p95 176.1, p99 211.1

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 123.2, p95 169.9, p99 200.3. Client-measured: p50 125.3, p95 176.1, p99 211.1 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 26.3 | 21.9 | 49.4 | 65.4 |
| context | 318 | 0.2 | 0.1 | 0.2 | 0.5 |
| detail.engineTotal | 318 | 118.0 | 119.5 | 162.6 | 194.0 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 26.3 | 21.9 | 49.4 | 65.4 |
| detail.queryEmbed | 318 | 82.3 | 91.0 | 97.3 | 110.1 |
| detail.scopeCheck | 318 | 2.0 | 1.4 | 4.2 | 11.1 |
| detail.vectorArm | 318 | 26.3 | 21.9 | 49.4 | 65.4 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.2 | 0.9 | 2.3 | 3.9 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 5.9 | 4.9 | 11.8 | 20.3 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 318 | 3.7 | 3.2 | 5.7 | 8.6 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 1.4 |
| total | 318 | 122.0 | 123.2 | 169.9 | 200.3 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
