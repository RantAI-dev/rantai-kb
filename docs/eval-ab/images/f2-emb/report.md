# Retrieval evaluation — ing5-qwen3vl2b-embedded

- dataset: `images-v1-embedded`
- harness commit: `14b887a1`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-13T15:08:39.409097653+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 350
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.901 [0.872–0.931] | 0.980 [0.960–0.995] | 0.875 [0.840–0.911] | 0.800 [0.750–0.855] | 0.098 [0.096–0.100] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.901 [0.872–0.931] | 0.980 [0.960–0.995] | 0.875 [0.840–0.911] | 0.800 [0.750–0.855] | 0.098 [0.096–0.100] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 15.7, p50 15.6, p95 18.1, p99 18.7

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 13.9, p95 16.3, p99 16.8. Client-measured: p50 15.6, p95 18.1, p99 18.7 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 5.1 | 5.1 | 6.0 | 6.5 |
| context | 200 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 200 | 11.9 | 11.8 | 13.9 | 14.4 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.globalArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 5.1 | 5.1 | 6.0 | 6.5 |
| detail.pageArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 200 | 0.2 | 0.2 | 0.2 | 0.2 |
| detail.scopeCheck | 200 | 1.6 | 1.6 | 2.0 | 2.1 |
| detail.vectorArm | 200 | 5.1 | 5.1 | 6.0 | 6.5 |
| detail.visionRerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.8 | 1.8 | 2.2 | 2.4 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 3.1 | 3.1 | 3.8 | 4.2 |
| pages | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 200 | 1.8 | 1.7 | 2.2 | 2.3 |
| serialise | 200 | 0.1 | 0.1 | 0.2 | 0.2 |
| total | 200 | 14.0 | 13.9 | 16.3 | 16.8 |

Query embeddings: 200 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
