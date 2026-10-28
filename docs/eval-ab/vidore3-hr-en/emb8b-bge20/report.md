# Retrieval evaluation — emb8b-bge20

- dataset: `vidore3-hr-en`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T12:04:07.420637556+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.921 [0.899–0.943] | 0.977 [0.961–0.992] | 0.905 [0.879–0.931] | 0.852 [0.814–0.890] | 0.103 [0.101–0.106] |
| page | 318 | 0.593 [0.558–0.626] | 0.611 [0.573–0.649] | 0.745 [0.705–0.786] | 0.651 [0.601–0.701] | 0.221 [0.204–0.238] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.921 [0.899–0.943] | 0.977 [0.961–0.992] | 0.905 [0.879–0.931] | 0.852 [0.814–0.890] | 0.103 [0.101–0.106] |
| page (answered only) | 318 | 0.593 [0.558–0.626] | 0.611 [0.573–0.649] | 0.745 [0.705–0.786] | 0.651 [0.601–0.701] | 0.221 [0.204–0.238] |

Latency over 318 answered queries (ms): mean 5592.4, p50 4923.7, p95 9587.3, p99 12435.5

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 4917.5, p95 9575.6, p99 12433.7. Client-measured: p50 4923.7, p95 9587.3, p99 12435.5 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 56.9 | 37.0 | 161.6 | 319.5 |
| context | 318 | 4.8 | 0.2 | 11.4 | 27.4 |
| detail.engineTotal | 318 | 5560.4 | 4916.0 | 9171.4 | 12432.8 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 56.9 | 37.0 | 161.6 | 319.5 |
| detail.queryEmbed | 318 | 2616.8 | 1905.8 | 6393.9 | 9607.6 |
| detail.scopeCheck | 318 | 3.2 | 1.3 | 10.7 | 37.5 |
| detail.vectorArm | 318 | 56.9 | 37.0 | 161.6 | 319.5 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 2.9 | 1.5 | 8.9 | 24.4 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 83.7 | 32.7 | 282.0 | 665.3 |
| rerank | 318 | 2788.7 | 2680.1 | 3643.5 | 4680.7 |
| scopeBuild | 318 | 2.9 | 1.0 | 11.1 | 22.3 |
| serialise | 318 | 8.5 | 0.3 | 11.2 | 27.2 |
| total | 318 | 5575.3 | 4917.5 | 9575.6 | 12433.7 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
