# Retrieval evaluation — k60-bge10

- dataset: `hotpot-200-test`
- harness commit: `4c01a7d-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T05:20:13.313646547+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 995
- queries: 100 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 100 | 0.900 [0.869–0.929] | 0.905 [0.865–0.940] | 0.990 [0.975–1.000] | 0.980 [0.950–1.000] | 0.181 [0.173–0.188] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 100 | 0.900 [0.869–0.929] | 0.905 [0.865–0.940] | 0.990 [0.975–1.000] | 0.980 [0.950–1.000] | 0.181 [0.173–0.188] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 100 answered queries (ms): mean 833.3, p50 818.7, p95 1068.0, p99 1203.7

### Server-side stage timings (ms)

Server-side total over 100 responses: p50 814.7, p95 1066.5, p99 1202.3. Client-measured: p50 818.7, p95 1068.0, p99 1203.7 over 100 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 100 | 18.5 | 16.5 | 34.2 | 40.2 |
| context | 100 | 0.2 | 0.1 | 0.6 | 1.8 |
| detail.engineTotal | 100 | 826.6 | 811.8 | 1063.0 | 1199.4 |
| detail.entityArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 100 | 18.5 | 16.5 | 34.2 | 40.2 |
| detail.queryEmbed | 100 | 80.3 | 89.4 | 93.2 | 95.5 |
| detail.scopeCheck | 100 | 1.6 | 1.4 | 2.6 | 5.0 |
| detail.vectorArm | 100 | 18.5 | 16.5 | 34.2 | 40.2 |
| enrich | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 100 | 1.0 | 0.8 | 1.5 | 4.1 |
| fusion | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 100 | 3.7 | 2.8 | 8.0 | 23.2 |
| rerank | 100 | 721.1 | 706.6 | 939.2 | 1093.3 |
| scopeBuild | 100 | 3.6 | 2.9 | 7.9 | 11.1 |
| serialise | 100 | 0.1 | 0.0 | 0.7 | 0.9 |
| total | 100 | 830.3 | 814.7 | 1066.5 | 1202.3 |

Query embeddings: 0 cache hit(s), 100 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
