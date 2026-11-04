# Retrieval evaluation — voy-text

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T21:41:28.586519+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.934 [0.915–0.953] | 0.985 [0.971–0.995] | 0.921 [0.897–0.943] | 0.874 [0.840–0.909] | 0.104 [0.102–0.107] |
| page | 318 | 0.645 [0.611–0.679] | 0.647 [0.608–0.686] | 0.789 [0.752–0.826] | 0.704 [0.657–0.755] | 0.233 [0.215–0.250] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.934 [0.915–0.953] | 0.985 [0.971–0.995] | 0.921 [0.897–0.943] | 0.874 [0.840–0.909] | 0.104 [0.102–0.107] |
| page (answered only) | 318 | 0.645 [0.611–0.679] | 0.647 [0.608–0.686] | 0.789 [0.752–0.826] | 0.704 [0.657–0.755] | 0.233 [0.215–0.250] |

Latency over 318 answered queries (ms): mean 1306.7, p50 580.5, p95 5951.4, p99 11013.1

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 578.6, p95 5949.6, p99 11010.0. Client-measured: p50 580.5, p95 5951.4, p99 11013.1 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 36.6 | 35.6 | 44.4 | 50.0 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.3 |
| detail.engineTotal | 318 | 1303.3 | 577.4 | 5948.3 | 11008.6 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 36.6 | 35.6 | 44.4 | 50.0 |
| detail.queryEmbed | 318 | 696.7 | 0.7 | 5381.5 | 10456.5 |
| detail.scopeCheck | 318 | 1.3 | 1.1 | 2.0 | 3.4 |
| detail.vectorArm | 318 | 36.6 | 35.6 | 44.4 | 50.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.5 | 1.3 | 2.7 | 4.3 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 18.4 | 17.0 | 36.3 | 45.3 |
| rerank | 318 | 548.7 | 512.1 | 584.1 | 1989.9 |
| scopeBuild | 318 | 1.1 | 0.9 | 1.8 | 3.4 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.4 |
| total | 318 | 1304.7 | 578.6 | 5949.6 | 11010.0 |

Query embeddings: 256 cache hit(s), 62 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
