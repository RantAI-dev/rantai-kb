# Retrieval evaluation — cs-voy-text

- dataset: `vidore3-cs-en`
- harness commit: `1aadbcc-dirty`
- nDCG gain: **linear**
- server version: `dev` (GET /health)
- server commit: null — no kb-server route exposes a build commit (`/health` reports only `version`)
- timestamp: 2026-10-23T22:31:06.481242524+00:00
- cutoff k: 10
- max chunks per query: 20
- documents known: 2
- queries: 215 (failed: 0, degraded: 0, noted: 0)

Strict means: every evaluable query is in them, and a query the server failed
to answer is scored as a zero at every level where it was evaluable.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document | 215 | 0.998 [0.994–1.000] | 1.000 [1.000–1.000] | 0.998 [0.993–1.000] | 0.995 [0.986–1.000] | 0.102 [0.100–0.104] |
| page | 215 | 0.785 [0.756–0.813] | 0.775 [0.740–0.808] | 0.930 [0.902–0.955] | 0.874 [0.828–0.916] | 0.315 [0.292–0.340] |

### Answered only (secondary)

Failed queries are left out of this table. It separates "the backend is worse"
from "the backend is down"; it is never the number to publish, because a
backend that fails exactly the hard queries scores higher here.

| level | n | nDCG@k | Recall@k | MRR@k | Hit@1 | P@k |
|---|---|---|---|---|---|---|
| document (answered only) | 215 | 0.998 [0.994–1.000] | 1.000 [1.000–1.000] | 0.998 [0.993–1.000] | 0.995 [0.986–1.000] | 0.102 [0.100–0.104] |
| page (answered only) | 215 | 0.785 [0.756–0.813] | 0.775 [0.740–0.808] | 0.930 [0.902–0.955] | 0.874 [0.828–0.916] | 0.315 [0.292–0.340] |

Latency over 215 answered queries (ms): mean 656.8, p50 637.8, p95 768.5, p99 1188.8

### Server-side stage timings (ms)

Server-side total over 215 responses: p50 634.5, p95 764.2, p99 1182.4. Client-measured: p50 637.8, p95 768.5, p99 1188.8 over 215 answered queries — the gap is HTTP, the serialisation the route cannot time from the inside, and the client's JSON parsing.

The first block is a partition of the request: those stages run one after another and
add up to no more than `total`. `detail.*` rows are overlapping — the query embed and
both arms happen inside `arms` — and must never be summed with it.

| stage | n | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| arms | 215 | 32.2 | 30.0 | 48.1 | 69.9 |
| context | 215 | 0.1 | 0.1 | 0.2 | 1.1 |
| detail.engineTotal | 215 | 652.3 | 632.4 | 761.5 | 1176.8 |
| detail.entityArm | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.graphArm | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| detail.keywordArm | 215 | 32.2 | 30.0 | 48.1 | 69.9 |
| detail.queryEmbed | 215 | 0.8 | 0.7 | 1.6 | 2.5 |
| detail.scopeCheck | 215 | 1.8 | 1.1 | 4.5 | 12.5 |
| detail.vectorArm | 215 | 32.2 | 30.0 | 48.1 | 69.9 |
| enrich | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| expand | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| figures | 215 | 2.1 | 1.7 | 4.5 | 6.7 |
| fusion | 215 | 0.0 | 0.0 | 0.0 | 0.0 |
| neighbours | 215 | 77.7 | 71.9 | 175.6 | 264.8 |
| rerank | 215 | 537.3 | 521.8 | 574.0 | 1027.9 |
| scopeBuild | 215 | 1.5 | 1.1 | 3.3 | 5.9 |
| serialise | 215 | 0.2 | 0.2 | 0.3 | 1.0 |
| total | 215 | 654.1 | 634.5 | 764.2 | 1182.4 |

Query embeddings: 215 cache hit(s), 0 provider call(s). The two are counted rather than averaged: one number for both states would describe neither.

`n` is the number of queries evaluable at that level: a query with no judged
document, or no judged page, is left out rather than scored as zero.
