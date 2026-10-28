# Retrieval evaluation — k60-bge20

- dataset: `miracl-id-200`
- harness commit: `4c01a7d-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T05:03:49.907045996+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.830 [0.804–0.857] | 0.939 [0.918–0.958] | 0.857 [0.820–0.892] | 0.770 [0.710–0.825] | 0.290 [0.263–0.316] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.830 [0.804–0.857] | 0.939 [0.918–0.958] | 0.857 [0.820–0.892] | 0.770 [0.710–0.825] | 0.290 [0.263–0.316] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 1439.1, p50 1446.1, p95 1751.6, p99 1895.0

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 1444.4, p95 1750.1, p99 1893.2. Client-measured: p50 1446.1, p95 1751.6, p99 1895.0 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 32.0 | 30.8 | 39.8 | 46.5 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 200 | 1430.7 | 1437.5 | 1743.3 | 1884.1 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 32.0 | 30.8 | 39.8 | 46.5 |
| detail.queryEmbed | 200 | 55.2 | 52.9 | 78.3 | 90.2 |
| detail.scopeCheck | 200 | 1.4 | 1.3 | 1.7 | 3.1 |
| detail.vectorArm | 200 | 32.0 | 30.8 | 39.8 | 46.5 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 0.8 | 0.8 | 1.0 | 1.1 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 2.3 | 2.2 | 3.1 | 3.6 |
| rerank | 200 | 1338.8 | 1345.6 | 1649.4 | 1793.2 |
| scopeBuild | 200 | 6.8 | 6.6 | 8.2 | 9.8 |
| serialise | 200 | 0.1 | 0.0 | 0.1 | 0.1 |
| total | 200 | 1437.6 | 1444.4 | 1750.1 | 1893.2 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
