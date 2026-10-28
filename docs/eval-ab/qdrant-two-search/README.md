# Two Qdrant searches instead of four, and `KB_RRF_K` (2026-10-28)

**Change.** `kb-store-qdrant` used to send three queries per hybrid search: dense,
sparse, and a Qdrant RRF query whose two prefetches repeated both searches. Qdrant's
fused score was never used (the adapter recomputes it from each arm's rank); the
third query only picked which candidates got scored. Now the adapter sends dense and
sparse only, and fuses their union in process (`d4b6b02`). The RRF constant is
`KB_RRF_K` (default 60, `feb5c0e`).

**Run.** `miracl-id-200` (200 queries, 2,933 passages, all indexed, 0 failed, 0
degraded), `KB_TIER=lite` (`intfloat/multilingual-e5-small` on TEI CPU), kb-server
release pinned to 2 cores. `ab-base` = `d8560ad` (before), `ab-new60` = new code,
`ab-new2` = new code with `KB_RRF_K=2`.

| Run | nDCG@10 | MRR@10 | Recall@10 | Search stage (`arms`) p50 / p95 |
|---|---:|---:|---:|---:|
| ab-base | 0.7915 | 0.8081 | 0.9388 | 56.4 / 85.5 ms |
| ab-new60 | 0.7894 | 0.8052 | 0.9388 | **25.7 / 38.1 ms** |
| ab-new2 | 0.7920 | 0.8061 | 0.9391 | 19.8 / 27.8 ms (quieter machine, see below) |

Paired deltas (`score_deltas.py`, bootstrap 95% CI):

| Comparison | nDCG@10 | MRR@10 | Recall@10 |
|---|---|---|---|
| new60 − base | −0.0021 [−0.0090, +0.0051] | −0.0029 [−0.0142, +0.0092] | +0.0000 [+0.0000, +0.0000] |
| new2 − new60 | +0.0026 [−0.0091, +0.0148] | +0.0008 [−0.0165, +0.0183] | +0.0003 [−0.0103, +0.0111] |

**Reading.**
- Quality: no measurable change from dropping the third query (every CI spans 0;
  recall@10 is identical). k=2 vs k=60 makes no difference with this embedder, so
  60 stays the default.
- Latency: base and new60 ran under the same machine load (the stages this change does
  not touch match: query embed 44.1 vs 41.4 ms mean, neighbours 6.6 vs 6.8, scope 8.6
  vs 8.5), and the search stage fell by **54% at p50 and 55% at p95**. The new2 run
  happened on a quieter machine (every untouched stage is faster too), so its
  latency is not comparable to the other two.
- Client end-to-end p50: 119 → 81 ms (base → new60); this includes the CPU query
  embedding.
