# DEP-7 — the one-embedder default, measured on the GB10 (2026-11-24/25)

**Status.** (A) ingest throughput, (B) one Studio-shaped end-to-end run and (C) memory are
measured on real hardware. (D) is optional and is reported separately below. No paid API
call was made. Every number in this file was produced by a command whose output is kept
under `~/dep7/` and quoted below.

## Hardware and environment

Raw outputs:

```text
$ date -u
Tue Nov 24 16:48:11 UTC 2026

$ free -g
               total        used        free      shared  buff/cache   available
Mem:             121          51          55           0          16          70
Swap:             16           4          11

$ nvidia-smi --query-gpu=name,memory.total,memory.used,compute_cap --format=csv,noheader
NVIDIA GB10, [N/A], [N/A], 12.1

$ nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader
4370, /app/llama-server, 5342 MiB
11253, VLLM::EngineCore, 1868 MiB
12418, VLLM::EngineCore, 10385 MiB
12911, VLLM::EngineCore, 14023 MiB

$ nproc
15
$ df -h /
/dev/nvme0n1p2  938G  817G   73G  92% /
```

ThinkStation PGX, NVIDIA GB10 (arm64, compute capability 12.1, 128 GB **unified** memory —
`nvidia-smi` reports `memory.total` as `[N/A]` for this card); 20 physical cores, `nproc`
sees 15. The box is shared: the other three GPU processes and the containers listed in
`docker ps` were left untouched throughout.

The embedder was the **already running** service, not one started for this benchmark:

```text
$ systemctl --user status ret8-phase0-nemo35.service
... ExecStart=/home/rismana/.venvs/vllm-spark/bin/vllm serve nvidia/Nemotron-3.5-Embed-1B-BF16
    --served-model-name nvidia/Nemotron-3.5-Embed-1B-BF16 --dtype bfloat16 --max-model-len 32768
    --chat-template-content-format openai --gpu-memory-utilization 0.35 --host 127.0.0.1 --port 8210
```

It was never restarted. `free -g` showed 66–70 GB available before and during the run, so
no extra model server was needed or started.

## Checkout and build

The checkout's `origin` pointed at `/tmp/feat-tables.bundle`, which does not exist, so the
literal `git fetch origin` failed with `fatal: '/tmp/feat-tables.bundle' does not appear to be
a git repository`. The same repository has a valid GitHub remote in another local clone;
`origin` was repointed to it and the branch was created from the real `origin/main`:

```sh
cd /home/rismana/work/rantai-kb
git stash -u || true
git remote set-url origin https://github.com/RantAI-dev/rantai-kb.git
git fetch origin
git checkout -B bench/dep-7 origin/main     # 79230443 (Merge PR #44, v1.0 roadmap)
```

```sh
export CARGO_TARGET_DIR=$HOME/kb-target-dep7 CARGO_BUILD_JOBS=8
cargo build --release --manifest-path rust/Cargo.toml -p kb-server
# Finished `release` profile [optimized] target(s) in 10m 35s
```

`rust/.vendor/pdfium/lib/libpdfium.so` was already present; no download was needed.

## (A) Cold ingest with the real Nemotron

**Document.** The 943-page `Introduction_to_Computer_Science.pdf` of
`docs/eval-ab/ingest-big-pdf` is **not on this box**: the corpus file is a dangling symlink
(`rust/crates/kb-eval/sets/structural/corpus/Introduction_to_Computer_Science.pdf ->
../../../datasets/vidore3-cs-en/corpus/...`, target absent). Per the task I used another
public 300+ page PDF, downloaded once to `~/dep7/principles-diffusion.pdf`:

* **The Principles of Diffusion Models**, arXiv:2510.21890,
  `https://arxiv.org/pdf/2510.21890` — **522 pages**, 19,745,365 bytes.
  `pdfinfo` and `sha256sum` output is in `~/dep7/results/`.

**Method.** `rust/scripts/bench-real.sh` (new, reusable) builds a BEIR-shaped dataset from
the PDF, starts a fresh `dep7-*` stack through `rust/scripts/stack-up.sh` (Postgres 16,
Qdrant 1.19.2, RustFS, release `kb-server`), and sends **both** roles to the existing vLLM
with the Studio one-embedder wire shape:

* text: `POST /v2/embed`, `input_type=query|document`, `inputs:[{"content":[{"type":"text",…}]}]`,
  `embedding_types:["float"]`, `truncate:"END"` — `KB_EMBEDDING_FORMAT=omniembed`;
* page images: the same endpoint with `{"content":[{"type":"text",…},{"type":"image_url",…}]}`
  — `KB_PAGE_EMBEDDING_FORMAT=omniembed`, 4 per request;
* `KB_TIER=studio`, `KB_EMBED_CONCURRENCY=4`, `KB_PAGE_EMBEDDING_CONCURRENCY=4`,
  `KB_PAGE_IMAGE_MAX_PAGES=2000`, empty disk cache, rerank and OCR off for this throughput
  run. The server and the model name the same checkpoint, at 2048-d.

A small counting proxy (`rust/scripts/embed_inflight_proxy.py`) sits in front of `:8210`
and logs one line per embedding request: time, text/page, number of inputs, requests in
flight at arrival, HTTP status and latency. It caches nothing and starts no model.

Endpoint probes (before the run):

```text
$ curl .../v2/embed  (text, input_type=document)  -> HTTP 200, 1 vector, dim 2048
$ curl .../v2/embed  (page image, data:image/png;base64, + text) -> HTTP 200, dim 2048
```

**Exact run command:**

```sh
KB_BENCH_PDF=$HOME/dep7/principles-diffusion.pdf LABEL=real-a \
KB_BENCH_OUT=$HOME/dep7/results BIN=$HOME/kb-target-dep7/release/kb-server \
systemd-run --user --unit dep7-ingest-a --property=WorkingDirectory=/home/rismana/work/rantai-kb \
  /bin/bash -lc 'rust/scripts/bench-real.sh > $HOME/dep7/results/bench-real-a.out 2>&1'
```

### (A) result

| pages | chunks | page images | ingest (claimed → finished) | pages/s | embedding requests | max in flight | failed / degraded pages |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 522 | 1383 | 522 | **257.44 s** | **2.03** | 177 (46 text + 131 page), all HTTP 200 | **8** (4 text + 4 page) | 0 |

Raw evidence (server log, ANSI stripped):

```text
2026-11-24T17:03:12.946422Z  INFO kb_engine::ingest: ingest claimed job_id=1d7ba3d3-3992-49f7-8ddd-d3af79a6aae3 filename=principles-diffusion.pdf
2026-11-24T17:07:30.378666Z  INFO kb_engine::ingest: page images indexed pages=522
2026-11-24T17:07:30.388533Z  INFO kb_engine::ingest: ingest finished job_id=1d7ba3d3-3992-49f7-8ddd-d3af79a6aae3 outcome=Ready { chunks: 1383 }
```

The only other log line matching `fail|degrad|error|warn` was an unrelated sqlx warning
(`ignoring unrecognized connect parameter key=schema value=public`); the eval report records
`n_failed: 0, n_degraded: 0, n_ingest_failed: 0`. The `page nDCG@10 0.000` in that eval output
is from the harness's single throwaway query (its qrel points at page 1, a title page) and
is **not** a retrieval claim.

Proxy log summary (computed from the 177 lines, all `status=200`):

```text
requests 177 kinds {'text': 46, 'page': 131} statuses {200: 177} max_inflight 8
text n 46 mean_s 7.787 p50 9.022 max 12.564
page n 131 mean_s 7.201 p50 6.834 max 11.153
```

For comparison only — **not re-measured here** — the old harness with a *fake* embedder
(2.4 s/request latency) took 71 s for the 943-page book
([`ingest-big-pdf`](../ingest-big-pdf/README.md)). The numbers above are a different PDF,
a different client and a real GPU model, so they are not a speed-up claim.

## (B) One Studio-shaped end-to-end run

**The full compose preset was not started.** `docker compose -f rust/deploy/compose.yaml
--profile studio --profile studio-one --profile studio-ocr --profile studio-vision config`
passes (rc=0), but running it would start four extra GPU model services (`vllm-embed`,
`llama-rerank`, `vllm-ocr-studio`, `vlm-gpu`) that are not `dep7-*` units, some with a GPU
fraction above the 0.15 safety cap, and it cannot use the existing embedder anyway:

```text
$ ss -ltnp | grep ':8210'
LISTEN 0 2048 127.0.0.1:8210 ... users:(("vllm",pid=11079,fd=42))
$ curl --max-time 6 http://172.17.0.1:8210/v1/models
curl: (7) Failed to connect to 172.17.0.1 port 8210 after 0 ms: Couldn't connect to server
```

The model server binds loopback only and the compose file has no `extra_hosts:
host.docker.internal:host-gateway`; the compose default is
`http://vllm-embed:8000/v2/embed`, i.e. a new container. The reranker service has no host
port (`docker port ugm-lab-kb6-rerank-1` → empty); the OCR endpoint `127.0.0.1:8083` and a
local vision model are absent. Nothing was started or stopped to work around this.

**What was run instead.** The repo's stack harness (`stack-up.sh`, the documented smoke/eval
stack) with `KB_TIER=studio` and the existing `:8210` Nemotron serving **both** text and
pages, on its own `dep7b-*` containers and ports. The document is a **1-page searchable
scanned textbook page** (`~/dep7/searchable-scan-page-20.pdf`, extracted with `pdfseparate`
from `~/ugm-lab/out/searchable/c6243749-….pdf`; its printed answer is "Jadi, dua suku
berikutnya adalah 95 dan 191"). An image-only scan with no text layer fails in this
configuration without an OCR provider — the exact error was recorded in
`~/dep7/results/studio-e2e.out`: `OCR failed for "scan-page-20.pdf": the PDF has no readable
text`. `ret8`'s own searchable version of the same scan was therefore used.

Raw curl outputs (full file `~/dep7/results/studio-e2e-2.out`):

```text
=== 1. GET /health ===
{"ok":true,"checks":{"postgres":true,"vectors":true},"version":"dev","tier":"studio","egress":[],"backends":{"embedder":{"state":"ready","detail":"HTTP 405 Method Not Allowed"},"reranker":{"state":"unavailable","detail":"not configured"},"ocr":{"state":"unavailable","detail":"error sending request for url (http://127.0.0.1:8083/v1/chat/completions)"}}}

=== 2. POST /v1/documents (scanned page) ===
{"id":"88bb6cf5-5980-4cf6-86f6-95cc6e4fb5aa","jobId":"4186a92d-b179-464f-b904-349d49934d19","status":"processing","title":"Scan of pattern page","knowledgeBaseIds":[]}

=== 3. GET /v1/jobs/<jobId> until success ===
{"id":"4186a92d-b179-464f-b904-349d49934d19","status":"pending","step":"queued",...}
{"id":"4186a92d-b179-464f-b904-349d49934d19","status":"success","step":"done","progress":100,"stepCurrent":null,"stepTotal":null,"etaSeconds":0,"attempt":1,"error":null,"documentId":"88bb6cf5-5980-4cf6-86f6-95cc6e4fb5aa"}

=== 4. GET /v1/documents/<id> ===
... "status":"ready", "chunkCount":1, "figures":[], "pages":1, "charsExtracted":635, "degrades":[] ...

=== 5. POST /v1/search ===
{"query":"Apa dua suku berikutnya dari pola barisan 5, 11, 23, 47?","maxChunks":5}
-> chunks[0].content ends "Jadi, dua suku berikutnya adalah 95 dan 191.",
   chunks[0].page = 0, retrieval.arms = ["dense","sparse","page"],
   stats.stageTimingsMs.detail.pageArm = 40.101329 ms,
   vectorResults = 1, totalResults = 1
```

`/health` reports `tier:"studio"`, `egress:[]` (no paid host), and the embedder ready. The
page arm ran on the same Nemotron endpoint and the answer on that specific page was
returned first. The reranker and OCR backends were **not** configured in this run — see
"not measured" below.

## (C) Memory during (A)

`~/dep7/mem-sample.sh` ran as `dep7-mem` and appended a line every 5 s:
UTC time, `free -g`, the whole `nvidia-smi --query-compute-apps` table and the `kb-server`
RSS. 48 samples fall inside the ingest window (17:03:12Z–17:07:31Z).

| process | measured peak | how |
|---|---:|---|
| Nemotron vLLM EngineCore, pid 12418 (parent pid 11079, `:8210`) | **10,387 MiB** GPU | `nvidia-smi --query-compute-apps=pid,used_memory` |
| kb-server, pid 701671 | **453,524 KiB (442.9 MiB)** RSS | `ps -o rss=` |
| host `available` memory | 69–70 GB | `free -g` |

kb-server does not appear in the GPU process table (it is CPU-only); its RSS is the relevant
memory figure. Raw lines:

```text
2026-11-24T17:03:12Z avail_g=69 used_g=52 model_pid=12418 model_mib= 10387 kb_pid=701671 kb_rss_kb=62268 gpu_all=4370, 5342 MiB;11253, 1868 MiB;12418, 10387 MiB;12911, 14023 MiB;
2026-11-24T17:06:43Z avail_g=70 used_g=51 model_pid=12418 model_mib= 10387 kb_pid=701671 kb_rss_kb=453072 gpu_all=4370, 5342 MiB;11253, 1868 MiB;12418, 10387 MiB;12911, 14023 MiB;
```

## (D) Page reranker on the one-space list

The ViDoRe v3 HR corpus **is** available on this box (`~/ret8/data/vidore3-hr-en`: 318
queries, 14 PDFs, `qrels/test.tsv` with 1,731 graded rows), so this optional item was
attempted. Input list: `~/ret8/out/vidore-hr-nemo35/nemo35.one-space.trec` — the already
measured Nemotron one-space run. The reranker is the **text-only** `Qwen3-Reranker-0.6B`
already serving in the `ugm-lab-kb6-rerank-1` container (reached at `172.18.0.7:8082`;
**not** stopped or restarted), applied by the repo's
`rust/crates/kb-eval/scripts/page_rerank.py` to the top 20 pages' *text*, blended with the
one-space order by a weight `w` and RRF constant `k` chosen by even/odd-query
cross-validation. Page text came from the repo's `extract_dump` example on the same 14
PDFs. This is a text cross-encoder over page text — **not** a multimodal page-image
reranker — so it does not settle the ≈0.67 expectation in `specs/dep-6.md`.

Exact command (`~/dep7/run-d.sh`, run as `dep7-rerank`):

```sh
python3 -I rust/crates/kb-eval/scripts/page_rerank.py \
  --qrels /home/rismana/ret8/data/vidore3-hr-en/qrels/test.tsv \
  --queries /home/rismana/ret8/data/vidore3-hr-en/queries.jsonl \
  --page-text-dir ~/dep7/hr-page-text \
  --run /home/rismana/ret8/out/vidore-hr-nemo35/nemo35.one-space.trec \
  --url http://172.18.0.7:8082/v1/rerank --key '<local reranker key>' \
  --model Qwen/Qwen3-Reranker-0.6B --depth 20 --batch 10 --workers 2 \
  --cache ~/dep7/hr-rerank-cache --out ~/dep7/results/hr-one-space-rerank.trec
```

Raw output:

```text
fold 0: w=0.3 k=1 train=0.646 test=0.620
fold 1: w=0.2 k=1 train=0.623 test=0.646
run 0.629 | rerank only 0.545 | CV blend 0.633
delta rerank-only vs run: -0.084 [-0.112, -0.057]
delta CV blend vs run:    +0.004 [-0.002, +0.011]
wrote /home/rismana/dep7/results/hr-one-space-rerank.trec with full-data w=0.2 k=1 (not held-out)
```

| run | page nDCG@10 |
|---|---:|
| one-space input list (this qrels) | 0.629 |
| rerank only (text cross-encoder on page text) | 0.545 |
| CV blend of the two lists | **0.633** (+0.004 [−0.002, +0.011]) |

With this qrels/scorer, `kb-server eval score --qrels …/qrels/test.tsv --run
…/nemo35.one-space.trec` also reports page nDCG@10 **0.629** [0.596–0.660]. `ret8`'s
`out/vidore-hr-nemo35/summary.json` quotes **0.621** for the same run — a different qrels /
scoring path, not a new measurement. The expected ≈0.67 from a *multimodal* page reranker
was **not** reproduced: the measured text reranker hurts on its own and the held-out blend
is inside its CI.

## What was not measured, and why

* **The 943-page `ingest-big-pdf` PDF.** Its corpus file is a broken symlink; no local copy
  exists. A 522-page open arXiv book was substituted and named above. A direct A/B against
  the old 71 s fake-embedder number is therefore not claimed.
* **The 0.6B-text-model comparison for ingest.** No Qwen3-Embedding-0.6B server was started
  (safety rules); (A) reports only the real Nemotron's throughput.
* **VRAM on an emulated 8 GB Desk card.** Not attempted: the GB10 has 128 GB unified memory
  and no way to reserve 8 GB for a second process without starting extra model servers.
* **The full compose `studio` preset** with its own vLLM embedder, llama.cpp reranker,
  Xiaomi OCR and VLM sidecars — see (B) for the exact blocker. `/health` in the run that
  succeeded reports `reranker: unavailable` and `ocr: unavailable`, so no rerank-quality or
  OCR-quality number is claimed from (B).
* **The built-in 405 on `/health`** for the embedder ("HTTP 405 Method Not Allowed") is the
  harness's probe detail, not a failed embedding: all 177 real requests returned 200, and
  both roles embedded successfully.
* **A multimodal page reranker (the actual item 1).** The only local reranker reachable
  without starting a new model server is a text cross-encoder; (D) measures that on page
  text, not a VL reranker on page images. The `voyageai/rerank-2.5` and
  `Qwen3-VL-Reranker-2B/8B` numbers in `specs/dep-6.md` were not re-run.
* **Anything on a paid endpoint.** No OpenRouter, voyage or Mistral call was made.
