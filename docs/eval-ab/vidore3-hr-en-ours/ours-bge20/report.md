# Retrieval evaluation — ours-bge20

- dataset: `vidore3-hr-en-ours`
- harness commit: `62958fd-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T08:17:13.432167714+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 1090
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.566 [0.528–0.600] | 0.573 [0.531–0.614] | 0.728 [0.686–0.769] | 0.635 [0.582–0.689] | 0.206 [0.190–0.224] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.566 [0.528–0.600] | 0.573 [0.531–0.614] | 0.728 [0.686–0.769] | 0.635 [0.582–0.689] | 0.206 [0.190–0.224] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 318 answered queries (ms): mean 2663.4, p50 2275.6, p95 5994.7, p99 8237.7

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2251.2, p95 5983.1, p99 8166.3. Client-measured: p50 2275.6, p95 5994.7, p99 8237.7 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 102.0 | 44.2 | 201.6 | 881.4 |
| context | 318 | 5.8 | 0.2 | 15.0 | 72.3 |
| detail.engineTotal | 318 | 2587.8 | 2237.0 | 5610.2 | 8158.0 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 102.0 | 44.2 | 201.6 | 881.4 |
| detail.queryEmbed | 318 | 98.3 | 93.0 | 178.3 | 285.5 |
| detail.scopeCheck | 318 | 10.8 | 1.6 | 5.6 | 34.2 |
| detail.vectorArm | 318 | 102.0 | 44.2 | 201.6 | 881.4 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.1 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 7.3 | 1.7 | 16.0 | 139.2 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 42.7 | 10.5 | 115.7 | 923.5 |
| rerank | 318 | 2318.6 | 2032.5 | 4482.1 | 7343.4 |
| scopeBuild | 318 | 18.9 | 5.2 | 54.4 | 380.6 |
| serialise | 318 | 17.4 | 0.3 | 21.7 | 143.8 |
| total | 318 | 2624.5 | 2251.2 | 5983.1 | 8166.3 |

Query embeddings: 0 cache hit(s), 318 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
