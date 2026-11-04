# Retrieval evaluation — voy-qp-ti-pr

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T22:51:52.073131487+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.940 [0.921–0.959] | 0.983 [0.969–0.995] | 0.928 [0.906–0.952] | 0.881 [0.846–0.918] | 0.104 [0.101–0.107] |
| page | 318 | 0.708 [0.679–0.738] | 0.731 [0.696–0.765] | 0.808 [0.775–0.843] | 0.708 [0.660–0.758] | 0.286 [0.264–0.308] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.940 [0.921–0.959] | 0.983 [0.969–0.995] | 0.928 [0.906–0.952] | 0.881 [0.846–0.918] | 0.104 [0.101–0.107] |
| page (answered only) | 318 | 0.708 [0.679–0.738] | 0.731 [0.696–0.765] | 0.808 [0.775–0.843] | 0.708 [0.660–0.758] | 0.286 [0.264–0.308] |

Latency over 318 answered queries (ms): mean 1332.6, p50 1294.0, p95 1588.2, p99 2168.5

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 1291.6, p95 1585.1, p99 2166.5. Client-measured: p50 1294.0, p95 1588.2, p99 2168.5 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 43.9 | 38.9 | 67.5 | 147.5 |
| context | 318 | 0.2 | 0.1 | 0.3 | 0.5 |
| detail.engineTotal | 318 | 1328.6 | 1290.4 | 1583.7 | 2163.4 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 318 | 43.9 | 38.9 | 67.5 | 147.5 |
| detail.queryEmbed | 318 | 0.8 | 0.7 | 1.6 | 2.3 |
| detail.scopeCheck | 318 | 1.5 | 1.2 | 2.9 | 5.0 |
| detail.vectorArm | 318 | 43.9 | 38.9 | 67.5 | 147.5 |
| enrich | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 318 | 1.8 | 1.5 | 3.7 | 5.6 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 21.0 | 18.7 | 44.7 | 60.5 |
| rerank | 318 | 530.8 | 521.9 | 583.0 | 673.4 |
| scopeBuild | 318 | 1.4 | 1.1 | 3.0 | 4.5 |
| serialise | 318 | 0.2 | 0.2 | 0.3 | 0.6 |
| total | 318 | 1330.2 | 1291.6 | 1585.1 | 2166.5 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
