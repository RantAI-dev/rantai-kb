# Search load test — l4-optimized-rerank-off-warm

- target: `http://127.0.0.1:18080`
- harness commit: `a6664c5-dirty`
- timestamp: 2026-10-17T23:01:15.310290296+00:00
- queries cycled: 50
- max chunks per query: 50
- warm-up requests: 5 (discarded)

Closed loop: every worker holds one request in flight and sends the next when the
last answers. Latency is client-side (HTTP round trip plus JSON), so it is always at
least the server's own `total` below.

| concurrency | requests | errors | degraded | req/s | p50 ms | p95 ms | p99 ms | max ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 592 | 0 | 0 | 19.70 | 51.3 | 66.8 | 76.6 | 86.6 |
| 8 | 1681 | 0 | 0 | 55.92 | 144.3 | 189.1 | 208.5 | 245.1 |
| 32 | 2052 | 0 | 0 | 67.92 | 487.4 | 620.5 | 688.6 | 1054.7 |

4325 requests, 0 error(s), 0 degraded.

HTTP statuses at concurrency 1: 200 × 592

HTTP statuses at concurrency 8: 200 × 1681

HTTP statuses at concurrency 32: 200 × 2052

## Server-side stage p95 (ms), from `stats.stageTimingsMs`

`detail.*` rows are overlapping measurements — the query embed and both arms happen
inside `arms` — and are read, not summed.

### concurrency 1 (592 of 592 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 19.3 | 26.2 |
| context | 0.2 | 0.3 |
| detail.engineTotal | 35.7 | 47.3 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 19.3 | 26.2 |
| detail.queryEmbed | 0.1 | 0.2 |
| detail.scopeCheck | 2.2 | 4.2 |
| detail.vectorArm | 19.3 | 26.2 |
| enrich | 0.0 | 0.0 |
| expand | 0.0 | 0.0 |
| figures | 1.9 | 4.9 |
| fusion | 0.0 | 0.0 |
| neighbours | 10.3 | 16.5 |
| rerank | 0.1 | 0.1 |
| scopeBuild | 5.1 | 8.1 |
| serialise | 3.7 | 6.0 |
| total | 45.4 | 59.5 |

### concurrency 8 (1681 of 1681 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 51.9 | 84.4 |
| context | 0.2 | 1.2 |
| detail.engineTotal | 109.6 | 148.9 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 51.9 | 84.4 |
| detail.queryEmbed | 0.1 | 0.2 |
| detail.scopeCheck | 5.4 | 13.4 |
| detail.vectorArm | 51.9 | 84.4 |
| enrich | 0.0 | 0.0 |
| expand | 0.0 | 0.0 |
| figures | 12.0 | 28.5 |
| fusion | 0.0 | 0.0 |
| neighbours | 32.8 | 55.4 |
| rerank | 0.1 | 0.1 |
| scopeBuild | 10.4 | 19.7 |
| serialise | 6.7 | 13.2 |
| total | 129.6 | 169.7 |

### concurrency 32 (2052 of 2052 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 134.8 | 271.6 |
| context | 0.2 | 2.1 |
| detail.engineTotal | 359.7 | 485.9 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 134.8 | 271.6 |
| detail.queryEmbed | 0.0 | 0.1 |
| detail.scopeCheck | 18.0 | 67.1 |
| detail.vectorArm | 134.8 | 271.6 |
| enrich | 0.0 | 0.1 |
| expand | 0.0 | 0.0 |
| figures | 47.7 | 118.5 |
| fusion | 0.0 | 0.0 |
| neighbours | 115.5 | 220.5 |
| rerank | 0.1 | 0.1 |
| scopeBuild | 50.8 | 85.9 |
| serialise | 8.0 | 14.6 |
| total | 422.1 | 552.4 |

