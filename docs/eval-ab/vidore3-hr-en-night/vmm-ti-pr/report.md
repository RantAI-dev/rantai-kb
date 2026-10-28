# Retrieval evaluation — vmm-ti-pr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T03:27:44.355857907+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.943 [0.927–0.960] | 0.986 [0.973–0.996] | 0.932 [0.912–0.955] | 0.887 [0.855–0.921] | 0.104 [0.102–0.107] |
| page | 318 | 0.711 [0.683–0.743] | 0.735 [0.699–0.769] | 0.809 [0.778–0.844] | 0.711 [0.664–0.761] | 0.288 [0.267–0.311] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.943 [0.927–0.960] | 0.986 [0.973–0.996] | 0.932 [0.912–0.955] | 0.887 [0.855–0.921] | 0.104 [0.102–0.107] |
| page (answered only) | 318 | 0.711 [0.683–0.743] | 0.735 [0.699–0.769] | 0.809 [0.778–0.844] | 0.711 [0.664–0.761] | 0.288 [0.267–0.311] |

Latency over 318 answered queries (ms): mean 2213.2, p50 2119.6, p95 2847.5, p99 3329.3

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2116.1, p95 2844.8, p99 3323.5. Client-measured: p50 2119.6, p95 2847.5, p99 3329.3 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 25.6 | 18.9 | 67.6 | 110.5 |
| context | 318 | 0.4 | 0.2 | 1.0 | 4.6 |
| detail.engineTotal | 318 | 2206.1 | 2114.0 | 2843.4 | 3314.8 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 25.6 | 18.9 | 67.6 | 110.5 |
| detail.queryEmbed | 318 | 800.5 | 774.6 | 882.2 | 1538.3 |
| detail.scopeCheck | 318 | 1.7 | 1.3 | 3.6 | 5.2 |
| detail.vectorArm | 318 | 25.6 | 18.9 | 67.6 | 110.5 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 2.1 | 1.6 | 5.8 | 8.3 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 31.2 | 21.1 | 101.7 | 131.6 |
| rerank | 318 | 559.7 | 541.7 | 615.4 | 1201.1 |
| scopeBuild | 318 | 1.5 | 1.2 | 3.0 | 4.5 |
| serialise | 318 | 0.5 | 0.2 | 1.3 | 7.5 |
| total | 318 | 2208.3 | 2116.1 | 2844.8 | 3323.5 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
