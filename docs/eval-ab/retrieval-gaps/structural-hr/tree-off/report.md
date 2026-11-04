# Retrieval evaluation — tree-off

- dataset: `structural-hr`
- harness commit: `9b9cf7fe-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-04T14:34:20.625727244+00:00
- cutoff k: 10
- max chunks per query: 100
- documents known: 14
- queries: 41 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 41 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.100 [0.100–0.100] |
| page | 41 | 0.597 [0.497–0.706] | 0.591 [0.469–0.714] | 0.710 [0.600–0.821] | 0.585 [0.439–0.732] | 0.137 [0.110–0.168] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 41 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.100 [0.100–0.100] |
| page (answered only) | 41 | 0.597 [0.497–0.706] | 0.591 [0.469–0.714] | 0.710 [0.600–0.821] | 0.585 [0.439–0.732] | 0.137 [0.110–0.168] |

Latency over 41 answered queries (ms): mean 2192.5, p50 2140.6, p95 2439.7, p99 2853.7

### Server-side stage timings (ms)

Server-side total over 41 responses: p50 2138.0, p95 2437.4, p99 2851.4. Client-measured: p50 2140.6, p95 2439.7, p99 2853.7 over 41 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 41 | 7.2 | 6.9 | 9.0 | 13.2 |
| context | 41 | 0.3 | 0.3 | 0.4 | 0.5 |
| detail.engineTotal | 41 | 2188.7 | 2136.4 | 2436.0 | 2849.8 |
| detail.entityArm | 41 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 41 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 41 | 7.2 | 6.9 | 9.0 | 13.2 |
| detail.pageArm | 41 | 795.0 | 788.5 | 827.7 | 954.9 |
| detail.queryEmbed | 41 | 780.6 | 762.7 | 806.9 | 1168.1 |
| detail.scopeCheck | 41 | 1.1 | 1.0 | 1.2 | 2.6 |
| detail.vectorArm | 41 | 7.2 | 6.9 | 9.0 | 13.2 |
| detail.visionRerank | 41 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 41 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 41 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 41 | 1.3 | 1.2 | 1.7 | 1.8 |
| fusion | 41 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 41 | 22.7 | 18.9 | 39.6 | 46.6 |
| pages | 41 | 831.2 | 790.4 | 1004.3 | 1397.2 |
| rerank | 41 | 544.2 | 538.5 | 609.0 | 625.4 |
| scopeBuild | 41 | 0.8 | 0.8 | 1.0 | 1.0 |
| serialise | 41 | 0.5 | 0.5 | 0.7 | 0.7 |
| total | 41 | 2190.2 | 2138.0 | 2437.4 | 2851.4 |

Query embeddings: 0 cache hit(s), 41 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
