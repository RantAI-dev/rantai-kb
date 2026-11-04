# Retrieval evaluation — miracl-rerank-on

- dataset: `miracl-id-50`
- harness commit: `a6664c5-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-17T23:10:04.839833350+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 564
- queries: 50 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 50 | 0.891 [0.843–0.932] | 0.979 [0.957–0.996] | 0.890 [0.827–0.947] | 0.800 [0.680–0.900] | 0.296 [0.238–0.356] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 50 | 0.891 [0.843–0.932] | 0.979 [0.957–0.996] | 0.890 [0.827–0.947] | 0.800 [0.680–0.900] | 0.296 [0.238–0.356] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 50 answered queries (ms): mean 1371.2, p50 1368.0, p95 1685.2, p99 1995.4

### Server-side stage timings (ms)

Server-side total over 50 responses: p50 1364.2, p95 1681.3, p99 1991.8. Client-measured: p50 1368.0, p95 1685.2, p99 1995.4 over 50 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 50 | 20.0 | 19.8 | 24.8 | 30.8 |
| context | 50 | 0.1 | 0.1 | 0.2 | 0.2 |
| detail.engineTotal | 50 | 1359.9 | 1356.3 | 1672.8 | 1985.7 |
| detail.entityArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 50 | 20.0 | 19.8 | 24.8 | 30.8 |
| detail.queryEmbed | 50 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.scopeCheck | 50 | 1.9 | 1.9 | 2.7 | 3.1 |
| detail.vectorArm | 50 | 20.0 | 19.8 | 24.8 | 30.8 |
| enrich | 50 | 0.0 | 0.0 | 0.0 | 0.1 |
| expand | 50 | 0.0 | 0.0 | 0.0 | 0.1 |
| figures | 50 | 1.7 | 1.6 | 2.2 | 2.3 |
| fusion | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 50 | 9.2 | 9.0 | 13.4 | 16.0 |
| rerank | 50 | 1326.6 | 1325.2 | 1641.7 | 1953.4 |
| scopeBuild | 50 | 5.5 | 5.3 | 7.7 | 8.7 |
| serialise | 50 | 1.7 | 1.6 | 2.1 | 2.6 |
| total | 50 | 1367.3 | 1364.2 | 1681.3 | 1991.8 |

Query embeddings: 50 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
