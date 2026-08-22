<div align="center">

<img src="assets/wordmark.svg" alt="RantAI KB" width="300">

**Document ingest and retrieval as a standalone service.**
Upload a PDF, spreadsheet, Word file or code file; get back grounded, cited chunks over HTTP.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Runtime](https://img.shields.io/badge/runtime-Bun-000.svg)](https://bun.sh)
[![Image](https://img.shields.io/badge/image-ghcr.io%2Frantai--dev%2Frantai--kb-24292e.svg)](https://github.com/RantAI-dev/rantai-kb/pkgs/container/rantai-kb)

</div>

---

Extracted from the RantAI Agents platform, where it has been running in production
across several deployments — a campus knowledge base, a 31-book reference library,
an on-prem GPU install. This repository is the same engine with its own database,
its own API and no application attached, so any product can use it.

## Why a service

The retrieval engine only ever needed four things: somewhere to put bytes, somewhere
to put vectors, somewhere to put metadata, and an embedding endpoint. Everything
else — accounts, billing, chat UI — belonged to the application around it. Splitting
along that line means one KB can serve a Next.js app, a Rust installer and a
partner's own frontend without any of them sharing a codebase.

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

All routes are under `/v1` and authenticate with `Authorization: Bearer <key>` (or
`X-Api-Key`). Every key belongs to exactly one tenant; **there is no route that reads
across tenants.**

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/documents` | Upload a file (multipart). Returns `202` with a `jobId`. |
| `GET` | `/v1/documents` | List documents, optionally filtered by knowledge base. |
| `DELETE` | `/v1/documents/:id` | Delete a document, its chunks and its stored file. |
| `POST` | `/v1/search` | Retrieve chunks for a query. The reason this service exists. |
| `GET` | `/v1/jobs/:id` | Ingest progress: step, percentage, ETA, error. |
| `GET` | `/v1/knowledge-bases` | List knowledge bases. `POST` to create one. |
| `GET` | `/v1/events` | Server-sent ingest progress for the tenant. |
| `GET` | `/v1/formats` | Accepted extensions and the size limit. |
| `GET` | `/health` | Liveness, dependency checks and build version. No auth. |

**Ingest options** (multipart fields): `title`, `categories`, `subcategory`,
`knowledgeBaseIds`, `externalRef`, and `figures` — `auto` (default), `force` or `skip`.

`figures` is the one knob worth understanding. Scanned PDFs go through a layout parser
that crops figures and tables so they can be retrieved and rendered; text PDFs skip it
because it is slow and buys nothing. `force` runs it anyway, which is what you want for
a text-layer textbook full of charts. Every other file type ignores the option — a
spreadsheet has no figures to crop.

**Search options** (JSON): `query` (required), `maxChunks`, `knowledgeBaseIds`,
`documentIds`, `category`, `hybrid`, `format`.

## What it does with a file

Extraction is chosen per file type rather than applied uniformly. The default
(`KB_EXTRACT_PRIMARY=smart`) runs a **smart router**: `unpdf` goes first at ~50 ms to
read the text-layer signals, and only a PDF that actually needs a layout parser pays
for one.

| Type | Extraction | Chunking | Figures | Entities |
|---|---|---|---|---|
| PDF (scanned) | layout parser (MinerU sidecar / MinerU API / Mistral OCR) | table-aware | yes | yes |
| PDF (text) | fast per-page, keeps a page map | table-aware | on request | yes |
| xlsx / csv / ods | sheet → CSV | table-aware | no | no |
| docx / pptx / odt / rtf / epub | native parsers | table-aware | no | yes |
| md / txt / html | direct | table-aware | no | yes |
| code / config | fenced blocks | table-aware | no | no |
| images | OCR | table-aware | no | no |

Chunking is always table- and code-aware: a fixed-size splitter cuts spreadsheet rows
in half, which is the single biggest cause of weak table answers.

## Retrieval

Per query, the pipeline is:

```
query
 ├─ query expansion → N paraphrases          (opt-in)
 ├─ vector search  ┐
 ├─ BM25           ├─ run in parallel — max, not sum, of the two latencies
 └─ entity arm     ┘
      ↓ resolve BM25 hits through the scoped DocumentStore
      ↓ Reciprocal Rank Fusion (k = 60)
      ↓ cross-encoder rerank, 30 candidates → 8                (opt-in)
      ↓ neighbour-window expansion, ±1 adjacent chunk
      ↓ figure co-retrieval by anchor, then the figure policy gate
      ↓ stable source numbering
      ↓ answer
      ↓ citation grounding report                              (opt-in)
```

Four parts of that are worth knowing before you tune anything.

**Hybrid is not an ensemble.** Dense embeddings are strong on paraphrase and weak on
rare tokens — part numbers, regulation codes, internal acronyms — because a vector
averages meaning and rare tokens sink. BM25 is strong exactly there and blind to
paraphrase. The failure modes are uncorrelated; that is the reason to run both.

**Fusion happens on ranks, not scores.** A cosine score and a BM25 score are not
commensurable, and any normalisation between them has to be re-tuned every time the
embedding model changes. RRF sidesteps that entirely.

**BM25 hits are not trustworthy until they are joined.** The full-text index has no
notion of category, group or ownership, so its arm queries the whole chunk table. Every
hit is resolved back to its parent document through `DocumentStore`, which is scoped
(soft-deleted rows excluded, tenant-scoped here). Skipping that join used to leak chunks
from documents the caller cannot see — visible as sources with an empty title.

**Neighbour-window expansion is cheap and load-bearing.** `KB_NEIGHBOR_WINDOW=1` pulls
±1 adjacent chunk around each hit, woven into reading order next to its anchor and
sharing the anchor's source card, so no new citation numbers appear. A retrieved table
travels with its explanation, and an explanation pulls in its table.

### Figures

Figure chunks are thin captions appended at the end of a document, so a specific query
surfaces a topic's prose but never its picture. Co-retrieval fixes that by using the
**anchor keys of the text chunks that were actually retrieved**: figures belonging to a
retrieved passage come first, caption overlap fills the rest. The method and its
evaluation are published in [IKAT](https://github.com/RantAI-dev/ikat).

`KB_VLM_AT_ANSWER_ENABLED` additionally shows surviving crops to a vision model on the
answer path. It is off by default because it adds a per-figure model call.

## Configuration

`.env.example` covers the common cases. The full surface — every variable with purpose
and default — is declared in [`src/lib/rag/config-surface.ts`](src/lib/rag/config-surface.ts),
and **a test fails if the code reads one that isn't declared there.** Config drift is
the silent killer in a retrieval system, so it is a CI failure here rather than a stale
docs table.

The essentials:

- `DATABASE_URL` — Postgres, for document metadata and jobs
- `SURREAL_DB_URL` — SurrealDB **v2** (the client rejects the 3.x handshake), for vectors and the entity graph
- `S3_*` — any S3-compatible store (MinIO, RustFS, AWS)
- `KB_EMBEDDING_*` — any OpenAI-compatible embeddings endpoint (OpenRouter, TEI, vLLM)
- `KB_EMBEDDING_DIM` must match the vector index; changing it means re-embedding

### Defaults are deliberately quiet

Query expansion, contextual retrieval, reranking, citation grounding, the intent
classifier and the VLM figure gate are all **off unless you turn them on**. Each adds a
model call to the answer path. The default should be the cheapest and most predictable
configuration; switching one on is an operator's decision about latency and cost.

Two more defaults that surprise people:

- **`KB_VECTOR_KNN` is `false`.** The schema defines an MTREE index on the embedding
  column, but the default query path runs an exact cosine scan. At current corpus sizes
  the scan is not the bottleneck and it gives exact recall; the ANN operator is one flag
  away when that stops being true.
- **The entity/graph arm is on but worth measuring.** On a corpus with no populated
  entity graph it returns nothing while adding up to +28 s (measured). Setting
  `KB_ENTITY_SEARCH_ENABLED=false` there is a large, quality-neutral latency win.

### Embedding drift

Every chunk records the model that produced its vector. When `KB_EMBEDDING_MODEL`
changes, `embedding-drift.ts` finds the stale rows. The vector index is
**dimension-bound at DEFINE time**, so a dimension change means re-embedding the corpus
*and* re-defining the index — the service fails fast at startup rather than letting
`vector::similarity::cosine()` panic at query time.

### Optional extras

The legacy OCR path rasterises PDF pages locally and needs two packages that are
deliberately **not** installed by default — `canvas` has no prebuilt binary for current
Node ABIs and needs cairo and pixman to compile, a poor trade for a fallback most
deployments never reach. Scanned PDFs normally go to MinerU or Mistral OCR instead.

```bash
bun add pdf-img-convert sharp   # only if you want the local rasteriser
```

Without them, `processDocumentOCR` on a scanned PDF returns an error naming the missing
package rather than failing obscurely.

## Scaling

One process serves the API and runs the ingest worker. Jobs are claimed with
`FOR UPDATE SKIP LOCKED`, so **running more replicas is the whole scaling story** — no
queue broker, no separate worker deployment. Set `KB_WORKER_ENABLED=false` on a replica
to make it API-only.

A worker that dies mid-job does not strand it: jobs stale after 5 minutes are reclaimed
by a sweep, retried up to 3 times, and then marked terminally failed.

## Architecture

The engine talks to the outside world through eight interfaces — blob storage, vectors,
documents, jobs, progress, config, endpoints, job execution — defined in
[`src/lib/kb-runtime/ports.ts`](src/lib/kb-runtime/ports.ts).
[`src/service/adapters.ts`](src/service/adapters.ts) binds them to this service's
infrastructure.

**Nothing under `src/lib` may import anything under `src/service`**, and
`bun run check:kb-boundary` enforces it in CI. That constraint is not tidiness — it is
what lets the same engine run embedded inside a host application *and* standalone here.

## Development

```bash
bun install
bunx prisma generate
docker compose up -d postgres surrealdb minio createbucket
bunx prisma migrate deploy
bun run kb:apply-schema        # SurrealDB tables + vector index + FTS index
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

Tagging `v*` builds a multi-arch image (amd64 + arm64 — the on-prem boxes are aarch64),
publishes it to `ghcr.io/rantai-dev/rantai-kb`, and cuts a GitHub release. `/health`
reports the build version.

## License

Apache-2.0.
