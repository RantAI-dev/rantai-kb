<div align="center">

<img src="assets/wordmark.svg" alt="RantAI KB" width="300">

**Document ingest and retrieval as a standalone service.**
Upload a PDF, spreadsheet, Word file or code file; get back grounded, cited chunks over HTTP.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Runtime](https://img.shields.io/badge/runtime-Rust-dea584.svg)](https://www.rust-lang.org/)
[![Image](https://img.shields.io/badge/image-ghcr.io%2Frantai--dev%2Frantai--kb-24292e.svg)](https://github.com/RantAI-dev/rantai-kb/pkgs/container/rantai-kb)

</div>

---

Extracted from the RantAI Agents platform, where it has been running in production across
several deployments — a campus knowledge base, a 31-book reference library, an on-prem GPU
install. This is the same engine with its own database, its own API and no application
attached, so any product can use it: one KB serves a Next.js app, a Rust installer and a
partner's frontend without sharing a codebase.

## Quick start

```bash
curl -fsSL https://raw.githubusercontent.com/RantAI-dev/rantai-kb/main/install.sh | sh

rantai-kb setup lite      # detect the hardware, write deploy/.env + deploy/.tier
rantai-kb up lite         # start the containers and wait for /health
rantai-kb key my-tenant   # print an API key, once
```

The installer puts the binary in `~/.rantai-kb/bin`, links `rantai-kb` (and `kb-server`,
the same program under its other name) into `~/.local/bin`, and keeps the compose stack in
`~/.rantai-kb/deploy`. Add `~/.local/bin` to your `PATH` if it is not there already.

`setup` picks the tier the machine can actually run, checks the VRAM budget and the model
licences, and refuses a tier that does not fit. `up` needs Docker and compose ≥ 2.20; the
first start downloads the models, so it can take a few minutes. Then:

```bash
curl -sS -H "Authorization: Bearer $KB_KEY" \
  -F file=@mission.pdf http://127.0.0.1:8080/v1/documents        # 202 + jobId
curl -sS -H "Authorization: Bearer $KB_KEY" \
  -d '{"query":"seven minutes of terror","maxChunks":5}' \
  -H 'content-type: application/json' http://127.0.0.1:8080/v1/search
```

The API page (every endpoint, with a "try it" button) is at
[`http://127.0.0.1:8080/docs`](http://127.0.0.1:8080/docs); the document behind it is
`/openapi.json`. From a source checkout, `rust/scripts/kb-dev.sh` is the same interface
without the install step.

## Tiers, hardware and models

A tier is a preset: which model serves each role, and which containers start. Any explicit
`KB_*` setting wins over the preset, so a tier is a starting point, not a mode.

Any role can also move off the tier: `setup --ocr mistral` (hosted OCR), `--ocr <url>` (a
shared OCR server), `--vision off|<url>`, or an external embedder and reranker.

### Minimum and recommended hardware

Measured for the CPU tiers, estimated for the rest (their numbers have to be measured on
their real hardware before we claim them).

| Tier | Minimum | Recommended | CPU only |
|---|---|---|---|
| `lite` | 2 cores, 4 GB RAM, 5 GB disk | 4 cores, 8 GB RAM, 10 GB disk | ✓ |
| `lite-en` | 2 cores, 2 GB RAM, 5 GB disk | 4 cores, 4 GB RAM, 10 GB disk | ✓ |
| `desk` | NVIDIA 8 GB, 4 cores, 8 GB RAM, 15 GB disk | 12 GB VRAM, 8 cores, 16 GB RAM, 25 GB disk | — |
| `studio` | NVIDIA 24 GB, 8 cores, 16 GB RAM, 25 GB disk | 32 GB VRAM, 12 cores, 32 GB RAM, 40 GB disk | — |
| `station` | NVIDIA 48 GB, 12 cores, 32 GB RAM, 50 GB disk | 80 GB VRAM, 16 cores, 64 GB RAM, 100 GB disk | — |
| `cloud` | 2 cores, 2 GB RAM, 5 GB disk | 4 cores, 4 GB RAM | ✓ |

- **The GPU is shared with your app's chat LLM, and chat comes first.** `setup` sizes the
  tier for that: Desk is ≈3.5 GB chat + ≈4.5 GB KB in 8 GB, with no slack; Studio ≈9 + ≈10
  of 24; Station ≈26 + ≈20 of 48. A bigger chat model means the next tier up, or moving a
  role to another host.
- **Disk** is the model files the containers download plus ~2.5 GB of images and the data
  itself. `lite` weighs ~3 GB before documents; `station` downloads ~41 GB of weights,
  because the 8B models are stored in FP16 and quantised to FP8 when they load.
- **Apple silicon** (Desk ≥16 GB, Studio ≥32, Station ≥128 unified memory) is detected and
  suggested by `setup`, but compose only reserves NVIDIA GPUs: the models run outside the
  stack, and `setup` says so.
- **No Windows** on any tier. The container tiers need the NVIDIA driver plus
  `nvidia-container-toolkit` on the host.

### What each tier runs

**CPU tiers.** No GPU, no paid key.

| Role | `lite` | `lite-en` |
|---|---|---|
| Text + chunk embeddings | `intfloat/multilingual-e5-small` — 118M, 384-d, TEI (CPU), 471 MB | `sentence-transformers/all-MiniLM-L6-v2` — 22M, 384-d, TEI (CPU), 91 MB |
| Scanned-page OCR | PP-Layout CDLA (~10M) + PP-OCRv6 tiny (family: 1.5–34.5M) + SLANet-plus tables (~7M), ONNX in the `ocr-cpu` sidecar | same |
| Pictures → description | `Qwen3-VL-2B-Instruct`, Q4_K_M GGUF, llama.cpp (`vlm-cpu`) — 1.11 GB + 445 MB projector | same |
| Reranker | none | none |
| Page-image search | off | off |

`lite-en` is English-only: it is ~4× faster per page and loses 0.12 nDCG on Indonesian.
Use `lite` for a mixed or non-English corpus.

**Local GPU tiers.** **One embedder for text and pages** — not two — and one reranker.

| Role | `desk` (8 GB) | `studio` (24 GB) | `station` (48 GB) |
|---|---|---|---|
| **Embeddings: text chunks *and* page images** | `nvidia/Nemotron-3.5-Embed-1B-BF16` — 1B, **2048-d, one vector space**, vLLM, 2.4 GB | same | same |
| Reranker | `Qwen/Qwen3-Reranker-0.6B` — 0.6B, llama.cpp `/v1/rerank`, 1.19 GB | same | same |
| Scanned-page OCR | `xiaomi-research/Xiaomi-OCR-0` — 0.8B, FP8, vLLM, 1.75 GB | same | same |
| Pictures → description | `Qwen3-VL-2B-Instruct`, Q4_K_M, llama.cpp CUDA, 1.11 GB | same | same |

One model serves both modalities, so there is **no text encoder beside an image encoder and no
fusion weight to tune**: a page and a paragraph are vectors in the same space, and one query
embedding searches both. Measured on ViDoRe HR: text 0.611, image 0.626, one space **0.621** —
every arm within ±0.015, against 0.277 for the two-encoder Qwen3-VL stack, and text on
MIRACL-id **0.826** against 0.796 ([`docs/eval-ab/one-embedder`](docs/eval-ab/one-embedder/README.md)).
The reranker replaces the VL rerankers that were 4.26 GB and 17.5 GB and had never been measured;
at 1.19 GB it lifts nDCG@10 **0.795 → 0.850** on Indonesian
([`docs/eval-ab/rerank-2026-11-17`](docs/eval-ab/rerank-2026-11-17/README.md)).

**How to select it.** `KB_EMBEDDERS=one` (documented in the bundle's `.env.example`). The 0.4.0
default is still the previous two-model stack until the last two gate measurements land — the page
reranker on top of the one-space list and ingest throughput at 32k context — so the switch is
explicit rather than silent ([`docs/core/specs/dep-6.md`](docs/core/specs/dep-6.md)).

The optional table pass (`--ocr-tables accurate`) adds the `ocr-layout` sidecar:
PP-DocLayoutV3, ~130 MB, on the CPU — **never on the same card as the OCR model**, which
crashed or hung vLLM in a full benchmark.

**Cloud tier.** Nothing local; two API keys at most.

| Role | Model | Spec |
|---|---|---|
| Text, chunk and page embeddings | `voyageai/voyage-multimodal-3.5` | hosted, 1024-d, one vector space for text and pages |
| Reranker | `voyageai/rerank-2.5` | hosted, Cohere-shaped rerank |
| Scanned-page OCR | Mistral OCR | hosted, billed per page |
| Pictures → description | `nvidia/nemotron-3.5-super-vl` | hosted |

Every local model above is Apache-2.0 or MIT — no commercial opt-in, no per-page cost, and
scans never leave the machine. Hosted models are governed by their provider's terms. The
verdicts, and the alternatives that were rejected on licence grounds, are in
[`docs/model-licences.md`](docs/model-licences.md); the reasoning behind each choice, the
VRAM arithmetic and the measured OCR comparison are in [`docs/kb-tiers.md`](docs/kb-tiers.md).

**How verified each tier is.** Lite and lite-en were run end to end, including OCR on two
cores. The GPU tiers' embedding and reranking were measured on a GB10 through the same harness
the eval runs use (the numbers above), and their compose profiles are `docker compose config`-
validated and unit-tested — but **never run on a Desk-sized card**. Numbers per tier:
[`docs/performance.md`](docs/performance.md).

## What it does with a file

Extraction is chosen per file type: PDFs use pdfium for the text layer, with a layout parser
or OCR for scans; Office Open XML, OpenDocument, text, Markdown, code, CSV/TSV/JSON,
HTML/XML, RTF and EPUB have native paths. 55 extensions are accepted — `GET /v1/formats`
lists them, with the size limit. Legacy binary Office files (`.doc`, `.xls`, `.ppt`) are
accepted but do not extract yet (`ING-1`).

Chunking is table- and code-aware: a fixed-size splitter cuts spreadsheet rows in half,
which is the single biggest cause of weak table answers. Retrieved tables come back as typed
grids you can query for numbers, not just text — see [`docs/api.md`](docs/api.md#tables).

## API

Every route is under `/v1` and takes `Authorization: Bearer <key>`. The three that matter
first are `POST /v1/documents`, `GET /v1/jobs/:id` and `POST /v1/search`; `GET /docs` is the
Swagger UI and `GET /openapi.json` the contract. The full route table, ingest and search
options, the table API and the structure read are in [`docs/api.md`](docs/api.md).

Tenant isolation, key scopes and the knowledge-base limit are in
[`docs/security.md`](docs/security.md). How the arms are fused, and what to tune before
blaming recall, is in [`docs/retrieval.md`](docs/retrieval.md).

## Documentation

| Document | What is in it |
|---|---|
| [`docs/api.md`](docs/api.md) | Every route, options, tables, document structure, keys |
| [`docs/kb-tiers.md`](docs/kb-tiers.md) | Tier decisions, VRAM estimates, OCR recipes, ingest seats |
| [`docs/performance.md`](docs/performance.md) | Measured retrieval, throughput and OCR numbers per tier |
| [`docs/benchmarks.md`](docs/benchmarks.md) | Every benchmark we have run, its result and its run folder |
| [`docs/retrieval.md`](docs/retrieval.md) | The retrieval pipeline and every knob that changes it |
| [`docs/security.md`](docs/security.md) | Tenants, scopes, the knowledge-base limit, known gaps |
| [`docs/local-models.md`](docs/local-models.md) | Running the models without compose (TEI, llama.cpp, vLLM) |
| [`rust/deploy/README.md`](rust/deploy/README.md) | The compose stack, overrides, GPU notes, troubleshooting |
| [`rust/README.md`](rust/README.md) | Every setting the server reads, with its default |
| [`docs/rust-cutover.md`](docs/rust-cutover.md) | Moving a v0.1.0 (Bun) deployment to Rust |
| [`docs/core/PRODUCT.md`](docs/core/PRODUCT.md) | What it is for, what it does not do |
| [`docs/core/BACKLOG.md`](docs/core/BACKLOG.md) | The one list of work, and what shipped |
| [`CHANGELOG.md`](CHANGELOG.md) | What changed in each release |

## Development

```bash
./rust/scripts/fetch-pdfium.sh ./rust/.vendor/pdfium
cargo test --manifest-path rust/Cargo.toml --workspace
cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings
cargo fmt --all --manifest-path rust/Cargo.toml --check
cargo run --manifest-path rust/Cargo.toml -p kb-server -- check-boundary

rust/scripts/smoke.sh          # ingest, search, isolation, deletes against a live stack
rust/scripts/ci-eval-gate.sh   # retrieval-quality and cross-scope leak gate
```

The engine talks to the outside world through ports (`rust/crates/kb-ports`): `kb-core`
holds the pure logic, `kb-engine` the pipelines, and the adapters bind those ports to
Postgres, Qdrant, S3, the models and the extractors. `kb-core` does no I/O and `kb-engine`
never names an adapter; two checks enforce that, because the boundary is what lets the same
engine run embedded in a host application and standalone here.

Contributions: [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Releases

Tagging `v*` builds the image for amd64 and arm64 on native runners, publishes it to
`ghcr.io/rantai-dev/rantai-kb` with the OCR sidecar images, packages the binary for Linux
(x86_64, aarch64) and macOS (arm64) as tarballs with their checksums, cuts a GitHub release,
and then installs that release with the one-liner on Linux and macOS to prove it works.
`/health` reports the build version. `v0.4.0` is the current release (it adds the opt-in
one-embedder stack, `KB_EMBEDDERS=one`); `v0.1.0` is the last
Bun one ([`docs/rust-cutover.md`](docs/rust-cutover.md)).

An installed bundle has no source, so its `deploy/.env` points at the published images
(`KB_IMAGE`, `KB_OCR_CPU_IMAGE`, `KB_OCR_LAYOUT_IMAGE`) instead of building them.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
