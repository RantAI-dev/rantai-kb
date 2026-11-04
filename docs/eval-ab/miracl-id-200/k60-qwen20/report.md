# Retrieval evaluation — k60-qwen20

- dataset: `miracl-id-200`
- harness commit: `4c01a7d-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T05:15:01.837975963+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.849 [0.824–0.872] | 0.946 [0.928–0.963] | 0.879 [0.845–0.911] | 0.805 [0.750–0.855] | 0.291 [0.265–0.317] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.849 [0.824–0.872] | 0.946 [0.928–0.963] | 0.879 [0.845–0.911] | 0.805 [0.750–0.855] | 0.291 [0.265–0.317] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 3351.2, p50 3363.4, p95 4135.2, p99 4329.8

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 3361.8, p95 4133.3, p99 4328.4. Client-measured: p50 3363.4, p95 4135.2, p99 4329.8 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 34.7 | 31.6 | 50.5 | 79.6 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.5 |
| detail.engineTotal | 200 | 3342.5 | 3355.2 | 4126.7 | 4322.4 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 34.7 | 31.6 | 50.5 | 79.6 |
| detail.queryEmbed | 200 | 57.5 | 53.9 | 89.6 | 96.6 |
| detail.scopeCheck | 200 | 1.4 | 1.3 | 2.0 | 2.7 |
| detail.vectorArm | 200 | 34.7 | 31.6 | 50.5 | 79.6 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 0.9 | 0.8 | 1.0 | 2.2 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 2.5 | 2.3 | 4.0 | 6.0 |
| rerank | 200 | 3245.2 | 3249.3 | 4031.9 | 4234.9 |
| scopeBuild | 200 | 7.0 | 6.6 | 8.7 | 14.1 |
| serialise | 200 | 0.1 | 0.0 | 0.1 | 0.1 |
| total | 200 | 3349.6 | 3361.8 | 4133.3 | 4328.4 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
