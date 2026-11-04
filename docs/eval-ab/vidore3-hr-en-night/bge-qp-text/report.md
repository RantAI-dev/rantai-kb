# Retrieval evaluation — bge-qp-text

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T03:09:42.275801992+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.926 [0.905–0.947] | 0.979 [0.963–0.992] | 0.912 [0.886–0.937] | 0.858 [0.821–0.896] | 0.103 [0.101–0.106] |
| page | 318 | 0.618 [0.585–0.651] | 0.650 [0.608–0.687] | 0.749 [0.710–0.791] | 0.651 [0.597–0.701] | 0.241 [0.222–0.262] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.926 [0.905–0.947] | 0.979 [0.963–0.992] | 0.912 [0.886–0.937] | 0.858 [0.821–0.896] | 0.103 [0.101–0.106] |
| page (answered only) | 318 | 0.618 [0.585–0.651] | 0.650 [0.608–0.687] | 0.749 [0.710–0.791] | 0.651 [0.597–0.701] | 0.241 [0.222–0.262] |

Latency over 318 answered queries (ms): mean 2194.6, p50 2138.4, p95 2763.7, p99 2934.4

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 2136.7, p95 2742.0, p99 2932.7. Client-measured: p50 2138.4, p95 2763.7, p99 2934.4 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 43.4 | 32.3 | 92.0 | 214.2 |
| context | 318 | 0.2 | 0.1 | 0.3 | 2.9 |
| detail.engineTotal | 318 | 2189.6 | 2135.5 | 2741.0 | 2931.3 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 43.4 | 32.3 | 92.0 | 214.2 |
| detail.queryEmbed | 318 | 1.1 | 1.0 | 2.0 | 4.7 |
| detail.scopeCheck | 318 | 1.4 | 1.1 | 2.8 | 4.9 |
| detail.vectorArm | 318 | 43.4 | 32.3 | 92.0 | 214.2 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.8 | 1.4 | 4.3 | 7.8 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 21.4 | 19.2 | 42.1 | 63.1 |
| rerank | 318 | 2120.1 | 2077.6 | 2659.8 | 2870.2 |
| scopeBuild | 318 | 1.3 | 1.0 | 3.0 | 5.7 |
| serialise | 318 | 0.2 | 0.2 | 0.4 | 0.7 |
| total | 318 | 2191.2 | 2136.7 | 2742.0 | 2932.7 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
