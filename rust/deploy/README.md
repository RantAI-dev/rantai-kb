# Deploying the Rust service with one command

`rust/deploy/compose.yaml` runs a complete rantai-kb stack — Postgres, Qdrant,
S3 (rustfs), the models a tier needs, and the Rust `kb-server` — behind
one profile per deployment tier. `rust/scripts/kb-dev.sh` is the interface.

This is the Rust service. The root `docker-compose.yml` still describes the
frozen Bun service and is not used here; `rust/scripts/stack-up.sh` remains the
smoke/eval harness (it starts containers by hand, without compose).

**Status (2026-10-25):** the `lite-en` profile was run end to end on a 2-core-class Linux host:
`kb-dev.sh up lite-en --build` healthy in 3 min (image built from `rust/Dockerfile`), key created,
two PDFs uploaded and ingested, `/v1/search` returned the right pages with no degrades, a second
tenant's key saw 0 chunks and got 404 on the document, and `kb-server reindex` (dry run, then
twice for real) re-ingested both documents with the Qdrant point count unchanged (188 → 188).
`docker compose config` passes for desk, studio, station and cloud; those were not run (no NVIDIA
GPU here; cloud is paid).

## Quickstart

```sh
rust/scripts/kb-dev.sh setup                    # detect the hardware, write .env + .tier
rust/scripts/kb-dev.sh up lite-en               # CPU only; lite | lite-en | desk | studio | station | cloud
rust/scripts/kb-dev.sh key my-tenant            # prints a key — copy it
```

`setup` is the only command that needs no Docker: it reads the hardware (NVIDIA via
`nvidia-smi`, Apple silicon via `sysctl`, CPU/RAM), suggests a tier, compares it to the
VRAM estimates in [`docs/kb-tiers.md`](../../docs/kb-tiers.md), gates the models on
[`docs/model-licences.md`](../../docs/model-licences.md), and writes `rust/deploy/.env`
plus `rust/deploy/.tier` for `up` to read. It never starts, downloads or probes anything
unless asked, and it never overwrites an existing `.env` without `--overwrite`. See
[`kb-server setup`](#kb-server-setup) below.

The manual route still works: `cp rust/deploy/.env.example rust/deploy/.env` (optional;
only cloud needs a key).

`up` builds the image if it is missing, starts the stores, waits for `/health`,
and prints the URL. Then upload a file and search it:

```sh
export KB_URL=http://127.0.0.1:8080        # KB_PORT in .env if you changed it
export KB_KEY=<the key you just copied>

# 1. Upload: POST /v1/documents, multipart field `file` (+ optional title,
#    categories, knowledgeBaseIds). 202 means "accepted", not "indexed".
curl -sS -H "Authorization: Bearer $KB_KEY" \
  -F file=@rust/crates/kb-eval/fixtures/tiny/corpus/mission.pdf \
  "$KB_URL/v1/documents"
# {"id":"…","jobId":"…","status":"processing","title":"mission","knowledgeBaseIds":[]}

# 2. Wait for the ingest worker: GET /v1/jobs/<jobId> until status is "success"
#    ("failed" carries the error in .error).
curl -sS -H "Authorization: Bearer $KB_KEY" "$KB_URL/v1/jobs/<jobId>"

# 3. Search: POST /v1/search, body {"query": "...", "maxChunks": n}.
curl -sS -H "Authorization: Bearer $KB_KEY" -H 'content-type: application/json' \
  -d '{"query":"seven minutes of terror","maxChunks":5}' \
  "$KB_URL/v1/search"
# .chunks[].content, .documentTitle, .page, .similarity; .degrades is non-empty
# when an arm failed (a failed arm is reported, never silently dropped).
```

Other useful routes: `GET /health`, `GET /v1/documents`, `GET /v1/documents/:id`,
`GET /v1/documents/:id/raw`, `GET /v1/formats`, `GET /v1/events` (SSE ingest
progress), `GET /v1/jobs/:id`. A key with no `--scopes` may read and write.

For engine clients that map hits back to their own records:
`GET /v1/documents?externalRef=<ref>` filters the list to the caller tenant's
live documents with that exact `externalRef` (composable with `limit` and
`knowledgeBaseIds`), every document list row carries `externalRef`, and every
`/v1/search` chunk carries the `externalRef` of its document. `GET /health` and
`GET /v1/health` also report `tier` (the `KB_TIER` preset, or `null`) and
`egress`, the sorted hosts of configured model endpoints that are not local
(loopback, RFC1918, link-local, `fc00::/7`, or a single-label compose name);
`cloud` reports `["openrouter.ai"]`, `lite`/`lite-en` report `[]`.

Everyday commands:

```sh
rust/scripts/kb-dev.sh setup             # detect, budget/licence check, write .env
rust/scripts/kb-dev.sh setup --check     # probe the configured models and stores
rust/scripts/kb-dev.sh status            # GET /health
rust/scripts/kb-dev.sh ps                # containers for the current tier
rust/scripts/kb-dev.sh logs -f kb-lite-en
rust/scripts/kb-dev.sh down              # stop, keep data
rust/scripts/kb-dev.sh down -v           # stop and delete all data + volumes
```

The tier is remembered in `rust/deploy/.tier`, so the commands above need no
tier argument once `up` has run. `kb-dev.sh key` needs the stack running.

## `kb-server setup`

`rust/scripts/kb-dev.sh setup [flags]` runs `kb-server setup` from the repo root
(or the binary in `KB_SERVER_BIN`). It is a planner and a config writer, not a
launcher: Postgres, Qdrant, Docker and the models do not have to be running.

```sh
rust/scripts/kb-dev.sh setup --detect-only        # print detection + the plan, write nothing
rust/scripts/kb-dev.sh setup                      # write rust/deploy/.env and .tier
rust/scripts/kb-dev.sh setup --tier cloud --accept-licence voyageai/voyage-multimodal-3.5 \
  --accept-licence voyageai/rerank-2.5 --openrouter-key-env OPENROUTER_API_KEY
rust/scripts/kb-dev.sh setup --start              # write, then run `up <tier>`
```

What it does:

1. **Detects** each NVIDIA GPU (`nvidia-smi --query-gpu=name,memory.total`), Apple
   silicon (`uname` + `sysctl hw.memsize` + the CPU brand, unified memory), CPU
   cores and RAM (`/proc/meminfo`, `available_parallelism`). An AMD/other or
   virtual display is reported as an unsupported accelerator and treated as CPU.
2. **Suggests a tier**: NVIDIA ≥48 GB → station, ≥24 → studio, ≥8 → desk; Apple
   unified ≥128 → station, ≥32 → studio, ≥16 → desk; otherwise `lite`
   (`lite-en` with `--english`). `cloud` is only ever chosen explicitly, because
   it spends money on every page and query. With several NVIDIA cards the
   **largest** one is used: compose reserves one GPU per model container, so
   cards do not pool.
3. **Checks the budget** against the estimates in
   [`docs/kb-tiers.md`](../../docs/kb-tiers.md) (they are estimates, and the
   output says so). A tier that does not fit, or a desk/studio/station choice on
   a box with no NVIDIA GPU, is refused unless `--force`.
4. **Gates the licences** from [`docs/model-licences.md`](../../docs/model-licences.md):
   a 🔴 model is refused, a 🟡 one needs `--accept-licence <model>`, and the 🟢
   list is printed. A model with no table entry is refused too, so a new preset
   model cannot skip the review.
5. **Writes `rust/deploy/.env`** from `.env.example` and `rust/deploy/.tier`.
   Only operator-level keys are written (`KB_TIER`, `KB_PORT`, `KB_CACHE_DIR`,
   keys, the caption endpoint); the tier preset fills the model roles, so no
   preset value is duplicated into the file. Secrets are written empty unless
   `--write-secrets` is passed, and a value is never printed.
6. **Prints the next step** — `rust/scripts/kb-dev.sh up <tier>` — and runs it
   with `--start`.

`setup --check` loads the env file plus the process environment (process wins,
like compose) with the tier preset applied, then makes **one real request per
configured role**: an embedding of `ping`, a rerank of two documents, the page
embedder, the caption endpoint, `GET /v1/models` for OCR, and TCP/HTTP
reachability for Postgres, Qdrant and S3. It prints role, host (no credentials),
status and latency, and exits non-zero if a required role fails. A paid host
(`openrouter.ai`, `api.mistral.ai`, voyage, Cohere) is **skipped** unless
`--check-paid` is passed: a health check must not spend money.

Not built yet, and deliberately not pretended: **model download and start** are
done by the containers on first start (`up` waits up to 15 minutes for the TEI
healthchecks), not by `setup`; and `KB_PARSE_RESIDENCY` / `KB_CAPTION_RESIDENCY`
and the caption priority queue do not exist, so setup does not ask about them.

## Tiers

One profile per tier; `up <tier>` starts that profile's model containers plus
the shared stores. Only one tier runs at a time (they share the stores and the
host port). Chunks, vectors, BM25 and page images live in Qdrant on every tier;
Postgres holds documents, jobs, KBs, keys and the entity graph.

| Tier | Model containers | Hardware | Measured (see below) |
|---|---|---|---|
| `lite` | TEI CPU, `intfloat/multilingual-e5-small` (384-d); `ocr-ppocr` (PP-OCRv6, CPU) for scanned pages | CPU, 1–2 cores, no GPU | MIRACL-id-50 nDCG@10 **0.802**, recall@10 0.938, MRR@10 0.823; 1,485 passages ingest ~126 s; search p50 42 ms / p95 49 ms. ViDoRe HR: 1.55 pages/s at page nDCG@10 0.545 |
| `lite-en` | TEI CPU, `sentence-transformers/all-MiniLM-L6-v2` (384-d, English only); `ocr-ppocr` | CPU, 1–2 cores, no GPU | ViDoRe HR: **6.43 pages/s** at page nDCG@10 0.517. English only: 0.679 vs 0.802 for `lite` on Indonesian MIRACL-id |
| `desk` | TEI CUDA, `Qwen/Qwen3-Embedding-0.6B` (1024-d) + `BAAI/bge-reranker-v2-m3`; `ocr-paddlevl-desk` (PaddleOCR-VL-1.6 GGUF on llama.cpp CUDA) | NVIDIA 8 GB | **not measured on this hardware** — see `docs/kb-tiers.md` for estimates |
| `studio` | TEI CUDA Qwen3-Embedding-0.6B; vLLM: `vllm-page-embed-studio` (Qwen3-VL-Embedding-2B, 2048-d page images), `vllm-rerank-studio` (Qwen3-VL-Reranker-2B), `vllm-ocr-studio` (PaddleOCR-VL-1.6) | NVIDIA 24 GB | not measured; `docker compose config` only |
| `station` | as studio with the 8B Qwen3-VL models in FP8 (4096-d page images) | NVIDIA 48 GB | not measured; `docker compose config` only |
| `cloud` | none (OpenRouter: `voyage-multimodal-3.5` embeddings, `voyageai/rerank-2.5`); page-image arm on | any machine + `OPENROUTER_API_KEY` | ViDoRe HR page nDCG@10 **0.711** ([vidore3-hr-en-image](../../docs/eval-ab/vidore3-hr-en-image/)), ViDoRe CS **0.828** ([vidore3-cs-en-night](../../docs/eval-ab/vidore3-cs-en-night/)); **paid** per page and query |

The measured rows come from
[`docs/eval-ab/lite-2core`](../../docs/eval-ab/lite-2core/) (2 pinned cores; the
MIRACL run capped the embedder at one thread, the HR runs did not).
Every vLLM container claims `--gpu-memory-utilization` of the whole card; the
defaults (studio: OCR 0.12, page embed 0.2, rerank 0.2; station: 0.06 / 0.2 / 0.2
with FP8 weights) are estimates and can be changed in `.env` with
`KB_STUDIO_*_GPU_FRACTION` / `KB_STATION_*_GPU_FRACTION` (`OCR`, `PAGE_EMBED`,
`RERANK`). The image is `vllm/vllm-openai:v0.32.0` (`VLLM_IMAGE` overrides it);
set `HF_TOKEN` if your Hugging Face access needs one. The PP-OCRv6 sidecar on
lite uses one core (`KB_LITE_OCR_CPUS`, `KB_OCR_THREADS`) and only sees pages
without a text layer.

Tier definitions and the role each model plays are in
[`docs/kb-tiers.md`](../../docs/kb-tiers.md). GPU numbers are deliberately left
blank: none were run here.

What the numbers mean for the compose stack: lite/lite-en let the embedder and kb-server
each use up to 2 CPUs (`KB_LITE_EMBED_CPUS`, `KB_LITE_SERVER_CPUS`, `KB_EMBED_THREADS` in
`.env`), the setup the 2-core HR numbers were measured with. Embedding is ~95% of ingest, so
the embedder's CPU limit — not the disk or the databases — sets pages/s (e5-small scaled
4.9 → 9.7 → 17.1 chunks/s on 1 → 2 → 4 cores). A 0.6B embedder on the same 2 cores starved
the API; e5-small and MiniLM did not.

### What `lite-en` uses (measured 2026-10-26)

The 14 ViDoRe HR PDFs (1,110 pages, 51 MB, 4,931 chunks) were ingested through the compose
stack, followed by the 318 HR queries. `docker stats` was sampled roughly every 3 s
(CPU is % of one core):

| Container | Idle RAM | Ingest CPU avg / peak | Ingest RAM peak | Search CPU peak | Search RAM peak | Disk after ingest |
|---|---|---|---|---|---|---|
| tei-lite-en (MiniLM) | 188 MiB | 181% / 201% | 299 MiB | 113% | 298 MiB | 87 MB model |
| kb-lite-en | 4 MiB | 7% / 104% | 120 MiB | 52% | 120 MiB | — |
| qdrant | 46 MiB | 1% / 8% | 95 MiB | 139% | 208 MiB | 38 MB |
| rustfs (S3) | 90 MiB | 2% / 71% | 161 MiB | 5% | 134 MiB | 51 MB (originals) |
| postgres | 31 MiB | 6% / 19% | 40 MiB | 20% | 52 MiB | 49 MB |
| **Total** | **~360 MiB** | **~2 cores** | **~715 MiB** | | **~810 MiB** | **~225 MB + 2.5 GB images** |

- Ingest took 172 s for 1,110 pages, which is **6.45 pages/s**. The embedder used both of
  its cores the whole time; everything else stayed under 10% on average.
- Search with one client gave 30 queries/s (p50 32 ms, p95 45 ms) with 0 degraded and 0
  empty results. A second pass with 4 clients reached 126 queries/s, but it repeated the
  same queries, so the query-embedding cache made it look faster than it really is.
- Minimum: 2 CPU cores and 2 GB RAM (the stack peaks near 0.8 GiB, plus the OS), with 5 GB
  of disk. Recommended: 4 cores, so the API and Qdrant don't compete with the embedder.
  Pages/s grows with the embedder's cores.
- `lite` (e5-small), same corpus and method: 733 s for 1,110 pages (**1.51 pages/s**). The
  embedder used 916 MiB idle and peaked at 1,326 MiB, because e5-small's multilingual vocabulary
  is large. The whole stack peaked at about 1.8 GiB, and the other four containers looked like
  they do under `lite-en`. Search with one client: 22 queries/s, p50 44 ms, p95 60 ms, 0 degraded.
  Minimum: 2 cores and 4 GB RAM.

`lite-en` is only for English corpora. MiniLM is ~2.5× faster than e5-small
because it has half the layers; on Indonesian it loses 0.123 nDCG. For a mixed
or non-English corpus use `lite`.

## Changing a setting

Three layers, in order of preference:

1. **`rust/deploy/.env`** — read by compose automatically (a shell export wins
   over it). Everything a developer normally changes is there with its default
   and a comment: `KB_PORT`, store credentials, `KB_CACHE_DIR`, the CPU budget,
   the TEI image, `OPENROUTER_API_KEY`, and the local-model overrides below.
2. **`KB_DEV_OVERRIDE=/path/to/extra.yaml rust/scripts/kb-dev.sh up lite`** —
   a compose file layered on top of `compose.yaml`, for anything the `.env` does
   not cover (extra ports, resource limits, a sidecar).
3. **A `kb-server` setting that is not in the env file** — add it to the tier's
   `environment:` block in `compose.yaml`, or to an override file. Remember the
   tier rule: a variable that is *present* wins over the `KB_TIER` preset, even
   when it is empty; a variable that is *absent* takes the preset. Every setting
   the server reads is listed in [`rust/README.md`](../README.md).

Model containers are profile-gated, so a `.env` override applies to the local
tier you start. Do not keep an external-embedder URL in `.env` while running
`cloud` — cloud deliberately gets no local URLs (its preset points everything
at OpenRouter).

### Point a GPU tier at an external model server

The TIER presets expect embeddings at `http://127.0.0.1:8081/v1/embeddings` and
a reranker at `http://127.0.0.1:8082`. Inside compose the base URLs default to
the bundled TEI services, but `.env` wins:

```sh
# .env — llama.cpp (or vLLM, or another TEI) on an AMD box or a bigger GPU host
KB_EMBEDDING_BASE_URL=http://192.168.1.20:8081/v1/embeddings
KB_EMBEDDING_API_KEY=local
KB_RERANK_BASE_URL=http://192.168.1.20:8082/v1/rerank
KB_RERANK_PROVIDER=cohere
```

`KB_RERANK_PROVIDER=cohere` is llama.cpp's `/v1/rerank` (the Cohere request
shape) and the base URL is then the full endpoint. For a remote TEI keep
`KB_RERANK_PROVIDER=tei` and give the base URL without `/rerank`. Keep the tier's
model name and dimension (`Qwen/Qwen3-Embedding-0.6B`, 1024) unless you override
`KB_EMBEDDING_MODEL` / `KB_EMBEDDING_DIM` to match what the external server
actually returns.

The bundled `tei-embed-*` / `tei-rerank-*` containers still start (they are part
of the profile and health-gate `kb-server`). To free the GPU once the stack is
up:

```sh
docker compose -f rust/deploy/compose.yaml -p rantai-kb-dev --profile desk \
  stop tei-embed-desk tei-rerank-desk
```

## GPU notes (desk, studio, station)

- Needs the NVIDIA driver plus `nvidia-container-toolkit` on the host
  (`docker run --rm --gpus all nvidia/cuda:12.4.0-base-ubuntu22.04 nvidia-smi`
  should work). Compose asks for one GPU per model container via
  `deploy.resources.reservations.devices`; without the toolkit the containers
  fail with "could not select device driver".
- The TEI images here are the CUDA builds. For Turing-class cards use a
  `:<version>-turing` tag; for AMD, `:<version>-rocm` (and the compose GPU
  reservation must be replaced — TEI's ROCm image wants `/dev/kfd` and
  `/dev/dri`, or run it outside compose as described above).
- Desk is the tight tier: the embedder and the reranker both hold VRAM, and the
  app's chat LLM shares the card. If a model cannot load, lower
  `KB_RERANK_CANDIDATE_LIMIT` (the tier presets are 10/20/30) or use an external
  reranker.
- The first start downloads each model into the shared `hf-models` volume. That
  is minutes on a slow link, which is why the TEI healthchecks allow a 15-minute
  start period.

## Troubleshooting

**`up cloud` refuses to start.** It needs `OPENROUTER_API_KEY` in the shell or
`rust/deploy/.env`; cloud embeddings and reranking are billed.

**Port already in use.** Only kb-server publishes a host port. Set
`KB_PORT=18080` in `.env` (or the shell) and run `up` again; `status` and the
quickstart URL follow the same value.

**`/health` stays unavailable.** `kb-dev.sh up` tails the kb-server log on
timeout. Check `ps` and `logs`; the usual causes are a store that never became
healthy (Postgres unreachable, or Qdrant not ready) or a model container that
cannot download its model. `logs tei-lite` / `logs tei-embed-desk` shows the
download.

**I switched tiers and search returns nothing / errors.** The vector index width
is fixed when the stack is created (384 for lite/lite-en, 1024 for the rest). Wipe
and re-ingest: `kb-dev.sh down -v`, then `up <new-tier>`.

**`down` says no tier is recorded.** `rust/deploy/.tier` was removed or never
written because `up` failed validation; run `kb-dev.sh down <tier>` once.

**An old Bun stack is still running.** `kb-dev.sh` never touches containers
outside the `rantai-kb-dev` compose project. Stop the Bun stack with
`docker compose down` in the repo root before starting a Rust tier on the same
port.

**Everything, including volumes.** `kb-dev.sh down -v` removes only this
project's network and the five named volumes (`pg-data`, `rustfs-data`,
`qdrant-data`, `hf-models`, `kb-cache`). Use `docker volume ls
--filter label=compose.project=rantai-kb-dev` to see them first.
