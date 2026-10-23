# Retrieval evaluation — fixed20-nn0-bge20

- dataset: `vidore3-hr-en`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T10:31:11.518972602+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.919 [0.897–0.941] | 0.972 [0.953–0.988] | 0.905 [0.878–0.931] | 0.852 [0.811–0.890] | 0.103 [0.100–0.105] |
| page | 318 | 0.559 [0.521–0.595] | 0.568 [0.526–0.608] | 0.716 [0.674–0.758] | 0.623 [0.566–0.676] | 0.205 [0.189–0.223] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.919 [0.897–0.941] | 0.972 [0.953–0.988] | 0.905 [0.878–0.931] | 0.852 [0.811–0.890] | 0.103 [0.100–0.105] |
| page (answered only) | 318 | 0.559 [0.521–0.595] | 0.568 [0.526–0.608] | 0.716 [0.674–0.758] | 0.623 [0.566–0.676] | 0.205 [0.189–0.223] |

Latency over 318 answered queries (ms): mean 2337.9, p50 2092.0, p95 4515.5, p99 7056.1

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2089.7, p95 4496.0, p99 7052.9. Client-measured: p50 2092.0, p95 4515.5, p99 7056.1 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 12.1 | 10.5 | 16.8 | 28.6 |
| context | 318 | 0.3 | 0.1 | 0.4 | 4.6 |
| detail.engineTotal | 318 | 2332.6 | 2088.7 | 4495.0 | 7051.7 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 12.1 | 10.5 | 16.8 | 28.6 |
| detail.queryEmbed | 318 | 82.7 | 91.8 | 95.9 | 103.0 |
| detail.scopeCheck | 318 | 1.1 | 1.0 | 1.5 | 2.4 |
| detail.vectorArm | 318 | 12.1 | 10.5 | 16.8 | 28.6 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.9 | 1.5 | 2.9 | 13.8 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 2234.2 | 1982.8 | 4387.4 | 6951.6 |
| scopeBuild | 318 | 1.0 | 0.8 | 1.6 | 5.0 |
| serialise | 318 | 0.2 | 0.1 | 0.8 | 1.5 |
| total | 318 | 2333.8 | 2089.7 | 4496.0 | 7052.9 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
