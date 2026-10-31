# Retrieval evaluation — rk-cloud

- dataset: `miracl-id-200`
- harness commit: `ead4e48-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-31T01:52:46.861107743+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 2933
- queries: 200 (failed: 0, degraded: 1, noted: 0)
- degradations reported by the server: rerank_unavailable

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

Latency over 200 answered queries (ms): mean 780.8, p50 575.0, p95 1365.2, p99 1901.7

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 573.5, p95 1363.2, p99 1899.8. Client-measured: p50 575.0, p95 1365.2, p99 1901.7 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 17.8 | 17.1 | 23.0 | 30.5 |
| context | 200 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 200 | 771.7 | 566.2 | 1355.2 | 1892.7 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 17.8 | 17.1 | 23.0 | 30.5 |
| detail.pageArm | 200 | 178.4 | 7.0 | 789.3 | 801.1 |
| detail.queryEmbed | 200 | 159.6 | 0.8 | 775.9 | 792.9 |
| detail.scopeCheck | 200 | 1.6 | 1.5 | 2.5 | 3.2 |
| detail.vectorArm | 200 | 17.8 | 17.1 | 23.0 | 30.5 |
| detail.visionRerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.0 | 1.0 | 1.2 | 1.3 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 3.4 | 3.2 | 5.0 | 6.9 |
| pages | 200 | 4.9 | 0.0 | 0.0 | 16.9 |
| rerank | 200 | 583.1 | 535.0 | 678.8 | 1101.7 |
| scopeBuild | 200 | 7.2 | 6.9 | 9.4 | 10.2 |
| serialise | 200 | 0.1 | 0.1 | 0.1 | 0.2 |
| total | 200 | 779.1 | 573.5 | 1363.2 | 1899.8 |

Query embeddings: 159 cache hit(s), 41 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
