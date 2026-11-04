<div align="center">

<img src="assets/wordmark.svg" alt="RantAI KB" width="300">

**Document ingest and retrieval as a standalone service.**
Upload a PDF, spreadsheet, Word file or code file; get back grounded, cited chunks over HTTP.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Runtime](https://img.shields.io/badge/runtime-Rust-dea584.svg)](https://www.rust-lang.org/)
[![Image](https://img.shields.io/badge/image-ghcr.io%2Frantai--dev%2Frantai--kb-24292e.svg)](https://github.com/RantAI-dev/rantai-kb/pkgs/container/rantai-kb)

</div>

---

Extracted from the RantAI Agents platform, where it has been running in production
across several deployments — a campus knowledge base, a 31-book reference library,
an on-prem GPU install. This repository is the same engine with its own database,
its own API and no application attached, so any product can use it.

The service is written in Rust. The Bun/TypeScript service that shipped as `v0.1.0` was
removed in `v0.2.0`; its release remains at tag [`v0.1.0`](https://github.com/RantAI-dev/rantai-kb/tree/v0.1.0).
Operators moving an old deployment should read [`docs/rust-cutover.md`](docs/rust-cutover.md).

## Why a service

The retrieval engine only ever needed four things: somewhere to put bytes, somewhere
to put vectors, somewhere to put metadata, and an embedding endpoint. Everything
else — accounts, billing, chat UI — belonged to the application around it. Splitting
along that line means one KB can serve a Next.js app, a Rust installer and a
partner's own frontend without any of them sharing a codebase.

## Quick start

`rust/scripts/kb-dev.sh` is the interface to the compose stack. `setup` reads the
hardware, suggests a tier, checks the model licences and writes `rust/deploy/.env`;
`up` starts the tier and waits for `/health`; `key` prints an API key once.

```bash
rust/scripts/kb-dev.sh setup                  # detect the hardware, write .env + .tier
rust/scripts/kb-dev.sh up lite-en             # CPU only; lite-en is the English CPU tier
rust/scripts/kb-dev.sh key my-tenant          # copy the key it prints
```

Then upload a file and search it:

```bash
export KB_URL=http://127.0.0.1:8080
export KB_KEY=<the key you just copied>

# Ingest — POST /v1/documents, multipart field `file`. 202 means accepted, not indexed.
curl -sS -H "Authorization: Bearer $KB_KEY" \
  -F file=@rust/crates/kb-eval/fixtures/tiny/corpus/mission.pdf \
  "$KB_URL/v1/documents"

# Wait for the worker: GET /v1/jobs/<jobId> until status is "success".
curl -sS -H "Authorization: Bearer $KB_KEY" "$KB_URL/v1/jobs/<jobId>"

# Retrieve — POST /v1/search.
curl -sS -H "Authorization: Bearer $KB_KEY" -H 'content-type: application/json' \
  -d '{"query":"seven minutes of terror","maxChunks":5}' \
  "$KB_URL/v1/search"
```

`chunks[].content`, `documentTitle`, `page` and `similarity` carry the results;
`degrades` is non-empty when an arm failed, because a failed arm is reported rather
than silently dropped. `rust/deploy/README.md` has the full command list, overrides
and troubleshooting.

## Tiers

The same `kb-server` binary runs everywhere. `KB_TIER` selects which model serves each
role, and `kb-dev.sh setup` suggests a tier from the hardware. The "Verified" column
says what was actually run, not what is expected.

| Tier | Runs on | Models and access | Verified |
|---|---|---|---|
| `lite` | CPU, 1–2 cores, no GPU | `intfloat/multilingual-e5-small` text embeddings; PP-OCRv6 sidecar for scans | Retrieval measured on 2 cores; **not yet run with the OCR sidecar** |
| `lite-en` | CPU, 1–2 cores, no GPU | `all-MiniLM-L6-v2` (English only); PP-OCRv6 sidecar | **Ran end to end 2026-10-25** |
| `desk` | NVIDIA 8 GB, or Apple silicon 16 GB | Qwen3-Embedding-0.6B + bge-reranker-v2-m3; PaddleOCR-VL | `docker compose config` validated only; **never run on an NVIDIA GPU** |
| `studio` | NVIDIA 24 GB, or Apple silicon 32–36 GB | Qwen3-Embedding-0.6B; Qwen3-VL page embedder and reranker; PaddleOCR-VL | `docker compose config` validated only; **never run on an NVIDIA GPU** |
| `station` | NVIDIA 48 GB, or 128 GB unified | Studio's stack with the 8B Qwen3-VL models in FP8 | `docker compose config` validated only; **never run on an NVIDIA GPU** |
| `cloud` | Any machine | OpenRouter: `voyage-multimodal-3.5` embeddings + `voyageai/rerank-2.5`; page-image arm on | Retrieval measured through the eval harness; requires an `OPENROUTER_API_KEY`; the compose profile itself is not run (`DEP-3`) |

Tier definitions, VRAM estimates and model choices: [`docs/kb-tiers.md`](docs/kb-tiers.md).
Deployment details and GPU notes: [`rust/deploy/README.md`](rust/deploy/README.md).

## Measured results

These numbers are copied from the committed run folders. Each folder has its script,
queries and per-query output.

**Cloud tier vs KF main** ([`docs/eval-ab/rk-cloud-vs-kf/`](docs/eval-ab/rk-cloud-vs-kf/),
run 2026-10-31, nDCG@10, every query answered):

| Dataset | Queries | KF main | rk-cloud-text | rk-cloud |
|---|---:|---:|---:|---:|
| MIRACL-id-200 (document) | 200 | 0.813 | — | **0.845** |
| ViDoRe v3 HR (page) | 318 | not run yet | 0.675 | **0.710** |
| ViDoRe v3 CS (page) | 215 | not run yet | 0.778 | **0.822** |

Paired bootstrap, 95% CI: MIRACL-id-200 +0.032 [+0.006, +0.057]; ViDoRe HR
text-only → multimodal +0.035 [+0.018, +0.052]; ViDoRe CS +0.044 [+0.030, +0.059].
The KF comparison is against KF's default stack, and KF has no ViDoRe arm yet.

**Lite on 2 CPU cores** ([`docs/eval-ab/lite-2core/`](docs/eval-ab/lite-2core/),
2026-10-24):

| Lite, MIRACL-id-50 | nDCG@10 | Recall@10 | MRR@10 | Ingest 1,485 passages | Search p50 / p95 |
|---|---:|---:|---:|---:|---:|
| `intfloat/multilingual-e5-small` (384-d) | 0.802 | 0.938 | 0.823 | ~126 s | 42 / 49 ms |

ViDoRe HR on the same two cores: `lite` (e5-small) ingests at 1.55 pages/s with page
nDCG@10 0.545; `lite-en` (MiniLM with a 512-character embedding cap) ingests at
6.43 pages/s with page nDCG@10 0.517. MiniLM is English-only: on Indonesian
MIRACL-id-50 it scores 0.679 against e5-small's 0.802.

## API

All routes are under `/v1` and authenticate with `Authorization: Bearer <key>` (or
`X-Api-Key`). Health and formats are unauthenticated. Every key belongs to exactly one
tenant; **there is no route that reads across tenants.**

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/documents` | Upload a file (multipart field `file`). Returns `202` with a `jobId`. |
| `GET` | `/v1/documents` | List documents. Filters: `knowledgeBaseIds`, `externalRef`, `limit`. |
| `GET` `PATCH` `DELETE` | `/v1/documents/:id` | Read, edit or delete one document. |
| `GET` | `/v1/documents/:id/raw` | Download the stored original. |
| `GET` | `/v1/documents/:id/assets` | Fetch a document figure or page image. |
| `GET` | `/v1/documents/:id/intelligence` | Extracted entities and relations. |
| `POST` | `/v1/search` | Retrieve chunks for a query. The reason this service exists. |
| `GET` | `/v1/jobs/:id` | Ingest progress: step, percentage, ETA, error. |
| `POST` | `/v1/jobs/:id/retry` | Retry a failed ingest job. |
| `GET` `POST` | `/v1/knowledge-bases` | List or create knowledge bases. |
| `PATCH` `DELETE` | `/v1/knowledge-bases/:id` | Edit or delete a knowledge base. |
| `GET` `POST` | `/v1/categories` | List or create categories. |
| `PATCH` `DELETE` | `/v1/categories/:id` | Edit or delete a category. |
| `GET` | `/v1/events` | Server-sent ingest progress for the tenant. |
| `GET` | `/v1/formats` | Accepted extensions and the size limit. No auth. |
| `GET` | `/health` · `/v1/health` | Liveness, dependency checks, build version, tier and egress hosts. No auth. |

**Ingest options** (multipart fields): `title`, `categories`, `subcategory`,
`knowledgeBaseIds`, `externalRef`, and `figures` — `auto` (default), `force` or `skip`.

**Search options** (JSON): `query` (required), `maxChunks`, `knowledgeBaseIds`,
`documentIds`, `category`, `hybrid`, `format`.

## What it does with a file

Extraction is chosen per file type. PDFs use pdfium for the text layer and a layout
parser or OCR for scans; Office Open XML, OpenDocument, text, Markdown, code,
CSV/TSV/JSON, HTML/XML, RTF and EPUB have native paths. About 30 file types are
accepted. Legacy binary Office files (`.doc`, `.xls`, `.ppt`) are accepted but do not
extract yet (`ING-1`).

Chunking is table- and code-aware: a fixed-size splitter cuts spreadsheet rows in half,
which is the single biggest cause of weak table answers.

## Retrieval

Per query, the pipeline is:

```
query
 ├─ query expansion → N paraphrases          (opt-in)
 ├─ dense vector search ┐
 ├─ BM25                ├─ run in parallel
 └─ entity arm          ┘
      ↓ resolve BM25 hits through the scoped document store
      ↓ Reciprocal Rank Fusion (k = 60)
      ↓ cross-encoder rerank                    (opt-in, tier dependent)
      ↓ neighbour-window expansion, ±1 adjacent chunk
      ↓ page-image arm and figure co-retrieval
      ↓ stable source numbering
```

Four parts are worth knowing before tuning anything.

**Hybrid is not an ensemble.** Dense embeddings are strong on paraphrase and weak on
rare tokens — part numbers, regulation codes, internal acronyms — because a vector
averages meaning and rare tokens sink. BM25 is strong exactly there and blind to
paraphrase. The failure modes are uncorrelated; that is the reason to run both.

**Fusion happens on ranks, not scores.** A cosine score and a BM25 score are not
commensurable, and any normalisation between them has to be re-tuned every time the
embedding model changes. RRF sidesteps that.

**BM25 hits are not trustworthy until they are joined.** The full-text index has no
notion of category, group or ownership. Every hit is resolved back to its parent
document through the scoped document store, so a keyword arm cannot leak chunks from
a document the caller cannot see.

**Neighbour-window expansion is cheap and load-bearing.** `KB_NEIGHBOR_WINDOW=1` pulls
±1 adjacent chunk around each hit, woven into reading order next to its anchor and
sharing the anchor's source card, so no new citation numbers appear. A retrieved table
travels with its explanation, and an explanation pulls in its table.

**Page images.** Scans, charts, slides and tables can also be embedded as whole pages.
On the measured cloud run the page arm adds +0.035 page nDCG@10 on ViDoRe HR and
+0.044 on CS over text-only; both CIs are above zero. The method and its evaluation are
published in [IKAT](https://github.com/RantAI-dev/ikat).

## Security model, in plain words

- **Tenants.** Every API key belongs to exactly one tenant. Every search arm, document
  route and asset route is filtered to that tenant. A CI gate probes the retrieval arms
  for cross-tenant leaks.
- **Key scopes.** A key carries `kb:read` and/or `kb:write`. A key with neither is
  rejected. `kb-dev.sh key <tenant>` mints an unrestricted key; the CLI accepts
  `--scopes kb:read,kb:write` and `--kb id,id` for a read-only or knowledge-base-limited
  key (`kb-server create-api-key --help`).
- **Knowledge-base limit.** A key can be limited to a set of knowledge bases. The limit
  applies to search, the document list, upload and metadata edits.
- **Known gap.** The by-ID document routes check only the tenant, so a knowledge-base-
  limited key can still open a document outside its knowledge bases if it knows the ID
  (`SEC-1`, being fixed before the release).
- **What it does not do.** There are no users, groups, roles or folders. Permissions
  belong to API keys, not people; a calling app decides who may see what and hands out
  keys or knowledge-base filters to match. There are no per-document permissions inside
  a knowledge base. See [`docs/core/PRODUCT.md` §3](docs/core/PRODUCT.md#3-what-it-does-not-do).

Report a vulnerability privately; see [`SECURITY.md`](SECURITY.md).

## Configuration

`rust/deploy/.env` covers the normal cases and is written by `kb-dev.sh setup`. Every
setting the server reads, with its purpose and default, is in
[`rust/README.md`](rust/README.md). The essentials:

- `DATABASE_URL` — Postgres, for documents, jobs, knowledge bases, keys and the graph
- `QDRANT_GRPC_URL` — Qdrant, for chunks, vectors, BM25 and page images
- `S3_*` — any S3-compatible store (rustfs, MinIO, AWS)
- `KB_EMBEDDING_*` — any OpenAI-compatible embeddings endpoint (OpenRouter, TEI, vLLM)
- `KB_TIER` — the tier preset; an explicit setting always wins over it
- `KB_CACHE_DIR` — content-addressed embedding, OCR and caption cache

Defaults are deliberately quiet. Query expansion, contextual retrieval, reranking,
citation grounding and the vision figure gate are off unless the tier or the operator
turns them on, because each adds a model call to the answer path.

## Architecture

The engine talks to the outside world through ports, defined in `rust/crates/kb-ports`.
`kb-core` holds the pure logic, `kb-engine` holds the ingest and retrieval pipelines, and
the adapters (`kb-inference`, `kb-store-pg`, `kb-store-qdrant`, `kb-store-blob`,
`kb-extract`) bind those ports to real infrastructure. `kb-server` is the composition
root and the HTTP API.

`kb-core` does no I/O and `kb-engine` never names an adapter. Two checks enforce that —
`kb-server check-boundary` and the scope-token check in CI — because the boundary is what
lets the same engine run embedded inside a host application and standalone here.

## Development

```bash
./rust/scripts/fetch-pdfium.sh ./rust/.vendor/pdfium
cargo test --manifest-path rust/Cargo.toml --workspace
cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings
cargo fmt --all --manifest-path rust/Cargo.toml --check
cargo run --manifest-path rust/Cargo.toml -p kb-server -- check-boundary
```

Live checks:

```bash
rust/scripts/smoke.sh          # ingest, search, isolation, deletes against a live stack
rust/scripts/ci-eval-gate.sh   # retrieval-quality and cross-scope leak gate
```

Contributions: [`CONTRIBUTING.md`](CONTRIBUTING.md). Work is tracked in
[`docs/core/BACKLOG.md`](docs/core/BACKLOG.md).

## Releases

Tagging `v*` builds the Rust image from `rust/Dockerfile` for amd64 and arm64, publishes
it to `ghcr.io/rantai-dev/rantai-kb` and cuts a GitHub release. `/health` reports the
build version. `v0.2.0` is the first Rust release; `v0.1.0` is the last Bun release.
Changes are in [`CHANGELOG.md`](CHANGELOG.md).

## License

Apache-2.0. See [`LICENSE`](LICENSE).
