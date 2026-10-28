# Retrieval evaluation — cs-vmm-ti-pr

- dataset: `vidore3-cs-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T06:47:55.833672050+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 2
- queries: 215 (failed: 0, degraded: 1, noted: 0)
- degradations reported by the server: rerank_unavailable

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 215 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.102 [0.100–0.104] |
| page | 215 | 0.828 [0.803–0.853] | 0.847 [0.819–0.873] | 0.918 [0.888–0.946] | 0.860 [0.814–0.907] | 0.360 [0.333–0.387] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 215 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.102 [0.100–0.104] |
| page (answered only) | 215 | 0.828 [0.803–0.853] | 0.847 [0.819–0.873] | 0.918 [0.888–0.946] | 0.860 [0.814–0.907] | 0.360 [0.333–0.387] |

Latency over 215 answered queries (ms): mean 2899.4, p50 1661.2, p95 4895.1, p99 36257.2

### Server-side stage timings (ms)

Server-side total over 215 responses: p50 1580.7, p95 4477.9, p99 24871.8. Client-measured: p50 1661.2, p95 4895.1, p99 36257.2 over 215 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 215 | 214.4 | 101.6 | 452.5 | 2242.5 |
| context | 215 | 6.8 | 3.7 | 11.1 | 21.6 |
| detail.engineTotal | 215 | 2655.2 | 1578.4 | 4389.1 | 22348.5 |
| detail.entityArm | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 215 | 0.1 | 0.0 | 0.4 | 1.4 |
| detail.keywordArm | 215 | 214.4 | 101.6 | 452.5 | 2242.5 |
| detail.queryEmbed | 215 | 19.1 | 8.1 | 39.8 | 127.5 |
| detail.scopeCheck | 215 | 26.9 | 7.1 | 126.3 | 214.3 |
| detail.vectorArm | 215 | 214.4 | 101.6 | 452.5 | 2242.5 |
| enrich | 215 | 0.6 | 0.0 | 2.5 | 5.3 |
| expand | 215 | 0.3 | 0.0 | 1.4 | 2.5 |
| figures | 215 | 54.7 | 7.2 | 172.2 | 516.3 |
| fusion | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 215 | 276.9 | 158.1 | 779.8 | 1377.4 |
| rerank | 215 | 344.4 | 68.9 | 1020.5 | 1488.8 |
| scopeBuild | 215 | 17.0 | 3.7 | 74.9 | 152.0 |
| serialise | 215 | 13.0 | 1.8 | 6.9 | 57.5 |
| total | 215 | 2692.0 | 1580.7 | 4477.9 | 24871.8 |

Query embeddings: 214 cache hit(s), 1 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
