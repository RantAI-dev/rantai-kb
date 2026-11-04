# Retrieval evaluation — lite-minilm-hr-c2

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-25T05:08:23.092612697+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.893 [0.869–0.918] | 0.970 [0.953–0.986] | 0.871 [0.843–0.900] | 0.799 [0.758–0.843] | 0.102 [0.099–0.105] |
| page | 318 | 0.536 [0.501–0.571] | 0.593 [0.550–0.634] | 0.656 [0.614–0.698] | 0.528 [0.475–0.582] | 0.220 [0.203–0.237] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.893 [0.869–0.918] | 0.970 [0.953–0.986] | 0.871 [0.843–0.900] | 0.799 [0.758–0.843] | 0.102 [0.099–0.105] |
| page (answered only) | 318 | 0.536 [0.501–0.571] | 0.593 [0.550–0.634] | 0.656 [0.614–0.698] | 0.528 [0.475–0.582] | 0.220 [0.203–0.237] |

Latency over 318 answered queries (ms): mean 40.0, p50 38.8, p95 52.9, p99 60.0

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 37.1, p95 51.2, p99 58.4. Client-measured: p50 38.8, p95 52.9, p99 60.0 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 8.3 | 7.6 | 12.0 | 15.1 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 318 | 37.2 | 36.0 | 50.1 | 56.5 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 8.3 | 7.6 | 12.0 | 15.1 |
| detail.pageArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 318 | 12.0 | 11.2 | 17.2 | 27.9 |
| detail.scopeCheck | 318 | 1.1 | 1.0 | 2.2 | 3.8 |
| detail.vectorArm | 318 | 8.3 | 7.6 | 12.0 | 15.1 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.0 | 1.0 | 1.3 | 1.8 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 14.5 | 14.3 | 26.5 | 30.4 |
| pages | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 0.9 | 0.8 | 1.4 | 2.1 |
| serialise | 318 | 0.2 | 0.2 | 0.2 | 0.3 |
| total | 318 | 38.3 | 37.1 | 51.2 | 58.4 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
