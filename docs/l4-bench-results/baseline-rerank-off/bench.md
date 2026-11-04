# Search load test — baseline-rerank-off

- target: `http://127.0.0.1:18080`
- harness commit: `a6664c5-dirty`
- timestamp: 2026-10-17T22:44:34.886697761+00:00
- queries cycled: 50
- max chunks per query: 50
- warm-up requests: 5 (discarded)

Closed loop: every worker holds one request in flight and sends the next when the
last answers. Latency is client-side (HTTP round trip plus JSON), so it is always at
least the server's own `total` below.

| concurrency | requests | errors | degraded | req/s | p50 ms | p95 ms | p99 ms | max ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 444 | 0 | 0 | 14.80 | 68.0 | 81.5 | 87.0 | 109.3 |
| 8 | 1293 | 0 | 0 | 42.92 | 192.9 | 241.8 | 257.3 | 280.6 |
| 32 | 1826 | 0 | 0 | 60.40 | 537.6 | 672.8 | 734.2 | 891.9 |

3563 requests, 0 error(s), 0 degraded.

HTTP statuses at concurrency 1: 200 × 444

HTTP statuses at concurrency 8: 200 × 1293

HTTP statuses at concurrency 32: 200 × 1826

## Server-side stage p95 (ms), from `stats.stageTimingsMs`

`detail.*` rows are overlapping measurements — the query embed and both arms happen
inside `arms` — and are read, not summed.

### concurrency 1 (444 of 444 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 16.7 | 22.1 |
| context | 0.2 | 0.2 |
| detail.engineTotal | 54.6 | 65.7 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 16.7 | 22.1 |
| detail.queryEmbed | 0.1 | 0.1 |
| detail.scopeCheck | 1.9 | 3.8 |
| detail.vectorArm | 16.7 | 22.1 |
| enrich | 1.9 | 3.5 |
| expand | 0.0 | 0.0 |
| figures | 1.9 | 3.7 |
| fusion | 0.0 | 0.0 |
| neighbours | 29.7 | 39.0 |
| rerank | 0.1 | 0.1 |
| scopeBuild | 5.0 | 7.6 |
| serialise | 3.0 | 4.8 |
| total | 63.1 | 75.3 |

### concurrency 8 (1293 of 1293 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 30.6 | 49.8 |
| context | 0.2 | 0.9 |
| detail.engineTotal | 165.4 | 213.4 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 30.6 | 49.8 |
| detail.queryEmbed | 0.1 | 0.5 |
| detail.scopeCheck | 3.6 | 8.8 |
| detail.vectorArm | 30.6 | 49.8 |
| enrich | 3.6 | 8.6 |
| expand | 0.0 | 0.0 |
| figures | 5.9 | 15.3 |
| fusion | 0.0 | 0.0 |
| neighbours | 114.4 | 157.6 |
| rerank | 0.1 | 0.2 |
| scopeBuild | 8.8 | 16.2 |
| serialise | 5.0 | 10.0 |
| total | 181.2 | 229.8 |

### concurrency 32 (1826 of 1826 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 61.9 | 146.8 |
| context | 0.2 | 1.4 |
| detail.engineTotal | 489.5 | 611.3 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 61.9 | 146.8 |
| detail.queryEmbed | 0.1 | 0.3 |
| detail.scopeCheck | 7.6 | 25.4 |
| detail.vectorArm | 61.9 | 146.8 |
| enrich | 7.5 | 25.2 |
| expand | 0.0 | 0.0 |
| figures | 18.7 | 51.2 |
| fusion | 0.0 | 0.0 |
| neighbours | 373.5 | 487.9 |
| rerank | 0.1 | 0.2 |
| scopeBuild | 16.4 | 42.3 |
| serialise | 6.4 | 11.4 |
| total | 515.6 | 643.6 |

