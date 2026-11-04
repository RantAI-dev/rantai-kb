# Retrieval evaluation — text

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T17:48:36.740618574+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.915 [0.893–0.935] | 0.982 [0.968–0.994] | 0.897 [0.871–0.922] | 0.836 [0.799–0.874] | 0.104 [0.101–0.107] |
| page | 318 | 0.572 [0.536–0.605] | 0.626 [0.586–0.666] | 0.702 [0.660–0.746] | 0.579 [0.525–0.635] | 0.225 [0.208–0.243] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.915 [0.893–0.935] | 0.982 [0.968–0.994] | 0.897 [0.871–0.922] | 0.836 [0.799–0.874] | 0.104 [0.101–0.107] |
| page (answered only) | 318 | 0.572 [0.536–0.605] | 0.626 [0.586–0.666] | 0.702 [0.660–0.746] | 0.579 [0.525–0.635] | 0.225 [0.208–0.243] |

Latency over 318 answered queries (ms): mean 63.0, p50 60.6, p95 92.0, p99 115.8

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 58.5, p95 88.2, p99 111.3. Client-measured: p50 60.6, p95 92.0, p99 115.8 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 34.4 | 33.0 | 46.1 | 53.2 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.3 |
| detail.engineTotal | 318 | 59.0 | 56.4 | 86.5 | 110.0 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 34.4 | 33.0 | 46.1 | 53.2 |
| detail.queryEmbed | 318 | 0.8 | 0.6 | 1.5 | 3.3 |
| detail.scopeCheck | 318 | 1.5 | 1.1 | 3.9 | 5.5 |
| detail.vectorArm | 318 | 34.4 | 33.0 | 46.1 | 53.2 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.8 | 1.4 | 4.2 | 6.2 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 20.4 | 17.4 | 43.7 | 56.2 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 1.2 | 1.0 | 2.5 | 4.3 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.4 |
| total | 318 | 60.5 | 58.5 | 88.2 | 111.3 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
