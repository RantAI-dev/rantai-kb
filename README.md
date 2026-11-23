<div align="center">

<img src="assets/wordmark.svg" alt="RantAI KB" width="300">

**Document processing, indexing, and hybrid retrieval as a standalone Rust service.**  
Upload a PDF, spreadsheet, Office file, or code file; get back structured chunks, typed tables, vectors, or cited search results over HTTP.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Runtime](https://img.shields.io/badge/runtime-Rust-dea584.svg)](https://www.rust-lang.org/)
[![Image](https://img.shields.io/badge/image-ghcr.io%2Frantai--dev%2Frantai--kb-24292e.svg)](https://github.com/RantAI-dev/rantai-kb/pkgs/container/rantai-kb)

</div>

---

Extracted from the RantAI Agents platform and packaged as an independent HTTP service so any application — web app, backend worker, or CLI — can use the same extraction, chunking, and retrieval engine without sharing a codebase.

## Two ways to run it

| | Full service (default) | Stateless processor (`--stateless`) |
|---|---|---|
| **When to use** | You want a turnkey, multi-tenant knowledge base with async ingest jobs, storage, hybrid search, and typed table queries. | Your app already owns its own database and Qdrant server and wants `rantai-kb` purely as a document-processing & inference microservice. |
| **External stores** | Postgres + Qdrant + S3/blob | **None** (zero DB, Qdrant, or S3 connections) |
| **Core endpoints** | `POST /v1/documents`, `GET /v1/jobs/:id`, `POST /v1/search`, `POST /v1/tables/:id/query` | `POST /v1/process`, `POST /v1/encode`, `POST /v1/rerank` |
| **Auth** | Per-tenant scoped keys (`rantai-kb key`) | Static key(s) via `KB_STATELESS_API_KEY` (or explicit private-network opt-out) |
| **Scaling & protection** | Async worker + stage seats (`KB_SEATS`) | Stage seats + query fast-lane + `KB_STATELESS_MAX_INFLIGHT` (`503 Retry-After`) |

---

## Quick start

### Option A — Full service (Docker Compose)

```bash
curl -fsSL https://raw.githubusercontent.com/RantAI-dev/rantai-kb/main/install.sh | sh

rantai-kb setup lite      # detect hardware, verify VRAM & licences, write deploy/.env
rantai-kb up lite         # start the stack and wait for /health
rantai-kb key my-tenant   # mint and print an API key
```

The installer places `rantai-kb` (and its alias `kb-server`) in `~/.local/bin` and the compose bundle in `~/.rantai-kb/deploy`. Requires Docker + Compose ≥ 2.20; the first start downloads the tier's models.

```bash
# Ingest a document (async: returns 202 + jobId)
curl -sS -H "Authorization: Bearer $KB_KEY" \
  -F file=@mission.pdf http://127.0.0.1:8080/v1/documents

# Hybrid search (dense + BM25 sparse + optional rerank)
curl -sS -H "Authorization: Bearer $KB_KEY" \
  -H 'content-type: application/json' \
  -d '{"query":"seven minutes of terror","maxChunks":5}' \
  http://127.0.0.1:8080/v1/search
```

### Option B — Stateless processor microservice (zero DB / Qdrant / S3)

```bash
# Minimal boot: extraction, smart chunking, tables, figures, and Qdrant-ready BM25 sparse vectors
KB_STATELESS_API_KEY=your-secret \
  rantai-kb serve --stateless
# Equivalent env toggle: KB_MODE=stateless rantai-kb serve

# With cloud embedder + reranker + OCR (or point KB_EMBEDDING_BASE_URL / KB_RERANK_BASE_URL at local vLLM/TEI/Ollama)
KB_MODE=stateless \
KB_TIER=cloud \
KB_STATELESS_API_KEY=your-secret \
OPENROUTER_API_KEY=sk-or-... \
  rantai-kb serve
```

```bash
# 1. Process a file -> chunks, tables, figures, sparse (+ optional dense) vectors, entities
curl -sS -H "Authorization: Bearer your-secret" \
  -F file=@report.pdf http://127.0.0.1:8080/v1/process

# 2. Encode search queries with the exact same dense prefixes & BM25 tokenizer for your Qdrant server
curl -sS -H "Authorization: Bearer your-secret" \
  -H 'content-type: application/json' \
  -d '{"texts":["quarterly revenue breakdown"],"mode":"query"}' \
  http://127.0.0.1:8080/v1/encode

# 3. Rerank candidate passages (local TEI/vLLM/llama.cpp or cloud Cohere/Voyage/OpenRouter)
curl -sS -H "Authorization: Bearer your-secret" \
  -H 'content-type: application/json' \
  -d '{"query":"Qdrant sparse search","documents":["...","..."],"topK":5}' \
  http://127.0.0.1:8080/v1/rerank
```

Interactive Swagger UI is always served at [`http://127.0.0.1:8080/docs`](http://127.0.0.1:8080/docs) (backed by `/openapi.json`) in both modes. From a source checkout, `rust/scripts/kb-dev.sh` provides the same CLI without installing.

---

## What it does with a file

- **Format-native extraction (55 extensions):** PDFs use pdfium for text layers with layout parsing or OCR for scans; Office Open XML (`.docx`, `.xlsx`, `.pptx`), OpenDocument, Markdown, plain text, code, CSV/TSV/JSON/YAML, HTML/XML, RTF, and EPUB take native paths. `GET /v1/formats` lists every accepted extension and the size limit (256 MiB default).
- **Table- and code-aware chunking:** Spreadsheets and tables are never cut mid-row; tables are structured into typed grids with inferred manifests (`table_summary` + `table_row` chunks) and can be queried via typed aggregations in full mode (see [`docs/api.md`](docs/api.md#tables)).
- **Hybrid vectors & reranking:** Every chunk carries a Qdrant-compatible BM25 sparse vector (FNV-1a hashed, server-side IDF) alongside optional dense embeddings and cross-encoder reranking.
- **Stage-seated concurrency:** `KB_SEATS` bounds local extraction and bulk embedding so VMs and model servers stay saturated without thrashing; remote OCR handoff releases local `extract` seats early, and live search queries run on a fast lane.

---

## Tiers, hardware, and models

A tier is a preset that picks models and containers; any explicit `KB_*` variable overrides it. Individual roles can also be moved off-box (`setup --ocr mistral`, `--ocr <url>`, `--vision off|<url>`, or external embedder/reranker endpoints).

### Hardware requirements

Measured for CPU tiers; estimated for GPU tiers.

| Tier | Minimum | Recommended | CPU only |
|---|---|---|---|
| `lite` | 2 cores, 4 GB RAM, 5 GB disk | 4 cores, 8 GB RAM, 10 GB disk | ✓ |
| `lite-en` | 2 cores, 2 GB RAM, 5 GB disk | 4 cores, 4 GB RAM, 10 GB disk | ✓ |
| `desk` | NVIDIA 8 GB, 4 cores, 8 GB RAM, 15 GB disk | 12 GB VRAM, 8 cores, 16 GB RAM, 25 GB disk | — |
| `studio` | NVIDIA 24 GB, 8 cores, 16 GB RAM, 25 GB disk | 32 GB VRAM, 12 cores, 32 GB RAM, 40 GB disk | — |
| `station` | NVIDIA 48 GB, 12 cores, 32 GB RAM, 50 GB disk | 80 GB VRAM, 16 cores, 64 GB RAM, 100 GB disk | — |
| `cloud` | 2 cores, 2 GB RAM, 5 GB disk | 4 cores, 4 GB RAM | ✓ |

> **Host notes:** GPU tiers budget VRAM to share the card with your chat LLM (Desk ≈4.5 GB KB / 8 GB; Studio ≈10 / 24 GB; Station ≈20 / 48 GB). Compose GPU reservations require Linux + NVIDIA driver + `nvidia-container-toolkit` (Apple Silicon runs models outside compose; Windows is unsupported).

### Model matrix

**CPU tiers** (no GPU, no paid API keys):

| Role | `lite` (multilingual) | `lite-en` (English-only, ~4× faster) |
|---|---|---|
| Embeddings | `intfloat/multilingual-e5-small` (384-d, TEI CPU, 471 MB) | `sentence-transformers/all-MiniLM-L6-v2` (384-d, TEI CPU, 91 MB) |
| Scanned-page OCR | PP-Layout CDLA + PP-OCRv6 tiny + SLANet-plus (`ocr-cpu` ONNX) | same |
| Figure captions | `Qwen3-VL-2B-Instruct` Q4_K_M (`vlm-cpu`, llama.cpp) | same |
| Reranker / Page search | off | off |

**Local GPU tiers** (`desk`, `studio`, `station` — one unified vector space for text chunks and page images):

| Role | Model |
|---|---|
| Embeddings (text + pages) | `nvidia/Nemotron-3.5-Embed-1B` (2048-d, vLLM; BF16 or auto NVFP4 on Blackwell) |
| Reranker | `Qwen/Qwen3-Reranker-0.6B` (llama.cpp `/v1/rerank`, 1.19 GB) |
| Scanned-page OCR | `xiaomi-research/Xiaomi-OCR-0` (0.8B FP8, vLLM, 1.75 GB) |
| Figure captions | `Qwen3-VL-2B-Instruct` Q4_K_M (llama.cpp CUDA, 1.11 GB) |

**Cloud tier** (zero local GPU; hosted APIs):

| Role | Model |
|---|---|
| Embeddings (text + pages) | `voyageai/voyage-multimodal-3.5` (1024-d, unified text + page space) |
| Reranker | `voyageai/rerank-2.5` (Cohere-shaped rerank) |
| Scanned-page OCR | Mistral OCR |
| Figure captions | `nvidia/nemotron-3.5-super-vl` |

Every local model is Apache-2.0 or MIT (details in [`docs/model-licences.md`](docs/model-licences.md)). Benchmark numbers, VRAM arithmetic, and verification status per tier are documented in [`docs/kb-tiers.md`](docs/kb-tiers.md) and [`docs/performance.md`](docs/performance.md).

---

## API overview

All `/v1/*` routes take `Authorization: Bearer <key>` or `X-API-Key: <key>`. Health (`/health`, `/v1/health`), formats (`/v1/formats`), and OpenAPI (`/openapi.json`, `/docs`) are unauthenticated.

- **Full service:** `POST /v1/documents`, `GET /v1/jobs/:id`, `POST /v1/search`, `GET /v1/tables`, `POST /v1/tables/:id/query`, `GET /v1/documents/:id/structure`, knowledge bases, categories, SSE events.
- **Stateless processor:** `POST /v1/process`, `POST /v1/encode`, `POST /v1/rerank` (inspect contract anytime via `rantai-kb openapi --stateless`).

Full endpoint reference, options, and schemas: [`docs/api.md`](docs/api.md).

---

## Documentation

| Document | Contents |
|---|---|
| [`docs/api.md`](docs/api.md) | Full & stateless routes, ingest/search/rerank options, typed tables, document structure, keys |
| [`rust/README.md`](rust/README.md) | Every server environment variable and default (including `KB_MODE=stateless` & `KB_STATELESS_*`) |
| [`docs/kb-tiers.md`](docs/kb-tiers.md) | Tier presets, VRAM budgets, OCR recipes, and ingest seats |
| [`docs/performance.md`](docs/performance.md) | Measured retrieval, throughput, and OCR numbers per tier |
| [`docs/benchmarks.md`](docs/benchmarks.md) | Benchmark runs and evaluation results |
| [`docs/retrieval.md`](docs/retrieval.md) | Hybrid retrieval pipeline, fusion, and tuning knobs |
| [`docs/security.md`](docs/security.md) | Tenant isolation, key scopes, KB limits, and threat model |
| [`docs/local-models.md`](docs/local-models.md) | Running TEI, llama.cpp, and vLLM outside Docker Compose |
| [`rust/deploy/README.md`](rust/deploy/README.md) | Compose stack overrides, GPU notes, and troubleshooting |
| [`docs/rust-cutover.md`](docs/rust-cutover.md) | Migrating from the legacy `v0.1.0` (Bun) service to Rust |
| [`docs/core/PRODUCT.md`](docs/core/PRODUCT.md) | Product scope and non-goals |
| [`docs/core/BACKLOG.md`](docs/core/BACKLOG.md) | Roadmap and shipped milestones |
| [`CHANGELOG.md`](CHANGELOG.md) | Release history |

---

## Development & architecture

The workspace follows a strict hexagonal boundary enforced by `check-boundary`:

- **Inner crates** (`kb-core`, `kb-ports`, `kb-engine`, `kb-eval`) hold pure domain logic and pipelines with zero adapter dependencies.
- **Adapter crates** (`kb-extract`, `kb-inference`, `kb-cache`, `kb-store-pg`, `kb-store-qdrant`, `kb-store-blob`, `kb-store-tables`, `kb-server`) wire external I/O and HTTP delivery.

```bash
./rust/scripts/fetch-pdfium.sh ./rust/.vendor/pdfium
cargo test --manifest-path rust/Cargo.toml --workspace
cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings
cargo fmt --all --manifest-path rust/Cargo.toml --check
cargo run --manifest-path rust/Cargo.toml -p kb-server -- check-boundary

rust/scripts/smoke.sh          # live ingest, search, isolation, and delete checks
rust/scripts/ci-eval-gate.sh   # retrieval-quality and cross-scope leak gate
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for guidelines.

---

## Releases

Tagging `v*` builds and publishes multi-arch images (`amd64`/`arm64`) to `ghcr.io/rantai-dev/rantai-kb` alongside OCR sidecars, packages Linux (`x86_64`, `aarch64`) and macOS (`arm64`) binaries with checksums, cuts a GitHub Release, and smoke-tests `install.sh`.

- **Current release:** `v0.8.0` — stateless processor-only microservice mode (`--stateless` / `KB_MODE=stateless`) with `/v1/process`, `/v1/encode` (query fast-lane), `/v1/rerank` (local & cloud), and `KB_STATELESS_MAX_INFLIGHT` load shedding.
- **Legacy Bun release:** `v0.1.0` ([migration guide](docs/rust-cutover.md)).

---

## License

Apache-2.0. See [`LICENSE`](LICENSE).
