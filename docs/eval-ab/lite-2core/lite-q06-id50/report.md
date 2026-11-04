# Retrieval evaluation — lite-q06-id50

- dataset: `miracl-id-50`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T16:49:37.344840694+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 1485
- queries: 50 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 50 | 0.811 [0.753–0.867] | 0.945 [0.889–0.986] | 0.837 [0.759–0.910] | 0.740 [0.620–0.860] | 0.284 [0.230–0.342] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 50 | 0.811 [0.753–0.867] | 0.945 [0.889–0.986] | 0.837 [0.759–0.910] | 0.740 [0.620–0.860] | 0.284 [0.230–0.342] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 50 answered queries (ms): mean 812.1, p50 802.2, p95 925.9, p99 942.8

### Server-side stage timings (ms)

Server-side total over 50 responses: p50 800.8, p95 922.0, p99 941.4. Client-measured: p50 802.2, p95 925.9, p99 942.8 over 50 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 50 | 20.8 | 19.6 | 25.1 | 56.4 |
| context | 50 | 0.1 | 0.1 | 0.1 | 0.3 |
| detail.engineTotal | 50 | 806.4 | 796.5 | 916.2 | 937.7 |
| detail.entityArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 50 | 20.8 | 19.6 | 25.1 | 56.4 |
| detail.pageArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 50 | 779.3 | 772.4 | 893.3 | 911.0 |
| detail.scopeCheck | 50 | 2.1 | 1.6 | 3.6 | 7.6 |
| detail.vectorArm | 50 | 20.8 | 19.6 | 25.1 | 56.4 |
| detail.visionRerank | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 50 | 0.8 | 0.8 | 1.0 | 1.3 |
| fusion | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 50 | 3.2 | 2.7 | 4.9 | 11.2 |
| pages | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 50 | 0.0 | 0.0 | 0.0 | 0.5 |
| scopeBuild | 50 | 4.0 | 3.7 | 5.6 | 7.9 |
| serialise | 50 | 0.1 | 0.1 | 0.1 | 0.1 |
| total | 50 | 810.5 | 800.8 | 922.0 | 941.4 |

Query embeddings: 0 cache hit(s), 50 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
