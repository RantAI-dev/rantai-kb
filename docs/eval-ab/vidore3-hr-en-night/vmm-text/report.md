# Retrieval evaluation — vmm-text

- dataset: `vidore3-hr-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-24T03:32:01.700715834+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 14
- queries: 318 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 318 | 0.944 [0.926–0.961] | 0.988 [0.977–0.997] | 0.931 [0.910–0.953] | 0.887 [0.855–0.921] | 0.104 [0.102–0.107] |
| page | 318 | 0.674 [0.646–0.704] | 0.680 [0.644–0.715] | 0.808 [0.772–0.844] | 0.720 [0.667–0.774] | 0.250 [0.232–0.269] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 318 | 0.944 [0.926–0.961] | 0.988 [0.977–0.997] | 0.931 [0.910–0.953] | 0.887 [0.855–0.921] | 0.104 [0.102–0.107] |
| page (answered only) | 318 | 0.674 [0.646–0.704] | 0.680 [0.644–0.715] | 0.808 [0.772–0.844] | 0.720 [0.667–0.774] | 0.250 [0.232–0.269] |

Latency over 318 answered queries (ms): mean 732.6, p50 630.8, p95 1427.8, p99 3925.5

### Server-side stage timings (ms)

Server-side total over 318 responses: p50 615.3, p95 1312.4, p99 3244.5. Client-measured: p50 630.8, p95 1427.8, p99 3925.5 over 318 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 318 | 95.1 | 55.6 | 233.7 | 389.5 |
| context | 318 | 3.2 | 2.8 | 7.7 | 11.8 |
| detail.engineTotal | 318 | 646.6 | 611.2 | 1264.7 | 3203.9 |
| detail.entityArm | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 318 | 0.0 | 0.0 | 0.0 | 0.2 |
| detail.keywordArm | 318 | 95.1 | 55.6 | 233.7 | 389.5 |
| detail.queryEmbed | 318 | 7.7 | 5.1 | 22.7 | 46.9 |
| detail.scopeCheck | 318 | 27.4 | 7.8 | 82.8 | 123.9 |
| detail.vectorArm | 318 | 95.1 | 55.6 | 233.7 | 389.5 |
| enrich | 318 | 0.2 | 0.0 | 0.8 | 1.5 |
| expand | 318 | 0.0 | 0.0 | 0.0 | 0.9 |
| figures | 318 | 22.2 | 6.7 | 81.7 | 148.9 |
| fusion | 318 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 318 | 109.6 | 58.7 | 194.5 | 1295.2 |
| rerank | 318 | 375.8 | 530.2 | 755.1 | 1490.7 |
| scopeBuild | 318 | 12.1 | 3.4 | 39.9 | 51.6 |
| serialise | 318 | 1.8 | 1.3 | 5.4 | 9.9 |
| total | 318 | 664.2 | 615.3 | 1312.4 | 3244.5 |

Query embeddings: 318 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
