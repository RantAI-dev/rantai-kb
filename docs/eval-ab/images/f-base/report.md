# Retrieval evaluation — baseline-ocr-qwen3vl2b-gpu

- dataset: `images-v1`
- harness commit: `14b887a1`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-13T15:08:33.705300433+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 350
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.447 [0.385–0.508] | 0.555 [0.485–0.620] | 0.413 [0.350–0.475] | 0.345 [0.280–0.415] | 0.055 [0.048–0.062] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.447 [0.385–0.508] | 0.555 [0.485–0.620] | 0.413 [0.350–0.475] | 0.345 [0.280–0.415] | 0.055 [0.048–0.062] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 13.6, p50 13.5, p95 16.3, p99 17.4

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 12.1, p95 14.9, p99 15.9. Client-measured: p50 13.5, p95 16.3, p99 17.4 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 4.7 | 4.6 | 6.3 | 7.0 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.2 |
| detail.engineTotal | 200 | 10.4 | 10.3 | 12.8 | 13.8 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.globalArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 4.7 | 4.6 | 6.3 | 7.0 |
| detail.pageArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 200 | 0.2 | 0.1 | 0.2 | 0.5 |
| detail.scopeCheck | 200 | 1.6 | 1.5 | 2.1 | 2.2 |
| detail.vectorArm | 200 | 4.7 | 4.6 | 6.3 | 7.0 |
| detail.visionRerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 0.9 | 0.8 | 1.2 | 1.9 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 2.8 | 2.7 | 4.0 | 4.4 |
| pages | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 200 | 1.6 | 1.6 | 2.1 | 2.2 |
| serialise | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| total | 200 | 12.1 | 12.1 | 14.9 | 15.9 |

Query embeddings: 200 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
