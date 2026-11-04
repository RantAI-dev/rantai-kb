# Deployment tiers

Decided with the user, 2026-10-23. Model licences are in `docs/model-licences.md`; only 🟢
(Apache-2.0 / MIT) models are defaults, and a 🟡 model needs an explicit opt-in.
**Every VRAM and speed figure below is an estimate from model sizes unless marked
"measured".** Each tier's numbers have to be measured on its real hardware before it ships.

## Why tiers

The same kb-server binary runs on a 2-core VPS and on a DGX Spark. What changes is which
model serves each *role* (parse, embed, rerank, caption), where it runs, and whether it stays
loaded. On every local tier the GPU is **shared with the app's chat LLM, and chat comes
first.**

## The tiers

| Tier | For | Hardware | OS |
|---|---|---|---|
| **Lite** | tiny servers | CPU only, 1–2 cores, no GPU | Linux |
| **Desk** | small office, ~5 users | NVIDIA 8 GB, or Apple silicon 16 GB | Linux / macOS |
| **Studio** | serious single GPU | NVIDIA 24 GB, or Apple silicon 32–36 GB | Linux / macOS |
| **Station** | team server | NVIDIA 48 GB, or 128 GB unified (DGX Spark, Mac Studio) | Linux / macOS |
| **Cloud** | no local hardware | API keys (OpenRouter, Mistral, Voyage, Cohere) | any |

No Windows on any tier. NVIDIA is served by **vLLM**, Apple by MLX / llama.cpp Metal. The
chat LLM on Desk and Studio is Bonsai, which needs **PrismML's llama.cpp fork** (CUDA /
Metal); that is accepted for the chat model only.

## What each tier runs

| Role | Lite | Desk (8 GB) | Studio (24 GB) | Station | Cloud |
|---|---|---|---|---|---|
| Chat LLM (the app's) | elsewhere | Bonsai-2-9B (2.0–2.6 GB + 0.6 GB vision), 5 users | Bonsai-2-27B (6.0–7.2 GB + 0.6 GB vision), 5 users | 32–35B × 5 users, or 8–9B × 20 users | API |
| Embedding (text + page images, one space) | multilingual-e5-small on CPU (measured: `docs/eval-ab/lite-2core`) | Qwen3-Embedding-0.6B, text only (decided 2026-10-26) | Qwen3-VL-Embedding-2B, full precision | Qwen3-VL-Embedding-8B | voyage-multimodal-3.5 or gemini-embedding-2 |
| Reranker | tiny text reranker, or none | bge-reranker-v2-m3 (text) | Qwen3-VL-Reranker-2B, full precision | Qwen3-VL-Reranker-8B | voyage rerank-2.5 / cohere rerank-4 / qwen3-reranker-8b |
| Digital PDF text | pdfium text layer | text layer | text layer | text layer | text layer |
| Scans / complex pages | PP-OCRv6 (ONNX, CPU) | PaddleOCR-VL-1.6, loaded during ingest | PaddleOCR-VL-1.6, resident | MinerU2.5-Pro / PaddleOCR-VL | Mistral OCR |
| Charts / figures | small VLM on figure crops, background queue | chat LLM's vision, idle time | chat LLM's vision | chat LLM's vision | Gemini Flash |
| Page-image search | optional tiny model (colSmol-256M), off by default | off (text-only embedder) | yes | yes | yes |
| Est. VRAM chat / KB | — | ~3.5 / ~4.5 GB (tight) | ~9 / ~10 GB | ~26 / ~20 GB on 48 GB | — |

### Lite: cheapest tool per page

On 1–2 cores no model is fast enough to run on every page, so ingest routes **each page**:

1. Text layer present → pdfium text. Free.
2. No text layer (scan) → PP-OCRv6 (1.5–34.5M params; vendor: 0.20–1.40 s per image on Xeon).
3. Tables in digital PDFs → rebuilt from the text layer's word positions. No model.
4. Figures → a small VLM on the crop, in a low-priority background queue, capped per
   document. The document is searchable before its captions arrive.
5. Page images → thumbnails for display. Embedding them is optional; the 0.25B page models
   score ~0.46–0.47 on ViDoRe HR, about our text level, so they only stay if fusion helps.

### Desk: the tier with no slack

| Item | VRAM (estimate) |
|---|---:|
| Bonsai-2-9B + vision | 2.6–3.2 GB |
| KV cache, 5 users (hybrid attention, 8 of 32 layers keep KV) | ~0.5–1 GB |
| Qwen3-Embedding-0.6B fp16 (TEI CUDA, incl. runtime) | ~2 GB |
| bge-reranker-v2-m3 fp16 (TEI CUDA, incl. runtime) | ~1.7 GB |
| **Total** | **~6.8–7.9 GB** |

Desk keeps the text-only Qwen3-Embedding-0.6B (decided 2026-10-26): it is what the compose
profile ships, it runs on TEI, and it leaves ~0.7 GB more slack than the 2B multimodal pair.
The cost is no page-image search on Desk; Studio and Station keep the multimodal plan.

If it doesn't fit when measured: reranker at 6-bit first, then fewer rerank candidates. The
parser does not stay resident unless the operator asks for it.

## Local page embedder (vLLM)

The page-image arm talks to vLLM's **Chat Embeddings API** when
`KB_PAGE_EMBEDDING_FORMAT=vllm`. The shape is a chat conversation per page, not OpenAI's
`input` array: a page is `messages: [{"role":"user","content":[{"type":"text",...}, {"type":
"image_url","image_url":{"url":"data:image/jpeg;base64,..."}}]}]`, and vLLM returns one
vector for the whole conversation. Pages cannot be batched in this shape, so kb-server sends
one request per page and runs `KB_PAGE_EMBEDDING_CONCURRENCY` of them at a time; queries are
one text-only message. No `input_type` is sent — the model's chat template carries the
instruction.

Serve the 2B model (the model card's command; `--max-model-len 8192` leaves room for the
image tokens of a 96 dpi page):

```sh
vllm serve Qwen/Qwen3-VL-Embedding-2B --runner pooling --max-model-len 8192
```

Point kb-server at it:

```sh
KB_PAGE_EMBEDDING_FORMAT=vllm
KB_PAGE_EMBEDDING_BASE_URL=http://host:8000/v1/embeddings
KB_PAGE_EMBEDDING_MODEL=Qwen/Qwen3-VL-Embedding-2B
KB_PAGE_EMBEDDING_DIM=2048
```

`2048` is the 2B model's output width (the Hugging Face model card lists "Embedding
Dimension: Up to 2048", MRL down to 64, and its examples return `(n, 2048)`). The 8B model on
Station is `4096`. The dimension must match what the server returns or every page is an
error; if you pass `dimensions` to vLLM, set `KB_PAGE_EMBEDDING_DIM` to the same value.
The same variables without `_FORMAT=vllm` are the OpenRouter path (`voyage-multimodal-3.5`).

**Status: tested against a mock server only, not a real vLLM.** This machine has no NVIDIA
GPU, so the request shape and ordering are covered by unit tests in
`rust/crates/kb-inference/src/page_embedder.rs`, but end-to-end embedding quality and
throughput with Qwen3-VL-Embedding-2B on vLLM are unverified. Do not quote a local-tier
page-retrieval number from this path until it has run on Desk/Studio hardware.

## Local OCR (PaddleOCR-VL / PP-OCRv6)

Scanned and complex pages are OCRed by a **local** model on every local tier: PaddleOCR-VL-1.6
under vLLM (Desk/Studio/Station) or `llama.cpp` (Apple, CPU), and PP-OCRv6 on Lite through a
sidecar that speaks the same shape. The adapter (`kb_extract::PageOcrExtractor`) renders each
PDF page with pdfium at `KB_OCR_DPI`, sends **one chat request per page** (image part first,
then the task prefix, `temperature: 0`) and keeps page order into the chunker's page map. Image
uploads (`png`, `jpg`, `tif`) are a document of one page.

```sh
# Desk / Studio / Station (vLLM; PaddleOCR-VL selects its task with the prompt prefix)
vllm serve PaddlePaddle/PaddleOCR-VL-1.6 --trust-remote-code

# Apple / CPU (llama.cpp, the GGUF build)
llama-server -m PaddleOCR-VL-1.6-Q8_0.gguf --host 127.0.0.1 --port 8000

# point kb-server at either one; `ocr` also works as a name in
# KB_LAYOUT_EXTRACTOR_ORDER when you want OCR ahead of another parser
KB_OCR_BASE_URL=http://127.0.0.1:8000/v1/chat/completions
KB_OCR_MODEL=PaddlePaddle/PaddleOCR-VL-1.6
```

| Setting | Meaning | Default |
|---|---|---|
| `KB_OCR_BASE_URL` | full URL of `/v1/chat/completions`; empty turns the adapter off | empty (off) |
| `KB_OCR_MODEL` | model name the server serves | `PaddlePaddle/PaddleOCR-VL-1.6` |
| `KB_OCR_API_KEY` | optional bearer token; never filled from `OPENROUTER_API_KEY` | empty (no header) |
| `KB_OCR_PROMPT` | task prefix (PaddleOCR-VL: `OCR:`, `Table Recognition:`, …) | `OCR:` |
| `KB_OCR_DPI` | page render resolution | `150` |
| `KB_OCR_CONCURRENCY` | page requests in flight | `2` |
| `KB_OCR_TIMEOUT_MS` | timeout per page request | `120000` |
| `KB_OCR_MAX_TOKENS` | `max_tokens` per page | `4096` |

When `KB_OCR_BASE_URL` is set it also replaces OpenRouter vision as the OCR path for image
uploads and as the layout chain's final fallback, so no page leaves the host. Unset, everything
resolves exactly as before. A page that still fails after its retries fails the extraction with
its page number rather than producing a document with a silently missing page. Answers are
cached under `KB_CACHE_DIR` with the rest of the extraction, keyed by model, prompt, DPI and
endpoint.

**Status: mock-tested.** The request shape, ordering, retries, `Authorization` rule and the
`ocr`/OpenRouter precedence are covered by tests in
`rust/crates/kb-extract/src/page_ocr.rs` and `builder.rs`; end-to-end OCR quality and
throughput with PaddleOCR-VL-1.6 on real hardware are not measured yet. PaddleOCR-VL-1.6's GGUF
did run end to end under llama.cpp on this CPU (102.5 s per page, text identical to the PDF text
layer: `docs/tier-readiness-plan.md`). PP-OCRv6 on Lite runs in `rust/deploy/ocr-ppocr`
(RapidOCR 3.10, ONNX Runtime, CPU), a sidecar that answers the same chat shape; it is
**built but not yet run** (the image has not been built on this machine).

## The one-model-per-role rule

One multimodal embedder puts text chunks, page images and the query in **one vector space**:
one index, one query embedding. One multimodal reranker scores (query, text) and
(query, page image) pairs alike, so a chart-only page is judged on what it shows.

Must be measured before this replaces today's text-only models:

1. Qwen3-VL-Embedding-2B on plain text (MIRACL-id) vs Qwen3-Embedding-0.6B. If it loses
   clearly on text, keep both embedders.
2. The 8-bit vision models against full precision on the same pages (llama.cpp's vision path
   has a known accuracy bug, #29251).
3. Reranker latency: a 2B reranker is ~3.5× bge's size; bge on the top 20 is ~1.4 s per
   query on the RX 590 (measured).

Evidence so far (ViDoRe v3 paper, Table 2): a text reranker added +13.7 nDCG on HR; the
image reranker jina-reranker-m0 added +0.2 on average. So the multimodal reranker is
A/B-tested against a text reranker on page text, not assumed.

## The document parser stays a separate model

Specialised parsers beat general vision models at OCR and tables: PaddleOCR-VL-1.6 scores
96.34 on OmniDocBench, Gemini 3 Flash 92.62 (published). The parser is also where page
numbers and bounding boxes come from. Desk and Studio run the same parser; only residency
differs.

## Settings

To run a tier locally, `rust/scripts/kb-dev.sh up <tier>` starts the stores, the tier's model
containers and kb-server from `rust/deploy/compose.yaml`; see
[`rust/deploy/README.md`](rust/deploy/README.md) for what each profile starts, the measured
numbers and how to override a setting.

| Setting | Values | Default |
|---|---|---|
| `KB_TIER` | `lite`, `lite-en`, `desk`, `studio`, `station`, `cloud` | unset: today's explicit per-role settings |
| `KB_PARSE_RESIDENCY` | `on-demand`, `always` | Desk `on-demand`, Studio and up `always` |
| `KB_PARSE_IDLE_UNLOAD_S` | seconds | 300 |
| `KB_CAPTION_RESIDENCY` | `on-demand`, `always` | same as parse |
| `KB_CAPTION_*` | endpoint of the chat LLM used as the captioner | — |
| `KB_OCR_*` | local OCR endpoint, model, prompt, DPI, concurrency and limits | off (`KB_OCR_BASE_URL` unset) |
| `KB_CACHE_DIR` | directory for content-addressed caches | unset: no cache |

A tier is a **preset**: it fills in role defaults; any explicit `KB_*` setting wins.
`kb-server setup` checks the VRAM budget and refuses a combination that cannot fit next to
the chat LLM (unless `--force`); server start does not check it yet.

### `KB_TIER` (implemented 2026-10-24; OCR and Qwen3-VL roles 2026-10-30)

`KB_TIER` fills a role default for every variable the operator did not set. It is not a mode
switch: the resolved config is exactly what the operator would have written by hand.

- **An explicit environment variable always wins.** A variable that is present counts as
  explicit even when empty: `KB_RERANK_ENABLED=` suppresses the preset for the reranker and
  falls back to that setting's own default.
- **Unset `KB_TIER` resolves exactly as before** — proven by
  `test_unset_tier_resolves_exactly_like_todays_config` in
  `rust/crates/kb-server/src/config.rs`, which compares the full resolved config.
- An unknown value is a startup error listing the five names.
- Startup logs one INFO line with the tier and every variable it filled, so an operator can see
  what the preset did. Values are model names and numbers; key-shaped names are redacted.
- `kb-server apply-schema` resolves the tier too, so the Qdrant collections are created at the
  width the server will store.

Every role in "What each tier runs" that has an adapter is filled: text embedding, reranker,
scanned-page OCR (`KB_OCR_*`) and, on Studio and Station, the page-image arm (Qwen3-VL-Embedding
on vLLM) with the Qwen3-VL reranker. Lite and Desk keep the page arm off, as the table says.
Each local service has a fixed loopback port (8081 text embed, 8082 rerank, 8083 OCR, 8084 page
embed) so a bare-metal install and `rust/deploy/compose.yaml` agree; compose overrides the host
with the service name. **The Studio/Station page and rerank roles are mock-tested only** (no
NVIDIA machine here): see "Local page embedder (vLLM)" for what is and is not verified. Lite's concurrency and batch numbers are **placeholders** until the 2-core
measurement (build order 5); the Desk / Studio / Station rerank widths are estimates.

| Variable | lite | desk | studio | station | cloud |
|---|---|---|---|---|---|
| `KB_EMBEDDING_MODEL` | `intfloat/multilingual-e5-small` | `Qwen/Qwen3-Embedding-0.6B` | same as desk | same as desk | `voyageai/voyage-multimodal-3.5` |
| `KB_EMBEDDING_DIM` | 384 | 1024 | 1024 | 1024 | 1024 |
| `KB_EMBEDDING_BASE_URL` | `http://127.0.0.1:8081/v1/embeddings` | same | same | same | OpenRouter (built-in default) |
| `KB_EMBEDDING_QUERY_PREFIX` | `query: ` | Qwen3 instruction¹ | same as desk | same as desk | — |
| `KB_EMBEDDING_PASSAGE_PREFIX` | `passage: ` | — | — | — | — |
| `KB_EMBEDDING_TIMEOUT_MS` | 120000 | same | same | same | — |
| `KB_EMBEDDING_INPUT_TYPE` | — | — | — | — | `true` |
| `KB_EMBED_CONCURRENCY` | 1 | 2 | 4 | 8 | — |
| `KB_EMBEDDING_BATCH_SIZE` | 16 | — | — | — | — |
| `KB_INGEST_CONCURRENCY` | 1 | — | — | — | — |
| `KB_ENTITY_EXTRACTION_ENABLED` | `false` | `false` | `false` | `false` | `false` (a paid LLM call per document; only written to the Postgres graph when `KB_GRAPH_STORE=pg`) |
| `KB_RERANK_ENABLED` | `false` | `true` | `true` | `true` | `true` |
| `KB_RERANK_PROVIDER` | — | `tei` | `vllm` | `vllm` | `cohere` |
| `KB_RERANK_BASE_URL` | — | `http://127.0.0.1:8082` | same | same | `https://openrouter.ai/api/v1/rerank` |
| `KB_RERANK_MODEL` | — | `BAAI/bge-reranker-v2-m3` | `Qwen/Qwen3-VL-Reranker-2B` | `Qwen/Qwen3-VL-Reranker-8B` | `voyageai/rerank-2.5` |
| `KB_RERANK_CANDIDATE_LIMIT` | — | 10 | 20 | 30 | 20 |
| `KB_OCR_BASE_URL` | `http://127.0.0.1:8083/v1/chat/completions` | same | same | same | — (Mistral OCR when `KB_MISTRAL_OCR_KEY` is set) |
| `KB_OCR_MODEL` | `PaddlePaddle/PP-OCRv6` | `PaddlePaddle/PaddleOCR-VL-1.6` | same as desk | same as desk | — |
| `KB_OCR_CONCURRENCY` | 1 | — (2) | 4 | 8 | — |
| `KB_PAGE_EMBEDDING_MODEL` | — | — | `Qwen/Qwen3-VL-Embedding-2B` | `Qwen/Qwen3-VL-Embedding-8B` | `voyageai/voyage-multimodal-3.5` |
| `KB_PAGE_EMBEDDING_FORMAT` | — | — | `vllm` | `vllm` | — (OpenRouter) |
| `KB_PAGE_EMBEDDING_BASE_URL` | — | — | `http://127.0.0.1:8084/v1/embeddings` | same | — (OpenRouter) |
| `KB_PAGE_EMBEDDING_DIM` | — | — | 2048 | 4096 | 1024 |
| `KB_PAGE_RERANK` | `false` | `false` | `false` | `false` | `true` |

¹ `Instruct: Given a question, retrieve passages that answer it\nQuery: ` — the same string
`rust/scripts/stack-up.sh` and `docs/local-models.md` already use.

A dash means the preset leaves the setting alone, so that setting's own default applies.
`same` means the value in lite's column, unless it says otherwise. Lite's e5-small was chosen by
measurement on 2 cores (`docs/eval-ab/lite-2core`); run its embedder with 1 thread so it leaves a
core for kb-server.

**`lite-en`** is Lite for English-only deployments: the same settings, but text is embedded with
`sentence-transformers/all-MiniLM-L6-v2` (384-d, no prefixes) and the dense vector sees only the
first 512 characters of each chunk (`KB_EMBEDDING_MAX_CHARS=512`; BM25 keeps the full text).
On 2 cores it ingests ViDoRe HR at 6.43 pages/s (page nDCG@10 0.517) against Lite's 1.55 pages/s
(0.545), but scores 0.679 vs 0.802 on Indonesian MIRACL-id-50, so it is not the default.

- The local text stack is what we can run today (`docs/local-models.md`):
  Qwen3-Embedding-0.6B at an OpenAI-compatible endpoint, bge-reranker-v2-m3 at a local
  `/rerank`. Local tiers need **no paid key**: the embedder sends no `Authorization` header
  unless `KB_EMBEDDING_API_KEY` is set, and a hosted `OPENROUTER_API_KEY` is never sent to a
  loopback or LAN endpoint.
- Cloud drives the embedder and the reranker from one `OPENROUTER_API_KEY` (or
  `KB_EMBEDDING_API_KEY`); missing it is a config error naming the tier and the variable. The
  Cohere-shaped rerank adapter accepts that key only when `KB_RERANK_BASE_URL` is an
  `openrouter.ai` URL.
- Qdrant is the only chunk store (SurrealDB was removed on 2026-10-25), so no tier selects a
  store. `KB_PAGE_EMBEDDING_MODEL=` (empty) is how an operator drops the cloud page arm.
- The preset fills embedding concurrency as `KB_EMBED_CONCURRENCY`, the legacy v0.1.0 name,
  which Rust honours when `KB_EMBEDDING_CONCURRENCY` is unset. That way it can never shadow an
  operator's `KB_EMBEDDING_CONCURRENCY`.
- The local vLLM page embedder never receives `OPENROUTER_API_KEY`: with
  `KB_PAGE_EMBEDDING_FORMAT=vllm` only `KB_PAGE_EMBEDDING_API_KEY` is sent.
- Studio and Station embed **text** with Qwen3-Embedding-0.6B (the measured local text stack)
  and **pages** with Qwen3-VL-Embedding in a separate Qdrant vector, fused at search time. One
  shared text+page space, as the role table puts it, would need the VL model to embed text too;
  that is unmeasured, so it is not the preset.
- Station runs PaddleOCR-VL-1.6 like Studio. MinerU2.5-Pro stays a documented alternative
  (`KB_LAYOUT_EXTRACTOR_ORDER=sidecar` with a MinerU sidecar); no preset selects it.
- bge is a poor page reranker (`docs/eval-ab/vidore3-hr-en-night`, "bge should not rerank
  pages"), so every local tier sets `KB_PAGE_RERANK=false`; it only matters when the page arm
  is on.

The caption role points at the app's chat LLM with a **priority queue**: it writes
descriptions when the LLM is idle and backs off while users are chatting.

## `kb-server setup`

Implemented 2026-10-30 (`rust/crates/kb-server/src/setup.rs`, run as
`rust/scripts/kb-dev.sh setup [flags]`). It plans and writes the config; it does not download
or start anything, so it works on a machine with no Postgres, Qdrant, Docker or models.

1. **Detect** each NVIDIA GPU (`nvidia-smi --query-gpu=name,memory.total`), Apple silicon
   (`uname -s` + `sysctl hw.memsize` + the CPU brand; unified memory), CPU cores and RAM
   (`/proc/meminfo`, `available_parallelism`). AMD/other GPUs and virtual displays are
   reported as "unsupported accelerator, treated as CPU". There is no Windows tier.
2. **Suggest a tier** from the table above: NVIDIA ≥48 / ≥24 / ≥8 GB → station / studio /
   desk; Apple unified ≥128 / ≥32 / ≥16 GB → the same; otherwise `lite` (`lite-en` with
   `--english`). With several NVIDIA cards the largest is used: compose reserves one GPU per
   model container, so cards do not pool. `cloud` is never suggested, because it spends
   money on every page and query.
3. **Check the budget** against the estimates above and refuse a tier that does not fit, or a
   desk/studio/station choice on a box with no NVIDIA GPU, unless `--force`. The output labels
   every number an estimate.
4. **Gate the licences**: every model a tier's preset or compose profile loads has a verdict
   from `docs/model-licences.md`; a 🔴 model is refused, a 🟡 one needs
   `--accept-licence <model>`, and a model with no entry is refused. A unit test fails when a
   model named in `tier.rs` has no licence entry.
5. **Write the config**: `rust/deploy/.env` (starting from `.env.example`) and
   `rust/deploy/.tier`, never overwriting an existing `.env` without `--overwrite`. Only
   operator-level keys are written — `KB_TIER` selects the preset, so preset values are not
   duplicated. Cloud takes `--openrouter-key-env NAME` (the value is written only with
   `--write-secrets`, otherwise an empty placeholder with a comment) and optionally
   `--mistral-key-env NAME` (which adds `mistral` to `KB_LAYOUT_EXTRACTOR_ORDER`). Studio,
   Station and Cloud — the tiers with the page arm — take `--chat-llm-url` /
   `--chat-llm-model` for `KB_PAGE_CAPTION_*`; on Lite and Desk the flags are reported as
   ignored. Then it prints `rust/scripts/kb-dev.sh up <tier>`, or runs it with `--start`.
6. **`--check`** loads the env file plus the process environment (process wins, like compose)
   with the tier preset applied, and makes one real request per configured role: an embedding
   of `ping`, a rerank of two documents, the page embedder, the caption endpoint, and
   `GET /v1/models` for OCR (`KB_OCR_BASE_URL`, or Mistral when `KB_MISTRAL_OCR_KEY` is set).
   It also probes Postgres (TCP), Qdrant and S3 (HTTP) when they are configured. The table
   prints role, host (credentials redacted), status and latency; a required failure exits
   non-zero. Paid hosts (openrouter.ai, api.mistral.ai, voyage, Cohere) are **skipped** unless
   `--check-paid`: a check must not spend.

Not built, and not pretended by `setup`:

- **Model download and start.** The containers pull the weights into the shared `hf-models`
  volume on their first start (`kb-dev.sh up` waits up to 15 minutes for the TEI healthcheck);
  `setup` only writes the file `up` reads. There is no vLLM/llama.cpp launcher in Rust.
- **A smoke eval.** `--check` proves each endpoint answers; it does not measure quality.
- **Residency and captions.** `KB_PARSE_RESIDENCY`, `KB_CAPTION_RESIDENCY` and the caption
  priority queue do not exist, so setup does not ask about them.
- **Apple silicon end to end.** The suggestion table covers it, but
  `rust/deploy/compose.yaml` only reserves NVIDIA GPUs; setup warns and expects the models to
  run outside compose.

## Build order

Status as of 2026-10-24:

1. ✅ Content-addressed disk cache for embeddings, extraction and page vectors
   (`kb-cache`, `KB_CACHE_DIR`). A repeat ViDoRe run went from 1,686 s to 24 s with zero
   provider calls. Captions are not cached yet; there is no caption role yet.
2. ✅ `KB_TIER` preset, **text roles only** (`rust/crates/kb-server/src/tier.rs`): Lite–Station
   point at the local Qwen3-Embedding-0.6B + bge stack, Cloud at voyage through OpenRouter.
   Explicit env wins, unset `KB_TIER` is byte-identical to today, and startup logs what the
   preset filled. Still deferred: the multimodal roles (Qwen3-VL embedder/reranker), so local
   tiers keep the page arm off.
3. ✅ measured, ❌ no gain: header/footer removal (`KB_STRIP_PAGE_FURNITURE`), −0.003
   [−0.013, +0.006] on ViDoRe HR. Off by default.
   ❌ dropped for retrieval: rebuilding tables in text. ViDoRe's own markdown, with real
   tables, scores no better on table questions than our flat text (0.500 vs 0.506). Tables
   still matter for computing answers (`docs/architecture-target.md` §10), a separate goal.
4. ✅ Page-image arm, live (`KB_PAGE_EMBEDDING_MODEL`), with text+image page vectors
   (`KB_PAGE_TEXT_CHARS`) and page-level reranking (`KB_PAGE_RERANK`, on by default when the
   arm is on). **ViDoRe v3 HR, live: 0.705 page nDCG@10** (text-only same stack 0.645;
   best published retriever 0.654, best published pipeline with a reranker 0.665),
   `docs/eval-ab/vidore3-hr-en-night`. Leak-matrix `page` row: 0 leaks.
   - Cloud tier, measured: `voyageai/voyage-multimodal-3.5` pages + `qwen/qwen3-embedding-8b`
     text (with Qwen's query instruction in `KB_EMBEDDING_QUERY_PREFIX`) + `voyageai/rerank-2.5`
     for chunks and pages: **0.708** live on HR. ~$0.0006 per page to index.
   - Cloud tier, one embedder (2026-10-24): `voyageai/voyage-multimodal-3.5` for text chunks
     too (`KB_EMBEDDING_INPUT_TYPE=true`): **0.711** (a tie with 0.708 at the top; text-only
     0.674 vs 0.657, +0.017 [+0.005, +0.030]). This is the cloud default: one model, one key.
   - Same stack with the local bge reranker instead of voyage: text-only 0.618; with pages
     **0.661 with `KB_PAGE_RERANK=false`** (0.628 with it on: bge is a poor page reranker,
     −0.058 alone offline, and adds ~10 s per search on the RX 590). Local tiers with bge keep
     page rerank off until a stronger local reranker (Qwen3-VL-Reranker) is measured.
   - ViDoRe v3 computer science (2026-10-24), cloud default (one voyage embedder, pages,
     voyage rerank): **0.828** (text-only 0.779; best published retriever 0.786). The page arm
     adds +0.049 [+0.035, +0.065]; voyage instead of qwen3-8b for chunks is a wash here
     (−0.006 [−0.021, +0.006]). `docs/eval-ab/vidore3-cs-en-night`.
   - Local tiers: `KB_PAGE_EMBEDDING_FORMAT=vllm` with Qwen3-VL-Embedding-2B/8B under vLLM
     (`3111e16`, mock-tested only; no NVIDIA box yet). The local page reranker is bge-reranker
     on page text (measured offline only with bge on chunks); Qwen3-VL-Reranker is untested.
   - Live vision rerank (opt-in, cloud): `KB_PAGE_VISION_RERANK_MODEL=qwen/qwen3.7-flash` adds
     one chat call over the top page images; its order is blended with the page order by
     weighted RRF. Offline ViDoRe HR: 0.711 → **0.727** nDCG@10 at depth 20
     (`docs/eval-ab/vidore3-hr-en-image`). Other knobs: `_DEPTH` (20), `_TIMEOUT_MS` (8000),
     `_WEIGHT` (0.6), `_K` (5), `_NO_REASONING`, `_BASE_URL`, `_API_KEY` (defaults to
     `OPENROUTER_API_KEY`). Requires the page arm; a failure keeps the page order and reports
     `vision_rerank_unavailable`. Live latency is not measured yet.
   - Page captions (opt-in, ingest, cloud): `KB_PAGE_CAPTION_MODEL` asks a vision model to
     describe only what each rendered page draws — charts, graphs, tables, diagrams, maps,
     photos — once, at ingest. The description joins the stored page text the page reranker
     reads and is embedded into the text index as a figure chunk, so chart content becomes
     searchable as text. Off by default and requires the page arm. Other knobs:
     `_BASE_URL`, `_API_KEY` (defaults to `OPENROUTER_API_KEY`), `_CONCURRENCY` (8),
     `_TIMEOUT_MS` (30000), `_MAX_TOKENS` (300), `_NO_REASONING` (false), `_TEXT_CHUNKS`
     (true) and `_RERANK_TEXT` (true), the last two so each use can be measured alone.
     Answers are cached on disk under `KB_CACHE_DIR`, keyed by model, prompt version,
     reasoning flag and image hash; a page with no visuals (`NONE`) is cached too. Effect on
     retrieval is not measured yet.
5. Lite on exactly 2 cores (`taskset`): ✅ small-embedder bake-off (2026-10-24, `docs/eval-ab/lite-2core`): multilingual-e5-small, 0.802 nDCG@10 on MIRACL-id-50, ~2 min ingest, 42 ms search; Qwen3-0.6B ties on quality but is 41× slower to ingest. The embedder must be capped to 1 thread or it starves the server. PP-OCRv6 not done.
6. ✅ `kb-server setup` (2026-10-30): hardware detection, tier suggestion, VRAM budget and
   licence gates, `.env`/`.tier` writing, `--check` probes. ❌ Desk / Studio / Station on
   real hardware: still no NVIDIA box, so those tiers are unit-tested and
   `docker compose config`-checked only.

## Open risks

- No NVIDIA or Apple box to measure Desk and up yet; the LAN GPUs are AMD on Windows.
- Bonsai's 98.2%-of-full-precision figure is PrismML's own; Indonesian chat quality unchecked.
- Throughput for 5 users on the PrismML fork vs vLLM batching is unmeasured.
- DGX Spark bandwidth (~273 GB/s) may make a 32B chat model slow for 5 users.
