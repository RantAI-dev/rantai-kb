# Search load test — l4-optimized-rerank-on

- target: `http://127.0.0.1:18080`
- harness commit: `a6664c5-dirty`
- timestamp: 2026-10-17T23:07:54.148684443+00:00
- queries cycled: 50
- max chunks per query: 8
- warm-up requests: 2 (discarded)

Closed loop: every worker holds one request in flight and sends the next when the
last answers. Latency is client-side (HTTP round trip plus JSON), so it is always at
least the server's own `total` below.

| concurrency | requests | errors | degraded | req/s | p50 ms | p95 ms | p99 ms | max ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 18 | 0 | 0 | 0.58 | 1660.9 | 2631.7 | 2631.7 | 2631.7 |
| 8 | 29 | 0 | 21 | 0.74 | 10076.2 | 11059.2 | 11272.2 | 11272.2 |
| 32 | 99 | 0 | 96 | 2.51 | 10166.3 | 18513.9 | 19038.2 | 19038.2 |

146 requests, 0 error(s), 117 degraded.

HTTP statuses at concurrency 1: 200 × 18

Degrades at concurrency 8 (a 2xx answer with an arm down, not an error):
- rerank_unavailable × 21

HTTP statuses at concurrency 8: 200 × 29

Degrades at concurrency 32 (a 2xx answer with an arm down, not an error):
- rerank_unavailable × 96

HTTP statuses at concurrency 32: 200 × 99

## Server-side stage p95 (ms), from `stats.stageTimingsMs`

`detail.*` rows are overlapping measurements — the query embed and both arms happen
inside `arms` — and are read, not summed.

### concurrency 1 (18 of 18 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 20.9 | 24.8 |
| context | 0.1 | 0.2 |
| detail.engineTotal | 1649.7 | 2623.5 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 20.9 | 24.8 |
| detail.queryEmbed | 331.5 | 405.2 |
| detail.scopeCheck | 2.4 | 3.8 |
| detail.vectorArm | 20.9 | 24.8 |
| enrich | 0.0 | 0.1 |
| expand | 0.0 | 0.0 |
| figures | 1.7 | 2.4 |
| fusion | 0.0 | 0.0 |
| neighbours | 7.2 | 11.4 |
| rerank | 1300.5 | 2244.6 |
| scopeBuild | 5.3 | 8.5 |
| serialise | 1.3 | 1.6 |
| total | 1656.8 | 2627.6 |

### concurrency 8 (29 of 29 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 26.0 | 143.0 |
| context | 0.1 | 0.2 |
| detail.engineTotal | 10051.6 | 11042.8 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 26.0 | 143.0 |
| detail.queryEmbed | 0.1 | 1006.6 |
| detail.scopeCheck | 3.7 | 9.9 |
| detail.vectorArm | 26.0 | 143.0 |
| enrich | 0.0 | 0.1 |
| expand | 0.0 | 0.0 |
| figures | 2.3 | 5.7 |
| fusion | 0.0 | 0.0 |
| neighbours | 8.6 | 13.6 |
| rerank | 10002.4 | 10002.4 |
| scopeBuild | 6.6 | 23.0 |
| serialise | 1.3 | 2.0 |
| total | 10068.0 | 11051.0 |

### concurrency 32 (99 of 99 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 43.2 | 278.0 |
| context | 0.1 | 0.2 |
| detail.engineTotal | 10125.3 | 18481.2 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 43.2 | 278.0 |
| detail.queryEmbed | 0.1 | 8405.0 |
| detail.scopeCheck | 3.6 | 11.7 |
| detail.vectorArm | 43.2 | 278.0 |
| enrich | 0.0 | 0.1 |
| expand | 0.0 | 0.0 |
| figures | 3.9 | 16.6 |
| fusion | 0.0 | 0.0 |
| neighbours | 10.4 | 49.6 |
| rerank | 10002.4 | 10010.6 |
| scopeBuild | 13.2 | 74.2 |
| serialise | 1.5 | 5.8 |
| total | 10141.7 | 18497.5 |

