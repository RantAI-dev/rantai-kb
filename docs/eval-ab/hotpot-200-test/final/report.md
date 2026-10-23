# Retrieval evaluation — final

- dataset: `hotpot-200-test`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T10:49:46.252083922+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 995
- queries: 100 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 100 | 0.814 [0.776–0.853] | 0.905 [0.865–0.940] | 0.897 [0.853–0.942] | 0.830 [0.760–0.900] | 0.181 [0.173–0.188] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 100 | 0.814 [0.776–0.853] | 0.905 [0.865–0.940] | 0.897 [0.853–0.942] | 0.830 [0.760–0.900] | 0.181 [0.173–0.188] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 100 answered queries (ms): mean 103.1, p50 111.5, p95 117.9, p99 122.7

### Server-side stage timings (ms)

Server-side total over 100 responses: p50 110.3, p95 116.7, p99 121.5. Client-measured: p50 111.5, p95 117.9, p99 122.7 over 100 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 100 | 13.0 | 12.0 | 17.3 | 18.1 |
| context | 100 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 100 | 99.0 | 107.5 | 113.9 | 118.9 |
| detail.entityArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 100 | 13.0 | 12.0 | 17.3 | 18.1 |
| detail.queryEmbed | 100 | 81.7 | 90.8 | 95.2 | 98.0 |
| detail.scopeCheck | 100 | 1.6 | 1.4 | 3.0 | 3.4 |
| detail.vectorArm | 100 | 13.0 | 12.0 | 17.3 | 18.1 |
| enrich | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 100 | 0.7 | 0.7 | 0.8 | 0.8 |
| fusion | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 100 | 1.9 | 1.8 | 2.3 | 4.5 |
| rerank | 100 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 100 | 2.8 | 2.6 | 3.5 | 4.7 |
| serialise | 100 | 0.0 | 0.0 | 0.1 | 0.1 |
| total | 100 | 101.9 | 110.3 | 116.7 | 121.5 |

Query embeddings: 0 cache hit(s), 100 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
