# Lite tier on 2 CPU cores (2026-10-24)

**Result.** On 2 pinned cores, with the embedder capped at 1 thread, `KB_TIER=lite`
(no reranker, no page arm) scores **nDCG@10 0.802** on MIRACL-id-50 (Indonesian, 1,485
passages, 50 queries) with **multilingual-e5-small**, ingests the corpus in **~2 min** and
answers a search in **42 ms** (p50; p95 49 ms). Qwen3-Embedding-0.6B under the same
conditions scores 0.811 (−0.009 [−0.047, +0.027] for e5-small: a tie on 50 queries) but takes
**86 min** to ingest and **802 ms** per search. The Lite preset now uses e5-small.

| Lite, MIRACL-id-50 | nDCG@10 | Recall@10 | MRR@10 | ingest 1,485 passages | search p50 / p95 |
|---|---:|---:|---:|---:|---:|
| `intfloat/multilingual-e5-small` (384-d, 118M) | 0.802 | 0.938 | 0.823 | ~126 s | 42 / 49 ms |
| `Qwen/Qwen3-Embedding-0.6B` (1024-d) | 0.811 | 0.945 | 0.837 | ~86 min | 802 / 926 ms |

For scale: the GPU-hosted 0.6B scores 0.793 without a reranker on the larger `miracl-id-200`
(`docs/local-models.md`).

## Finding: the embedder must leave a core for the server

First attempt (ViDoRe HR, Qwen3-0.6B, embedder unrestricted on the same 2 cores): ingest used
both cores fully, kb-server stopped answering (`no HTTP response` on status polls, Postgres
calls "slow" at 10-60 s because the server got no CPU), and 12 of 14 documents failed. With
the embedder capped at 1 thread (TEI: `OMP_NUM_THREADS=1 RAYON_NUM_THREADS=1
MKL_NUM_THREADS=1`) the server stays responsive, but Qwen3-0.6B then embeds only ~0.2 long
chunks/s (163 HR chunks in 14 min; HR in full would take ~5 h), and its requests later
timed out. A 0.6B model is too heavy for Lite; a ~100M model is not.

## Setup and caveats

`lite.sh` (this directory): TEI CPU image (`text-embeddings-inference:cpu-latest`, ONNX for
e5-small) pinned with `--cpuset-cpus=0,1`, kb-server pinned with `taskset -c 0,1`, fresh
empty cache, `KB_TIER=lite`. Postgres, SurrealDB, Qdrant and S3 were **not** pinned, so a real
2-core box also pays for them; and the machine was shared with another project's test run
(load ~5-14), so timings are upper bounds rather than clean numbers. `KB_STACK_REMOTE_EMBED_QUERY_PREFIX="query: "` and
`_PASSAGE_PREFIX="passage: "` for e5 (added to `stack-up.sh` in `ddc0e6b`).

Not measured: ViDoRe HR on Lite (only the failed attempt above), a reranker on 2 cores, and
PP-OCRv6 for scans.
