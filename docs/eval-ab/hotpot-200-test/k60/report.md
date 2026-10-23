# Retrieval evaluation — k60

- dataset: `hotpot-200-test`
- harness commit: `4c01a7d-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T05:18:48.029839140+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 995
- queries: 100 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 100 | 0.820 [0.780–0.857] | 0.905 [0.865–0.940] | 0.908 [0.867–0.952] | 0.850 [0.780–0.920] | 0.181 [0.173–0.188] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 100 | 0.820 [0.780–0.857] | 0.905 [0.865–0.940] | 0.908 [0.867–0.952] | 0.850 [0.780–0.920] | 0.181 [0.173–0.188] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 100 answered queries (ms): mean 103.8, p50 110.8, p95 121.4, p99 123.4

### Server-side stage timings (ms)

Server-side total over 100 responses: p50 109.6, p95 120.1, p99 122.1. Client-measured: p50 110.8, p95 121.4, p99 123.4 over 100 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 100 | 14.7 | 13.6 | 20.7 | 22.1 |
| context | 100 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 100 | 99.4 | 106.6 | 116.5 | 118.6 |
| detail.entityArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 100 | 14.7 | 13.6 | 20.7 | 22.1 |
| detail.queryEmbed | 100 | 79.9 | 89.4 | 92.8 | 94.8 |
| detail.scopeCheck | 100 | 1.6 | 1.5 | 3.0 | 3.8 |
| detail.vectorArm | 100 | 14.7 | 13.6 | 20.7 | 22.1 |
| enrich | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 100 | 0.8 | 0.7 | 0.9 | 1.2 |
| fusion | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 100 | 2.3 | 2.2 | 3.9 | 4.9 |
| rerank | 100 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 100 | 3.1 | 2.9 | 4.5 | 5.8 |
| serialise | 100 | 0.0 | 0.0 | 0.1 | 0.1 |
| total | 100 | 102.6 | 109.6 | 120.1 | 122.1 |

Query embeddings: 0 cache hit(s), 100 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
