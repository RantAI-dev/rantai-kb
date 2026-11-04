# Retrieval evaluation — lite-e5s-hr-512

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T19:07:59.915992417+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.890 [0.864–0.913] | 0.975 [0.958–0.989] | 0.868 [0.838–0.896] | 0.792 [0.748–0.833] | 0.103 [0.100–0.106] |
| page | 318 | 0.524 [0.486–0.559] | 0.572 [0.528–0.611] | 0.656 [0.611–0.703] | 0.550 [0.497–0.607] | 0.220 [0.201–0.241] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.890 [0.864–0.913] | 0.975 [0.958–0.989] | 0.868 [0.838–0.896] | 0.792 [0.748–0.833] | 0.103 [0.100–0.106] |
| page (answered only) | 318 | 0.524 [0.486–0.559] | 0.572 [0.528–0.611] | 0.656 [0.611–0.703] | 0.550 [0.497–0.607] | 0.220 [0.201–0.241] |

Latency over 318 answered queries (ms): mean 50.0, p50 48.6, p95 62.9, p99 71.5

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 47.0, p95 61.2, p99 69.8. Client-measured: p50 48.6, p95 62.9, p99 71.5 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 8.2 | 7.8 | 11.4 | 13.2 |
| context | 318 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 318 | 47.1 | 45.9 | 60.1 | 68.5 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 8.2 | 7.8 | 11.4 | 13.2 |
| detail.pageArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 318 | 21.6 | 21.5 | 28.6 | 33.2 |
| detail.scopeCheck | 318 | 1.2 | 1.0 | 2.4 | 4.0 |
| detail.vectorArm | 318 | 8.2 | 7.8 | 11.4 | 13.2 |
| detail.visionRerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.0 | 1.0 | 1.4 | 1.6 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 14.9 | 14.9 | 27.9 | 31.8 |
| pages | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 318 | 0.9 | 0.8 | 1.4 | 2.3 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.3 |
| total | 318 | 48.3 | 47.0 | 61.2 | 69.8 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
