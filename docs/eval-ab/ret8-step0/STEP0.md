# RET-8 step 0 — teacher baselines on the GB10

Machine: ThinkStation PGX (NVIDIA GB10, arm64, 20 cores, 128 GB shared CPU/GPU).
Date: 2026-11-18 (evening). Everything below is from commands run on this machine;
raw `summary.json` files live under `~/ret8/out/`.

## 0. What was stopped to free the GPU, and how to restart it

| What | Was | Now | Restart |
|---|---|---|---|
| `xiaomi-vllm.service` (Xiaomi OCR vLLM, :8192) | our transient unit, idle, 4.3 GB GPU | stopped | `systemd-run --user --unit=xiaomi-vllm /home/rismana/.venvs/vllm-spark/bin/vllm serve /home/rismana/llmhost/bakeoff/models/xiaomi-ocr-0 --served-model-name xiaomi-ocr-0 --host 0.0.0.0 --port 8192 --api-key <key> --max-model-len 20480 --gpu-memory-utilization 0.15 --max-num-seqs 64 --limit-mm-per-prompt "{\"image\":1}" --quantization fp8 --kv-cache-dtype fp8 --mamba-ssm-cache-dtype bfloat16 --no-enable-prefix-caching --gpu-memory-utilization 0.04 --max-num-seqs 16 --kv-cache-memory-bytes 1073741824 --max-num-batched-tokens 8192 --max-model-len 16384 --mm-processor-kwargs "{\"max_pixels\":4194304}" --enforce-eager` (unit also set `VLLM_CACHE_ROOT=/home/rismana/llmhost/bakeoff/vllm-cache`, log `~/llmhost/bakeoff/logs/xiaomi-vllm.log`) |
| Qwen3-Embedding-0.6B vLLM, :8201 (pid 122963 + EngineCore child) | our server, in `session-1801.scope`, 3.9 GB GPU | killed (`kill 122963`) | `/home/rismana/.venvs/vllm-spark/bin/python -m vllm.entrypoints.openai.api_server --model Qwen/Qwen3-Embedding-0.6B --served-model-name qwen3-embed-0.6b --runner pooling --max-model-len 8192 --gpu-memory-utilization 0.05 --host 0.0.0.0 --port 8201 --api-key <key>` |
| `xwatch` | **no such unit/process exists on this machine** (checked `systemctl --user list-units --all`, `list-unit-files`, `ps aux`) | nothing to stop | — |

Not restarted at the end, per instructions. Left alone: the three owner `peregrine serve` processes on :18397–18399, all `clickhouse-server`, `t3code.service`, `ocr-layout.service` (still running), `~/peregrine` untouched.

GPU after freeing: 0 MiB used by compute (only Xorg 33 MiB / gnome-shell 16 MiB).

## 1. Serving the teacher (task 1) — and the reproduction gate

Weights downloaded fresh with `hf download nvidia/Nemotron-3.5-Embed-1B-BF16`
(the cached entry had no blobs; 3.1 GB `model-00001-of-00001.safetensors`). Served with:

```
systemd-run --user --unit=ret8-teacher /home/rismana/.venvs/vllm-spark/bin/vllm serve \
  nvidia/Nemotron-3.5-Embed-1B-BF16 --served-model-name nvidia/Nemotron-3.5-Embed-1B-BF16 \
  --dtype bfloat16 --max-model-len 32768 --chat-template-content-format openai \
  --gpu-memory-utilization 0.35 --host 127.0.0.1 --port 8210
```

`/v2/embed` proven before the eval: query/document text both 2048-d, page image 2048-d,
batches of 2 texts and 2 images OK, repeat request returns the identical vector.
GPU memory while serving: 11,399 MiB (EngineCore), i.e. ~11.4 GB of the shared 128 GB.

**Reproduction gate — ViDoRe V3 HR, `--mode both`, kind `nemo`, name `nemo35`
(318 queries, 1,110 pages, 1,731 qrels):**

```
  nemo35/text       n=318 nDCG@10 0.611  MRR@10 0.740  Hit@1 0.635  Recall@10 0.658
  nemo35/image      n=318 nDCG@10 0.626  MRR@10 0.747  Hit@1 0.642  Recall@10 0.668
  nemo35/one-space  n=318 nDCG@10 0.621  MRR@10 0.745  Hit@1 0.642  Recall@10 0.673
  Δ nDCG@10 nemo35/text − nemo35/image:        -0.015 [-0.032, +0.002]
  Δ nDCG@10 nemo35/text − nemo35/one-space:    -0.010 [-0.016, -0.005]
  Δ nDCG@10 nemo35/image − nemo35/one-space:   +0.004 [-0.011, +0.019]
```

Committed numbers: text **0.611**, image **0.626**, one-space **0.621**. Measured:
**0.611 / 0.626 / 0.621** — inside ±0.005 (in fact exact to 3 decimals). Gate passed;
`--batch 32` worked for multi-image batches (no fallback to 4/1 needed).

## 2. Teacher throughput on the GB10 (task 2)

`scripts/bench_teacher.py`, 200 ViDoRe HR pages, 8-page warm-up (not timed), batch =
concurrency, wall clock over 200 pages, `nvidia-smi` process table for memory:

| arm | concurrency | batch | wall (s) | pages/s |
|---|---:|---:|---:|---:|
| text | 1 | 1 | 6.13 | 32.65 |
| text | 8 | 8 | 3.96 | 50.56 |
| text | 32 | 32 | 3.95 | 50.59 |
| image | 1 | 1 | 38.16 | 5.24 |
| image | 8 | 8 | 37.19 | 5.38 |
| image | 32 | 32 | 37.72 | 5.30 |

GPU memory during all of the above: 11,399 MiB (`VLLM::EngineCore`; `nvidia-smi
--query-gpu=memory.used` prints `N/A` on this driver, the process table carries it).

Best settings: **text 50.6 pages/s at c≥8**, **image 5.38 pages/s at c8** — the GPU
is compute-bound past concurrency 8 (c32 adds nothing).

**Price of 300k pages (image + text, both arms, at the best settings):**
- text: 300,000 / 50.59 = 5,930 s = **1.65 h**
- image: 300,000 / 5.38 = 55,762 s = **15.5 h**
- total: **≈ 17.1 h** of GB10 time for the 300k-page cache (plus ~300k/50.6 s ≈ 1.7 h if
  the image+text "combined" arm is wanted instead of separately).

## 3. MIRACL-Vision, Indonesian and English (task 3) — main result

### Dataset stats (from the parquet files, inspected with pyarrow)

| lang | corpus pages | queries | qrels | corpus schema | images |
|---|---:|---:|---:|---|---|
| id | 23,842 | 603 | 937 | `_id` (str), `text`, `title`, `image_id` (str, unique, `i0..`), `text_downloaded_from_Wikipedia` | `images.parquet`: `file_name` (== corpus.image_id) → jpeg bytes; 6.6 GB |
| en | 42,971 | 447 | 623 | same + `text_obtained_via_ocr_from_screenshots` | 13.4 GB |

qrels ids are int64 while corpus/queries ids are strings — the converter
(`scripts/miracl_vision_to_ab.py`) normalizes everything to strings, keeps the corpus's
own ids, writes every corpus page (no subsampling). Images materialized by
`scripts/miracl_vision_images.py` as `pages/<_id>.jpg`, quality 90, native resolution
unless long side > 1568 px (none of the id/en pages needed downscaling: max long side
came in under 1568; the counters printed "0 downscaled"). Text length: id p50 375 /
p99 1466 chars (max 14,303), en p50 323 / p99 1306 (max 7,632).

### Prefixes and limits for the baselines

- Harness `openai` kind does **not** send `"truncate": true` (checked `_body`), so
  over-limit texts were cut in a converter for that model only, as instructed:
  `multilingual-e5-small` has a 511-position context (Peregrine `inspect`), so its pages
  variant `pages-e5.jsonl` cuts the *prefixed* text (`passage: ` + text) at 511 tokens
  with the model's own XLM-R tokenizer (`scripts/cap_pages_tokens.py`); 49 id / 59 en
  pages were cut. First attempt forgot the prefix (a 513-token input 400'd), the committed
  cut counts the prefix.
- `bge-m3` (8192 positions) and granite-r2 (32k positions) needed no cut; teacher uses
  `truncate: "END"`.
- e5 served with `|query: |passage: ` prefixes; bge-m3 and granite with none.
- **Peregrine 0.3.0 cannot run granite-embedding-97m-multilingual-r2**: `peregrine
  inspect` says `supported: no: pre-tokenizer "granite-embed-multi-97m"`. Latest release
  is v0.3.0 (checked GitHub), so granite was served with llama.cpp `llama-server`
  0.9.0-dev instead, CPU-only (`-ngl 0` + `CUDA_VISIBLE_DEVICES=` after it turned out
  that `-ngl 0` alone still allocates a 183 MiB CUDA buffer). Parity spot check vs the
  transformers F32 reference: cosine 0.999855 / 0.999859 / 0.999845 on three test
  strings (Q8 + kernel differences) — the llama-server tokenizer handles the GGUF.

### Results — Indonesian (603 queries, 23,842 pages)

| model | arm | nDCG@10 | MRR@10 | Hit@1 | Recall@10 |
|---|---|---:|---:|---:|---:|
| nemo35 (teacher, GPU) | text | **0.769** | 0.780 | 0.685 | 0.892 |
| nemo35 (teacher, GPU) | image | 0.654 | 0.671 | 0.557 | 0.782 |
| nemo35 (teacher, GPU) | one-space | 0.762 | 0.777 | 0.682 | 0.880 |
| bge-m3 (CPU, Q8) | text | 0.740 | 0.762 | 0.655 | 0.846 |
| multilingual-e5-small (CPU, Q8) | text | 0.711 | 0.735 | 0.640 | 0.823 |
| granite-97m-r2 (CPU, Q8, llama-server) | text | 0.671 | 0.691 | 0.585 | 0.792 |

Paired bootstrap (same-run, cached vectors):

```
  Δ nDCG@10 nemo35/text − bge-m3/text:    +0.030 [+0.015, +0.044]
  Δ nDCG@10 nemo35/text − e5-small/text:  +0.058 [+0.041, +0.076]
  Δ nDCG@10 nemo35/text − granite-r2/text: +0.098 [+0.080, +0.116]
  Δ nDCG@10 bge-m3/text − granite-r2/text: +0.068 [+0.050, +0.086]
```

**The teacher is clearly ahead of bge-m3 on Indonesian text, but not by much:**
+0.030 nDCG@10 with the 95% CI excluding 0. Its *image* arm (0.654) is **below all
text baselines** — on MIRACL-Vision's simple Wikipedia page renders the pixels carry
less signal than the text, and the one-space mix slightly *hurts* vs text alone
(Δ text − one-space = +0.007 [+0.002, +0.012], Δ image − one-space = −0.109).

### Results — English (447 queries, 42,971 pages)

| model | arm | nDCG@10 | MRR@10 | Hit@1 | Recall@10 |
|---|---|---:|---:|---:|---:|
| nemo35 (teacher, GPU) | text | 0.809 | 0.802 | 0.718 | 0.912 |
| nemo35 (teacher, GPU) | image | 0.735 | 0.731 | 0.631 | 0.862 |
| nemo35 (teacher, GPU) | one-space | **0.812** | 0.806 | 0.725 | 0.913 |
| bge-m3 (CPU, Q8) | text | 0.767 | 0.752 | 0.640 | 0.883 |
| multilingual-e5-small (CPU, Q8) | text | 0.729 | 0.718 | 0.604 | 0.864 |
| granite-97m-r2 (CPU, Q8, llama-server) | text | 0.716 | 0.707 | 0.600 | 0.847 |

Teacher self-deltas (one run): Δ text − image +0.073 [+0.051, +0.095];
Δ text − one-space −0.003 [−0.010, +0.003] (one-space ≈ text here);
Δ image − one-space −0.076 [−0.097, −0.056].

Paired bootstrap vs baselines (same-run, cached vectors):

```
  Δ nDCG@10 nemo35/text − bge-m3/text:    +0.042 [+0.022, +0.062]
  Δ nDCG@10 nemo35/text − e5-small/text: +0.079 [+0.058, +0.101]
  Δ nDCG@10 nemo35/text − granite-r2/text: +0.093 [+0.071, +0.115]
  Δ nDCG@10 bge-m3/text − granite-r2/text: +0.051 [+0.029, +0.073]
```

### Cross-language summary (nDCG@10, page-level, full corpora)

| | id text | id image | id one-space | en text | en image | en one-space |
|---|---:|---:|---:|---:|---:|---:|
| teacher nemo35 | 0.769 | 0.654 | 0.762 | 0.809 | 0.735 | 0.812 |
| bge-m3 (text) | 0.740 | — | — | 0.767 | — | — |
| e5-small (text) | 0.711 | — | — | 0.729 | — | — |
| granite-r2 (text) | 0.671 | — | — | 0.716 | — | — |

vs published numbers (Nemotron card): MIRACL-VISION **68.93** is the image-arm
average over 18 languages; our image arms measure **65.4 (id)** and **73.5 (en)** —
bracketing the published average from both sides, consistent with a language spread
around 68.93. Note we embed images at native resolution (≤1568 px) while the card used
the default 1,120-px longest edge. The card's ViDoRe V3 numbers for the 1B are image
58.02 / text 57.80 / image+text 58.82 across the whole V3 sweep; our ViDoRe-V3-HR
subset reproduces the committed 0.626/0.611/0.621 exactly.

**Conclusion on assumption 1 ("the teacher is good on Indonesian pages"):** on
MIRACL-Vision-id the teacher's *text* arm is the best system measured (+0.030 over
bge-m3, CI excludes 0), so distilling its text space is defensible; but its *image*
arm (0.654) loses to every text baseline, including the 33M-parameter e5-small — so
on this dataset the value of the teacher is in its text tower, not its pixels. For
"pages found by what they look like" (scans, charts) ViDoRe HR remains the case where
pixels win (0.626 vs 0.611 text, and 0.545 for today's Lite path); MIRACL-Vision's
clean Wikipedia renders are not that case.


## 4. Granite vs teacher on Indonesian text (task 4)

`~/ret8/data/miracl-id-200` is a chunks-style dataset (corpus/*.md one file per chunk,
`<doc>_<page>.md`, 2,933 chunks; 200 queries; 640 qrels). Converted with
`scripts/miracl_id_to_chunks.py` into the harness's `--chunks` format (chunk = retrieval
unit, scored at the document level via the qrels' bare doc ids; qrels `corpus-id`
`N.md` → `N`).

| dataset | model | nDCG@10 | MRR@10 | Hit@1 | Recall@10 |
|---|---|---:|---:|---:|---:|
| MIRACL-Vision-id (23,842 pages) | granite-r2 text | 0.671 | 0.691 | 0.585 | 0.792 |
| MIRACL-Vision-id | nemo35 text | 0.769 | 0.780 | 0.685 | 0.892 |
| miracl-id-200 (2,933 chunks) | granite-r2 text | 0.756 | 0.793 | 0.695 | 0.886 |
| miracl-id-200 (2,933 chunks) | nemo35 text | **0.829** | 0.837 | 0.755 | 0.962 |

Paired bootstrap on miracl-id-200 (same run, cached): Δ nemo35 − granite-r2 =
**+0.073 [+0.052, +0.095]**. On both Indonesian text datasets the teacher is well
ahead of granite-r2 with a CI excluding 0; granite is also behind bge-m3 on
MIRACL-Vision-id (+0.068 [+0.050, +0.086] for bge over granite). Granite's
Indonesian weakness is real, not an artifact of one dataset.

## 5. CPU parity run — Peregrine Q8 on ViDoRe HR page text (task 5)

GGUF `~/models/nemotron/Nemotron-3.5-Embed-1B-Q8_0.gguf`
(sha256 `27c8e82c48828017c29f16fa87eb8958b19d3e35d8f25d73bf51b61efb2ec298`, verified
identical). Served per spec: `peregrine serve -m <it> -t 12 -c 32768 --port 8311`.

```
  nemo35-q8-peregrine/query: 318 cached, 0 to embed
  nemo35-q8-peregrine/text:   800 cached, 290 to embed
  nemo35-q8-peregrine/text n=318 nDCG@10 0.611  MRR@10 0.739  Hit@1 0.632  Recall@10 0.658
```

Cache reuse worked exactly as intended (1,118 cached from the x86 run, 290 finished on
this machine's CPU). **nDCG@10 0.611 vs the GPU's text 0.611** — identical to 3 decimals.

Parity, cosine Peregrine Q8 vs GPU bf16 for the first 20 ViDoRe HR page texts (GPU
vector with `input_type=document`, i.e. the server applies `passage: ` itself;
Peregrine gets `passage: ` client-side; raw output in `out/parity-nemo35-q8.txt`):

```
mean cosine over 20 pages: 0.999871   min 0.999755
```

Above the 0.999 target from the plan.

## Units started for this work (all `--user`)

`ret8-teacher` (:8210), `ret8-bge-m3` (:8312), `ret8-e5-small` (:8313), `ret8-granite`
(llama-server :8314), `ret8-nemo35-q8` (:8311), one-shot: `ret8-mv-id-images`,
`ret8-mv-en-images`, `ret8-teacher-mv-id`, `ret8-teacher-mv-en`, `ret8-baselines-id`
and `ret8-baselines2-id` (both exited non-zero mid-script — see problems 4 and 5 —
their surviving steps were re-run directly), `ret8-baselines-en` (exited 0). All
stopped at the end; see the final section.

## Problems hit

1. First `huggingface-cli` download stalled (the CLI is `hf` in this venv; first model
   download froze at ~470 MB and had to be killed and restarted; the restart completed
   at ~100 MB/s).
2. `miracl-vision` images are keyed by `file_name` == corpus `image_id` (`i0`, `i1`,
   ...), not by the corpus `_id` the qrels use; the first image-materialization attempt
   guessed the wrong join column and was restarted. Counts: every corpus row has exactly
   one image, 23,842 id / 42,971 en, 0 images downscaled.
3. qrels ids are int64, corpus/queries ids strings (see above).
4. e5-small: the first pages cap forgot the `passage: ` prefix → a 513-token request
   400'd ("served with 511 positions"); the committed cap counts prefix + text ≤ 511
   tokens (49 id / 59 en pages cut, ~0.2%).
5. granite GGUF: unsupported by Peregrine 0.3.0 (pre-tokenizer), served with
   llama-server instead; it also 500'd on texts longer than its physical ubatch
   (587 then 2831 tokens, killing the scripted granite step of both baseline units)
   → llama-server restarted with `-b 8192 -ub 8192` and granite re-run directly;
   llama-server also had to be pinned off the GPU (`CUDA_VISIBLE_DEVICES=`) because
   `-ngl 0` alone still holds a 183 MiB CUDA context.
6. `nvidia-smi --query-gpu=memory.used` returns `N/A` on GB10; memory readings come from
   the process table.

## Not run

- The e5-small arm on ViDoRe HR and the baselines' *image* arms — not asked for
  (baselines are text-only by design; e5/bge-m3/granite have no image tower).
- The 8B teacher, NVFP4 checkpoint, and any image+text "combined" (`--mode combined`)
  arm on MIRACL-Vision — not asked for in step 0.
- `time_pages.py`-style per-page CPU timings on this box for the baselines (the plan's
  table already carries the Xeon numbers; only the Q8 parity run was required here).
- No training/step-1+ work: out of scope for this task.

## Final state (cleanup)

All `ret8-*` units stopped at the end (list below); GPU verified free with
`nvidia-smi` (only Xorg 33 MiB / gnome-shell 16 MiB), and
`systemctl --user list-units 'ret8-*'` shows none running. The owner's processes
(peregrine :18397–18399, clickhouse-servers, t3code.service, ocr-layout.service)
were never touched. Old servers (xiaomi OCR :8192, Qwen3-Embedding :8201) were **not**
restarted, per instructions — see section 0 for their restart commands.

Left in place: results under `~/ret8/out/` (summary.json + per-query TREC files for
every run named above, plus `parity-nemo35-q8.txt`), caches under `~/ret8/cache/`
(`nemo35.jsonl`, `nemo35-q8-peregrine.jsonl`, `bge-m3.jsonl`, `e5-small.jsonl`,
`granite-r2.jsonl`), converted datasets under `~/ret8/data/miracl-vision-{id,en}/`
(with materialized `pages/` JPEGs) and `~/ret8/data/miracl-id-200-chunks/`, scripts
under `~/ret8/scripts/`, downloads under `~/ret8/logs/`. The HF caches hold the
teacher weights and both miracl-vision languages (19.7 GB of parquet).

