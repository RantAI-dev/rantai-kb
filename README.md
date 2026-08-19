# RantAI KB

Document ingest and retrieval as a standalone service. Upload a PDF, spreadsheet, Word file or code file; get back grounded, cited chunks over HTTP.

Extracted from the [RantAI Agents](https://github.com/RantAI-dev/RantAI-Agents) platform, where it has been running in production across several deployments (a campus knowledge base, a 31-book reference library, an on-prem GPU install). This repository is the same engine with its own database, its own API and no application attached — so any product can use it, not just ours.

## Why a service

The retrieval engine only ever needed four things: somewhere to put bytes, somewhere to put vectors, somewhere to put metadata, and an embedding endpoint. Everything else — accounts, billing, chat UI — belonged to the application around it. Splitting along that line means one KB can serve a Next.js app, a Rust installer and a partner's own frontend without any of them sharing a codebase.

## Quick start

```bash
cp .env.example .env          # set KB_EMBEDDING_API_KEY (any OpenAI-compatible endpoint)
docker compose up -d
docker compose exec kb bun scripts/create-api-key.ts acme
```

That prints a key once. Then:

```bash
KEY="rkb_…"

# Ingest — returns 202 immediately; a worker does the heavy lifting
curl -X POST http://localhost:8080/v1/documents \
  -H "Authorization: Bearer $KEY" \
  -F file=@handbook.pdf -F title="Employee Handbook"

# Retrieve
curl -X POST http://localhost:8080/v1/search \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"query":"how much parental leave do I get?","maxChunks":5}'
```

## API

All routes are under `/v1` and authenticate with `Authorization: Bearer <key>` (or `X-Api-Key`). Every key belongs to exactly one tenant; there is no route that reads across tenants.

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/documents` | Upload a file (multipart). Returns `202` with a `jobId`. |
| `GET` | `/v1/documents` | List documents, optionally filtered by knowledge base. |
| `DELETE` | `/v1/documents/:id` | Delete a document, its chunks and its stored file. |
| `POST` | `/v1/search` | Retrieve chunks for a query. |
| `GET` | `/v1/jobs/:id` | Ingest progress: step, percentage, ETA, error. |
| `GET` | `/v1/knowledge-bases` | List knowledge bases. `POST` to create one. |
| `GET` | `/v1/events` | Server-sent ingest progress for the tenant. |
| `GET` | `/v1/formats` | Accepted extensions and the size limit. |
| `GET` | `/health` | Liveness + dependency checks. No auth. |

**Ingest options** (multipart fields): `title`, `categories`, `subcategory`, `knowledgeBaseIds`, `externalRef`, and `figures` — `auto` (default), `force` or `skip`.

`figures` is the one knob worth understanding. Scanned PDFs go through a layout parser that crops figures and tables so they can be retrieved and rendered; text PDFs skip it because it is slow and buys nothing. `force` runs it anyway, which is what you want for a text-layer textbook full of charts. Every other file type ignores the option — a spreadsheet has no figures to crop.

**Search options** (JSON): `query` (required), `maxChunks`, `knowledgeBaseIds`, `documentIds`, `category`, `hybrid`, `format`.

## What it does with a file

Extraction is chosen per file type rather than applied uniformly:

| Type | Extraction | Chunking | Figures | Entities |
|---|---|---|---|---|
| PDF (scanned) | layout parser (MinerU / Mistral OCR) | table-aware | yes | yes |
| PDF (text) | fast per-page, keeps a page map | table-aware | on request | yes |
| xlsx / csv / ods | sheet → CSV | table-aware | no | no |
| docx / pptx / odt / rtf / epub | native parsers | table-aware | no | yes |
| md / txt / html | direct | table-aware | no | yes |
| code / config | fenced blocks | table-aware | no | no |
| images | OCR | table-aware | no | no |

Chunking is always table- and code-aware: a fixed-size splitter cuts spreadsheet rows in half, which is the single biggest cause of weak table answers.

## Configuration

`.env.example` covers the common cases. The full surface — all 55 variables with purpose and default — is declared in [`src/lib/rag/config-surface.ts`](src/lib/rag/config-surface.ts), and a test fails if the code reads one that isn't declared there.

The essentials:

- `DATABASE_URL` — Postgres, for document metadata and jobs
- `SURREAL_DB_URL` — SurrealDB **v2** (the client rejects the 3.x handshake), for vectors and the entity graph
- `S3_*` — any S3-compatible store (MinIO, RustFS, AWS)
- `KB_EMBEDDING_*` — any OpenAI-compatible embeddings endpoint (OpenRouter, TEI, vLLM)
- `KB_EMBEDDING_DIM` must match the vector index; changing it means re-embedding

Optional: `KB_EXTRACT_MINERU_BASE_URL`, `KB_MINERU_API_KEY`, `KB_MISTRAL_OCR_KEY` for scanned-PDF layout extraction; `KB_RERANK_*` for reranking; `KB_HYBRID_BM25_ENABLED` for the full-text arm.

### Optional extras

The legacy OCR path rasterises PDF pages locally and needs two packages that
are deliberately **not** installed by default — `canvas` (a transitive
dependency) has no prebuilt binary for current Node ABIs and needs cairo and
pixman to compile, which is a poor trade for a fallback most deployments never
reach. Scanned PDFs normally go to MinerU or Mistral OCR instead.

If you do want it:

```bash
bun add pdf-img-convert sharp
```

Without them, `processDocumentOCR` on a scanned PDF returns an error naming the
missing package rather than failing obscurely.

## Scaling

One process serves the API and runs the ingest worker. Jobs are claimed with `FOR UPDATE SKIP LOCKED`, so running more replicas is the whole scaling story — no queue broker, no separate worker deployment. Set `KB_WORKER_ENABLED=false` on a replica to make it API-only.

## Architecture

The engine talks to the outside world through eight interfaces — blob storage, vectors, documents, jobs, progress, config, endpoints, job execution — defined in [`src/lib/kb-runtime/ports.ts`](src/lib/kb-runtime/ports.ts). `src/service/adapters.ts` binds them to this service's infrastructure. Nothing under `src/lib` may import anything under `src/service`; `bun run check:kb-boundary` enforces it in CI.

That constraint is what lets the same engine run embedded inside a host application and standalone here.

## Development

```bash
bun install
bunx prisma generate
docker compose up -d postgres surrealdb minio createbucket
bunx prisma migrate deploy
bun run kb:apply-schema        # SurrealDB tables + HNSW + FTS index
bun run dev
```

Checks:

```bash
bun run check:kb-boundary      # engine imports no infrastructure
bun run typecheck
bun run test
bun tests/smoke/round-trip.ts  # ingest + retrieve + tenant isolation, needs the stack up
```

## Releases

Tagging `v*` builds a multi-arch image (amd64 + arm64 — the on-prem boxes are aarch64), publishes it to `ghcr.io/rantai-dev/rantai-kb`, and cuts a GitHub release.

## License

Apache-2.0.
