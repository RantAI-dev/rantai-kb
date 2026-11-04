# Retrieval evaluation — lite-minilm-id50

- dataset: `miracl-id-50`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-25T05:14:04.692699465+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 1485
- queries: 50 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 50 | 0.679 [0.602–0.757] | 0.837 [0.770–0.910] | 0.696 [0.589–0.793] | 0.560 [0.420–0.700] | 0.244 [0.198–0.292] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 50 | 0.679 [0.602–0.757] | 0.837 [0.770–0.910] | 0.696 [0.589–0.793] | 0.560 [0.420–0.700] | 0.244 [0.198–0.292] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 50 answered queries (ms): mean 40.3, p50 38.7, p95 50.4, p99 55.3

### Server-side stage timings (ms)

Server-side total over 50 responses: p50 37.1, p95 48.9, p99 53.5. Client-measured: p50 38.7, p95 50.4, p99 55.3 over 50 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 50 | 17.4 | 16.1 | 23.7 | 25.4 |
| context | 50 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 50 | 34.6 | 33.2 | 44.7 | 48.9 |
| detail.entityArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 50 | 17.4 | 16.1 | 23.7 | 25.4 |
| detail.pageArm | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.queryEmbed | 50 | 11.0 | 10.7 | 14.2 | 20.4 |
| detail.scopeCheck | 50 | 2.0 | 1.6 | 3.3 | 4.3 |
| detail.vectorArm | 50 | 17.4 | 16.1 | 23.7 | 25.4 |
| detail.visionRerank | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| enrich | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 50 | 0.8 | 0.8 | 0.9 | 1.0 |
| fusion | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 50 | 3.2 | 3.0 | 4.5 | 6.3 |
| pages | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| rerank | 50 | 0.0 | 0.0 | 0.0 | 0.0 |
| scopeBuild | 50 | 4.1 | 3.9 | 5.4 | 5.6 |
| serialise | 50 | 0.1 | 0.1 | 0.1 | 0.1 |
| total | 50 | 38.8 | 37.1 | 48.9 | 53.5 |

Query embeddings: 0 cache hit(s), 50 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
