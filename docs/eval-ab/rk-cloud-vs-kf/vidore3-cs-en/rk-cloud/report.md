# Retrieval evaluation — rk-cloud

- dataset: `vidore3-cs-en`
- harness commit: `ead4e48-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-31T02:01:14.601402390+00:00
- cutoff k: 10
- max chunks per query: 50
- documents known: 2
- queries: 215 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 215 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.102 [0.100–0.104] |
| page | 215 | 0.822 [0.798–0.847] | 0.839 [0.809–0.866] | 0.920 [0.891–0.947] | 0.865 [0.818–0.912] | 0.354 [0.327–0.381] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 215 | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 1.000 [1.000–1.000] | 0.102 [0.100–0.104] |
| page (answered only) | 215 | 0.822 [0.798–0.847] | 0.839 [0.809–0.866] | 0.920 [0.891–0.947] | 0.865 [0.818–0.912] | 0.354 [0.327–0.381] |

Latency over 215 answered queries (ms): mean 716.7, p50 832.9, p95 1316.4, p99 1608.2

### Server-side stage timings (ms)

Server-side total over 215 responses: p50 830.9, p95 1313.2, p99 1605.9. Client-measured: p50 832.9, p95 1316.4, p99 1608.2 over 215 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 215 | 6.7 | 6.3 | 9.2 | 10.6 |
| context | 215 | 0.3 | 0.3 | 0.4 | 0.5 |
| detail.engineTotal | 215 | 712.8 | 829.3 | 1311.7 | 1604.5 |
| detail.entityArm | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 215 | 6.7 | 6.3 | 9.2 | 10.6 |
| detail.pageArm | 215 | 9.1 | 9.0 | 11.5 | 12.6 |
| detail.queryEmbed | 215 | 0.6 | 0.6 | 0.8 | 0.9 |
| detail.scopeCheck | 215 | 2.1 | 2.5 | 3.3 | 4.2 |
| detail.vectorArm | 215 | 6.7 | 6.3 | 9.2 | 10.6 |
| detail.visionRerank | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 215 | 1.5 | 1.5 | 1.9 | 2.4 |
| fusion | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 215 | 72.4 | 70.9 | 128.6 | 145.8 |
| pages | 215 | 590.9 | 718.5 | 882.7 | 1166.6 |
| rerank | 215 | 38.2 | 1.0 | 522.6 | 573.8 |
| scopeBuild | 215 | 0.8 | 0.8 | 1.1 | 1.5 |
| serialise | 215 | 0.5 | 0.4 | 0.6 | 0.8 |
| total | 215 | 714.3 | 830.9 | 1313.2 | 1605.9 |

Query embeddings: 215 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
