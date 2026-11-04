# Retrieval evaluation — final-bge20

- dataset: `hotpot-200-test`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T10:52:19.270333520+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 995
- queries: 100 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 100 | 0.928 [0.902–0.952] | 0.945 [0.910–0.970] | 0.990 [0.975–1.000] | 0.980 [0.950–1.000] | 0.189 [0.182–0.194] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 100 | 0.928 [0.902–0.952] | 0.945 [0.910–0.970] | 0.990 [0.975–1.000] | 0.980 [0.950–1.000] | 0.189 [0.182–0.194] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 100 answered queries (ms): mean 1511.6, p50 1509.6, p95 1902.1, p99 2096.2

### Server-side stage timings (ms)

Server-side total over 100 responses: p50 1508.4, p95 1900.7, p99 2094.7. Client-measured: p50 1509.6, p95 1902.1, p99 2096.2 over 100 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 100 | 14.0 | 12.9 | 18.8 | 24.6 |
| context | 100 | 0.1 | 0.1 | 0.1 | 0.2 |
| detail.engineTotal | 100 | 1507.2 | 1505.7 | 1898.0 | 2091.1 |
| detail.entityArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 100 | 14.0 | 12.9 | 18.8 | 24.6 |
| detail.queryEmbed | 100 | 82.4 | 91.3 | 95.4 | 97.2 |
| detail.scopeCheck | 100 | 1.4 | 1.3 | 2.0 | 3.2 |
| detail.vectorArm | 100 | 14.0 | 12.9 | 18.8 | 24.6 |
| enrich | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 100 | 0.7 | 0.7 | 0.9 | 1.0 |
| fusion | 100 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 100 | 2.3 | 2.2 | 2.8 | 3.1 |
| rerank | 100 | 1406.2 | 1397.2 | 1785.8 | 1970.7 |
| scopeBuild | 100 | 2.8 | 2.7 | 3.4 | 4.5 |
| serialise | 100 | 0.0 | 0.0 | 0.1 | 0.1 |
| total | 100 | 1510.2 | 1508.4 | 1900.7 | 2094.7 |

Query embeddings: 0 cache hit(s), 100 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
