# Retrieval evaluation — md-bge20

- dataset: `vidore3-hr-en-md`
- harness commit: `b0394eb`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T07:18:13.750812485+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 1110
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.549 [0.513–0.586] | 0.549 [0.507–0.592] | 0.718 [0.676–0.762] | 0.629 [0.579–0.686] | 0.200 [0.184–0.218] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.549 [0.513–0.586] | 0.549 [0.507–0.592] | 0.718 [0.676–0.762] | 0.629 [0.579–0.686] | 0.200 [0.184–0.218] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 318 answered queries (ms): mean 4719.9, p50 4021.3, p95 12099.1, p99 14883.7

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 4019.5, p95 12097.5, p99 14881.8. Client-measured: p50 4021.3, p95 12099.1, p99 14883.7 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 23.6 | 21.2 | 35.9 | 43.7 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.3 |
| detail.engineTotal | 318 | 4714.6 | 4014.2 | 12094.3 | 14878.1 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 23.6 | 21.2 | 35.9 | 43.7 |
| detail.queryEmbed | 318 | 83.7 | 91.0 | 96.9 | 111.2 |
| detail.scopeCheck | 318 | 1.3 | 1.3 | 1.7 | 2.4 |
| detail.vectorArm | 318 | 23.6 | 21.2 | 35.9 | 43.7 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 0.9 | 0.9 | 1.2 | 2.9 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 4.7 | 4.1 | 8.9 | 13.4 |
| rerank | 318 | 4600.0 | 3891.0 | 11969.8 | 14756.2 |
| scopeBuild | 318 | 3.3 | 3.1 | 4.4 | 6.8 |
| serialise | 318 | 0.2 | 0.2 | 0.2 | 0.3 |
| total | 318 | 4718.1 | 4019.5 | 12097.5 | 14881.8 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
