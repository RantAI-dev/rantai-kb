# Retrieval evaluation — ing5-qwen3vl8b-standalone

- dataset: `images-v1`
- harness commit: `14b887a1`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-11-13T15:23:07.985783924+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 350
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.883 [0.851–0.914] | 0.985 [0.965–1.000] | 0.850 [0.809–0.888] | 0.775 [0.715–0.835] | 0.099 [0.097–0.100] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.883 [0.851–0.914] | 0.985 [0.965–1.000] | 0.850 [0.809–0.888] | 0.775 [0.715–0.835] | 0.099 [0.097–0.100] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 16.0, p50 15.5, p95 19.8, p99 23.6

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 13.9, p95 18.1, p99 21.6. Client-measured: p50 15.5, p95 19.8, p99 23.6 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 5.0 | 4.9 | 6.1 | 6.6 |
| context | 200 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 200 | 12.1 | 11.8 | 14.7 | 17.2 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.globalArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 5.0 | 4.9 | 6.1 | 6.6 |
| detail.pageArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 200 | 0.4 | 0.4 | 0.5 | 0.6 |
| detail.scopeCheck | 200 | 1.8 | 1.7 | 2.7 | 4.6 |
| detail.vectorArm | 200 | 5.0 | 4.9 | 6.1 | 6.6 |
| detail.visionRerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 1.7 | 1.7 | 1.9 | 2.1 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 3.0 | 3.0 | 3.6 | 4.2 |
| pages | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 200 | 2.1 | 1.8 | 3.3 | 4.4 |
| serialise | 200 | 0.1 | 0.1 | 0.2 | 0.2 |
| total | 200 | 14.4 | 13.9 | 18.1 | 21.6 |

Query embeddings: 200 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
