# Retrieval evaluation — final

- dataset: `miracl-id-200`
- harness commit: `28d8372-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T11:02:06.481523005+00:00
- cutoff k: 10
- max chunks per query: 10
- documents known: 2933
- queries: 200 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 200 | 0.792 [0.762–0.823] | 0.928 [0.904–0.950] | 0.822 [0.779–0.862] | 0.720 [0.655–0.780] | 0.286 [0.260–0.311] |
| page | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 200 | 0.792 [0.762–0.823] | 0.928 [0.904–0.950] | 0.822 [0.779–0.862] | 0.720 [0.655–0.780] | 0.286 [0.260–0.311] |
| page (answered only) | 0 | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] | 0.000 [0.000–0.000] |

Latency over 200 answered queries (ms): mean 104.8, p50 100.3, p95 128.9, p99 150.3

### Server-side stage timings (ms)

Server-side total over 200 responses: p50 98.6, p95 127.3, p99 148.7. Client-measured: p50 100.3, p95 128.9, p99 150.3 over 200 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 200 | 35.0 | 31.6 | 47.4 | 55.1 |
| context | 200 | 0.1 | 0.1 | 0.1 | 0.1 |
| detail.engineTotal | 200 | 96.3 | 91.8 | 119.9 | 142.3 |
| detail.entityArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 200 | 35.0 | 31.6 | 47.4 | 55.1 |
| detail.queryEmbed | 200 | 56.3 | 54.1 | 81.9 | 92.6 |
| detail.scopeCheck | 200 | 1.6 | 1.4 | 2.3 | 3.4 |
| detail.vectorArm | 200 | 35.0 | 31.6 | 47.4 | 55.1 |
| enrich | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 200 | 0.9 | 0.8 | 1.0 | 1.4 |
| fusion | 200 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 200 | 2.4 | 2.3 | 3.3 | 4.2 |
| rerank | 200 | 0.0 | 0.0 | 0.0 | 0.1 |
| scopeBuild | 200 | 6.8 | 6.5 | 8.2 | 9.3 |
| serialise | 200 | 0.0 | 0.0 | 0.1 | 0.1 |
| total | 200 | 103.2 | 98.6 | 127.3 | 148.7 |

Query embeddings: 0 cache hit(s), 200 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
