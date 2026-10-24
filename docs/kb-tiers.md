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
| Embedding (text + page images, one space) | small multilingual text model on CPU | Qwen3-VL-Embedding-2B, 8-bit | Qwen3-VL-Embedding-2B, full precision | Qwen3-VL-Embedding-8B | voyage-multimodal-3.5 or gemini-embedding-2 |
| Reranker | tiny text reranker, or none | Qwen3-VL-Reranker-2B, 8-bit | Qwen3-VL-Reranker-2B, full precision | Qwen3-VL-Reranker-8B | voyage rerank-2.5 / cohere rerank-4 / qwen3-reranker-8b |
| Digital PDF text | pdfium text layer | text layer | text layer | text layer | text layer |
| Scans / complex pages | PP-OCRv6 (ONNX, CPU) | PaddleOCR-VL-1.6, loaded during ingest | PaddleOCR-VL-1.6, resident | MinerU2.5-Pro / PaddleOCR-VL | Mistral OCR |
| Charts / figures | small VLM on figure crops, background queue | chat LLM's vision, idle time | chat LLM's vision | chat LLM's vision | Gemini Flash |
| Page-image search | optional tiny model (colSmol-256M), off by default | yes (same embedder) | yes | yes | yes |
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
| Qwen3-VL-Embedding-2B Q8 | ~2.2 GB |
| Qwen3-VL-Reranker-2B Q8 | ~2.2 GB |
| **Total** | **~7.5–8.6 GB** |

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

| Setting | Values | Default |
|---|---|---|
| `KB_TIER` | `lite`, `desk`, `studio`, `station`, `cloud` | unset: today's explicit per-role settings |
| `KB_PARSE_RESIDENCY` | `on-demand`, `always` | Desk `on-demand`, Studio and up `always` |
| `KB_PARSE_IDLE_UNLOAD_S` | seconds | 300 |
| `KB_CAPTION_RESIDENCY` | `on-demand`, `always` | same as parse |
| `KB_CAPTION_*` | endpoint of the chat LLM used as the captioner | — |
| `KB_CACHE_DIR` | directory for content-addressed caches | unset: no cache |

A tier is a **preset**: it fills in role defaults; any explicit `KB_*` setting wins. Server
start and `kb-server setup` check the VRAM budget and warn, or refuse, when a combination
can't fit next to the chat LLM.

The caption role points at the app's chat LLM with a **priority queue**: it writes
descriptions when the LLM is idle and backs off while users are chatting.

## `kb-server setup --tier …`

1. Detect the hardware (CUDA, Metal, or none) and suggest a tier.
2. Download only 🟢/🟡 models from `docs/model-licences.md`.
3. Start them (vLLM on CUDA, llama.cpp / MLX on Apple, the parser sidecar included).
4. Health checks and a small smoke eval.
5. Write the config kb-server needs.

## Build order

Status as of 2026-10-24:

1. ✅ Content-addressed disk cache for embeddings, extraction and page vectors
   (`kb-cache`, `KB_CACHE_DIR`). A repeat ViDoRe run went from 1,686 s to 24 s with zero
   provider calls. Captions are not cached yet; there is no caption role yet.
2. ⏸ `KB_TIER` preset. Deferred until the roles it switches exist: a preset that points at
   models kb-server cannot drive yet would be a setting that does nothing.
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
5. Lite on exactly 2 cores (`taskset`): PP-OCRv6 via ONNX, small-embedder bake-off.
6. Desk / Studio / Station on real hardware; `kb-server setup`.

## Open risks

- No NVIDIA or Apple box to measure Desk and up yet; the LAN GPUs are AMD on Windows.
- Bonsai's 98.2%-of-full-precision figure is PrismML's own; Indonesian chat quality unchecked.
- Throughput for 5 users on the PrismML fork vs vLLM batching is unmeasured.
- DGX Spark bandwidth (~273 GB/s) may make a 32B chat model slow for 5 users.
