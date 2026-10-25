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

## PDFs: ViDoRe HR on 2 cores, toward 5 pages/s (2026-10-24)

1,110 pages, 14 PDFs, `KB_TIER=lite`, no reranker, no page arm; embedder and kb-server pinned to
cores 0-1 (databases not pinned); machine quiet (load ~1.3). Ingest time is first "ingest
claimed" to last "ingest finished" in the server log.

| Setup | Ingest | **Pages/s** | Page nDCG@10 | vs full (95% CI) |
|---|---:|---:|---:|---|
| e5-small, full chunk (~234 tokens) | 715 s | 1.55 | **0.545** | — |
| e5-small, `KB_EMBEDDING_MAX_CHARS=512`, 2 requests in flight | 315 s | 3.52 | 0.524 | −0.021 [−0.041, −0.002] |
| same + `KB_CHUNK_OVERLAP=0` | 317 s | 3.50 | 0.507 | −0.038 [−0.061, −0.015] |
| e5-small, `KB_EMBEDDING_MAX_CHARS=384` | 260 s | 4.27 | 0.509 | −0.036 [−0.057, −0.016] |
| `minishlab/potion-multilingual-128M` (model2vec, static) | 21 s | 53 | 0.442 | −0.102 [−0.125, −0.080] |
| floor: instant fake embedder | 17 s | 65 | — | — |
| `sentence-transformers/all-MiniLM-L6-v2` (6 layers, English-only), full chunk | 282 s | 3.93 | 0.534 | −0.011 [−0.029, +0.008] |
| same, 2 requests in flight | 289 s | 3.85 | 0.536 | — |
| **all-MiniLM-L6-v2, `KB_EMBEDDING_MAX_CHARS=512`** | 173 s | **6.43** | 0.517 | −0.028 [−0.049, −0.007] |

### Other small models (2026-10-25)

Raw throughput on the same 2 pinned cores (`embed_bench.py`, 192 HR-sized chunks, batch 16,
2 in flight): all-MiniLM-L6-v2 **23.6** chunks/s, multilingual-e5-small 9.5, `BAAI/bge-base-en-v1.5`
3.7 (768-wide, 12 layers: slower than e5-small, so not run end to end).

MiniLM-L6 has half of e5-small's layers, so it is ~2.5x faster at the same quality on English
HR, and with the 512-char cap it clears **5 pages/s** (6.43). But it is English-only. On
Indonesian MIRACL-id-50 it scores **0.679 vs 0.802** for e5-small
(−0.123 [−0.187, −0.056]; run `lite-minilm-id50`). So it fits an English-only Lite deployment,
not the multilingual default. The Lite preset stays e5-small; an English deployment can set
`KB_EMBEDDING_MODEL=sentence-transformers/all-MiniLM-L6-v2`, empty prefixes and
`KB_EMBEDDING_MAX_CHARS=512`.

For scale: the fully local GPU text stack (Qwen3-0.6B + BM25 + bge reranker) scored 0.561 on HR.
e5-small full-chunk Lite, with no reranker, is 0.016 behind it.

What limits it (`ort_bench.py`, `embed_bench.py` in this directory):

- **Embedding is ~95% of ingest.** Everything else (pdfium text, chunking, BM25, Qdrant)
  ingests HR in 17 s.
- **e5-small runs at ~4.9 chunks/s per core** on this Xeon E5-2680 v4 (Broadwell, AVX2) and
  scales with cores (1: 4.9, 2: 9.7, 4: 17.1 chunks/s). Thread variables change nothing.
- **int8 does not help here**: the int8 ONNX file runs at the same speed as fp32 (9.2 vs 8.9
  chunks/s), because Broadwell has no VNNI. CPUs with AVX512-VNNI / AVX-VNNI (Ice Lake and newer
  Xeons, Zen 4) usually get 2-3x from int8; not measured here.
- **Cost is linear in tokens**: 234 tokens 9.2, 128 tokens 19.8, 96 tokens 27.3, 64 tokens
  41.5 chunks/s. Hence `KB_EMBEDDING_MAX_CHARS` (the dense vector sees a prefix; BM25 and the
  stored chunk keep the full text).
- `KB_CHUNK_OVERLAP=0` does not reduce the chunk count on HR (pages are chunked one by one), and
  costs quality: rejected.
