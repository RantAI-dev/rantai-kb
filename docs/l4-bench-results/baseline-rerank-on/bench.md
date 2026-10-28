# Search load test — baseline-rerank-on

- target: `http://127.0.0.1:18080`
- harness commit: `a6664c5-dirty`
- timestamp: 2026-10-17T22:46:13.123068447+00:00
- queries cycled: 50
- max chunks per query: 8
- warm-up requests: 0 (discarded)

Closed loop: every worker holds one request in flight and sends the next when the
last answers. Latency is client-side (HTTP round trip plus JSON), so it is always at
least the server's own `total` below.

| concurrency | requests | errors | degraded | req/s | p50 ms | p95 ms | p99 ms | max ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2 | 0 | 2 | 0.04 | 22560.8 | 26050.6 | 26050.6 | 26050.6 |

2 requests, 0 error(s), 2 degraded.

Degrades at concurrency 1 (a 2xx answer with an arm down, not an error):
- rerank_unavailable × 2

HTTP statuses at concurrency 1: 200 × 2

## Server-side stage p95 (ms), from `stats.stageTimingsMs`

`detail.*` rows are overlapping measurements — the query embed and both arms happen
inside `arms` — and are read, not summed.

### concurrency 1 (2 of 2 responses reported timings)

| stage | p50 ms | p95 ms |
|---|---:|---:|
| arms | 18.7 | 21.6 |
| context | 0.1 | 0.1 |
| detail.engineTotal | 22544.4 | 26034.2 |
| detail.entityArm | 0.0 | 0.0 |
| detail.graphArm | 0.0 | 0.0 |
| detail.keywordArm | 18.7 | 21.6 |
| detail.queryEmbed | 2492.4 | 5976.1 |
| detail.scopeCheck | 2.5 | 3.4 |
| detail.vectorArm | 18.7 | 21.6 |
| enrich | 2.4 | 3.6 |
| expand | 0.0 | 0.0 |
| figures | 1.5 | 1.9 |
| fusion | 0.0 | 0.0 |
| neighbours | 14.8 | 16.4 |
| rerank | 20004.9 | 20004.9 |
| scopeBuild | 4.9 | 13.6 |
| serialise | 1.3 | 1.5 |
| total | 22560.8 | 26050.6 |

