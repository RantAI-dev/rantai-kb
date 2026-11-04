# Retrieval evaluation — rk-e5

- dataset: `miracl-id-200`
- harness commit: `cc553ac-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-21T15:57:23.468011587+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.796 [0.764–0.825] | 0.961 [0.941–0.979] | 0.791 [0.747–0.833] | 0.670 [0.605–0.730] | 0.302 [0.274–0.331] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.796 [0.764–0.825] | 0.961 [0.941–0.979] | 0.791 [0.747–0.833] | 0.670 [0.605–0.730] | 0.302 [0.274–0.331] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 44.4, p50 42.7, p95 53.9, p99 63.4

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 41.1, p95 52.2, p99 61.7. Client-measured: p50 42.7, p95 53.9, p99 63.4 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 30.2 | 28.8 | 37.9 | 45.5 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 200 | 36.0 | 34.4 | 45.0 | 52.4 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 30.2 | 28.8 | 37.9 | 45.5 |
| detail.queryEmbed | 200 | 0.0 | 0.0 | 0.1 | 0.1 |
| detail.scopeCheck | 200 | 1.3 | 1.3 | 1.5 | 1.9 |
| detail.vectorArm | 200 | 30.2 | 28.8 | 37.9 | 45.5 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 0.9 | 0.9 | 1.1 | 1.3 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 3.4 | 3.2 | 5.0 | 7.4 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 200 | 6.7 | 6.5 | 8.1 | 9.3 |
| serialise | 200 | 0.1 | 0.1 | 0.1 | 0.2 |
| total | 200 | 42.8 | 41.1 | 52.2 | 61.7 |

Query embeddings: 200 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
