# Retrieval evaluation — rk-cloud-text

- dataset: `vidore3-cs-en`
- harness commit: `ead4e48-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-31T02:01:31.033823252+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 2
- queries: 215 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 215 | 0.998 [0.994–1.000] | 1.000 [1.000–1.000] | 0.998 [0.993–1.000] | 0.995 [0.986–1.000] | 0.102 [0.100–0.104] |
| page | 215 | 0.778 [0.749–0.808] | 0.770 [0.735–0.804] | 0.922 [0.894–0.946] | 0.865 [0.819–0.907] | 0.315 [0.292–0.340] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 215 | 0.998 [0.994–1.000] | 1.000 [1.000–1.000] | 0.998 [0.993–1.000] | 0.995 [0.986–1.000] | 0.102 [0.100–0.104] |
| page (answered only) | 215 | 0.778 [0.749–0.808] | 0.770 [0.735–0.804] | 0.922 [0.894–0.946] | 0.865 [0.819–0.907] | 0.315 [0.292–0.340] |

Latency over 215 answered queries (ms): mean 67.9, p50 60.5, p95 130.9, p99 148.9

### Server-side stage timings (ms)

Server-side total over 215 responses: p50 58.5, p95 129.2, p99 147.0. Client-measured: p50 60.5, p95 130.9, p99 148.9 over 215 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 215 | 6.4 | 6.1 | 8.5 | 12.5 |
| context | 215 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 215 | 64.9 | 57.4 | 128.2 | 145.9 |
| detail.entityArm | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 215 | 6.4 | 6.1 | 8.5 | 12.5 |
| detail.pageArm | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 215 | 0.2 | 0.2 | 0.2 | 0.4 |
| detail.scopeCheck | 215 | 1.0 | 0.9 | 1.2 | 3.2 |
| detail.vectorArm | 215 | 6.4 | 6.1 | 8.5 | 12.5 |
| detail.visionRerank | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 215 | 1.5 | 1.4 | 1.8 | 2.6 |
| fusion | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 215 | 55.3 | 48.2 | 117.6 | 136.3 |
| pages | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 215 | 0.3 | 0.3 | 0.4 | 0.6 |
| scopeBuild | 215 | 0.9 | 0.8 | 1.1 | 1.7 |
| serialise | 215 | 0.2 | 0.2 | 0.2 | 0.3 |
| total | 215 | 66.1 | 58.5 | 129.2 | 147.0 |

Query embeddings: 215 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
