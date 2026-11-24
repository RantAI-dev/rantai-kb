# Full Service Mode — Integration & Operations Guidelines

Use **Full service mode** (default: `kb-server serve` / `KB_MODE=full`) when you want `rantai-kb` to own the complete document lifecycle — raw file storage, async ingest queue, extraction/OCR, chunking, typed tables, Qdrant indexing, hybrid retrieval, and multi-tenant isolation.

If your application already owns Postgres and Qdrant and only wants a stateless processing microservice, use [`docs/modes/stateless/guidelines.md`](../stateless/guidelines.md) instead.

---

## 1. Architecture & Responsibilities

| Component | Owned by | Purpose |
|---|---|---|
| **Postgres** (`DATABASE_URL`) | `rantai-kb` | Tenants, scoped API keys, documents, ingest jobs, knowledge bases, categories, typed table records & manifests, optional knowledge graph |
| **Qdrant** (`QDRANT_GRPC_URL`) | `rantai-kb` | Dense + BM25 sparse (`modifier: idf`) chunk vectors and optional page-image vectors |
| **Object store** (`S3_*` / local blob) | `rantai-kb` | Raw uploaded originals, extracted figure PNGs, page JPEGs, and versioned structured table Parquet files |
| **Embedded DuckDB** (`kb-store-tables`) | `rantai-kb` (in-process) | Writes extracted tables to Parquet and executes typed `POST /v1/tables/:id/query` aggregations locally — **no separate DuckDB server or container required** |
| **Model backends** | Tier compose or external | Embedder, optional reranker, optional OCR, optional vision captioner |
| **Your application** | Caller | End-user auth/UX, mapping app users/workspaces to `tenant` + `knowledgeBaseIds`, calling `/v1/*` |

---

## 2. Deployment & Tier Selection

1. **Start with `rantai-kb setup <tier>`** to validate VRAM, licences, and env generation before booting:
   - `lite` — CPU-only multilingual (`multilingual-e5-small` + ONNX CPU OCR + Qwen3-VL-2B CPU).
   - `lite-en` — CPU-only English (~4× faster than `lite`).
   - `desk` / `studio` / `station` — NVIDIA GPU tiers (`Nemotron-3.5-Embed-1B` unified text+page space, `Qwen3-Reranker-0.6B`, `Xiaomi-OCR-0`).
   - `cloud` — CPU box + hosted APIs (`voyage-multimodal-3.5`, `voyageai/rerank-2.5`, Mistral OCR, `nemotron-3.5-super-vl`).
2. **Share GPUs safely:** `setup` budgets VRAM assuming your chat LLM shares the card. If a larger chat model needs more VRAM, move one role off-box (`--ocr mistral`, `--ocr <url>`, `--vision off|<url>`, or external `KB_EMBEDDING_BASE_URL` / `KB_RERANK_BASE_URL`).
3. **Always mount a persistent `KB_CACHE_DIR`** in production — re-indexing after changing chunking or Qdrant settings reuses cached OCR pages and embeddings without re-paying model/API costs, and `kb-store-tables` caches downloaded table Parquet files there (falling back to `<temp>/rantai-kb-tables` if unset) so repeated DuckDB queries avoid re-downloading from object storage.
4. **Separate external DB/Qdrant in production:** Point `DATABASE_URL`, `QDRANT_GRPC_URL`, and `S3_*` at managed or dedicated hosts; run `kb-server serve --foreground` behind your load balancer.

---

## 3. Multi-Tenancy & API Key Hygiene

- **One API key = one tenant.** There is no cross-tenant route; every DB query, Qdrant filter, and table query is scoped to the key's tenant.
- **Least-privilege keys:** Mint separate keys per service or knowledge base instead of reusing one admin key everywhere:
  ```bash
  # Read-only search key restricted to specific knowledge bases
  kb-server create-api-key --tenant acme --name web-search \
    --scopes kb:read --kb kb_docs,kb_policies

  # Ingest worker key
  kb-server create-api-key --tenant acme --name ingest-worker \
    --scopes kb:read,kb:write
  ```
- **Never expose raw KB keys to browsers.** Proxy uploads and searches through your backend so your app enforces per-user RBAC and maps users to `knowledgeBaseIds`.
- See [`docs/security.md`](../../security.md) for the full threat model and scope rules.

---

## 4. Document Ingestion Best Practices

### Upload & job lifecycle
`POST /v1/documents` is **asynchronous** (`202 Accepted` + `jobId`). Never assume a document is searchable immediately after upload.

1. **Deduplication:** `KB_UPLOAD_DEDUPE` is on by default — uploading identical bytes into the same tenant + knowledge base set returns `409 Conflict` with `existingDocumentId`. Treat `409` as idempotent success in your app if the existing document is already `ready`.
2. **Track status via SSE or polling:**
   - Stream progress in real time with `GET /v1/events`, or poll `GET /v1/jobs/:id` with exponential backoff.
   - **Handle `waiting_for_ocr` as in-progress, not failed:** When a scanned PDF arrives while the OCR backend is restarting or temporarily down, `rantai-kb` parks the job in `waiting_for_ocr`, holds no worker seat, and automatically resumes when OCR recovers (preserving already-OCRed pages).
   - **Handle `needs_ocr` as failed** (only when `KB_EXTRACT_QUALITY_GATE=true` flags unreadable text layers with no working OCR).
   - Retry failed jobs via `POST /v1/jobs/:id/retry` (never re-upload the same bytes blindly).
3. **Pass `externalRef` and `knowledgeBaseIds`:** Store your app's own record ID in `externalRef` so you can reconcile documents with `GET /v1/documents?externalRef=...` and paginate large collections with `?cursor=...&limit=200`.
4. **Choose `figures` and `tablePrivacy` deliberately:**
   - `figures=auto` (default) extracts charts/diagrams on PDFs where they add signal; use `figures=skip` for pure text corpora to save latency/VLM cost.
   - `tablePrivacy=aggregate-only` enforces group-level aggregation and small-group suppression (`< 5` rows) on sensitive spreadsheets.

---

## 5. Retrieval & Grounded Answers

1. **Always inspect `degrades` on `POST /v1/search`:** `rantai-kb` degrades gracefully rather than failing the whole search if a secondary arm (e.g., reranker or page arm) times out, and lists what degraded in `degrades[]`. Log non-empty `degrades` in your app telemetry.
2. **Scope every query:** Pass `knowledgeBaseIds` or `documentIds` whenever the user is inside a project/workspace rather than searching the entire tenant.
3. **Use typed table queries (Parquet + embedded DuckDB) for numbers:**
   - During ingest, extracted tables are typed against their inferred manifest (`DOUBLE`, `BOOLEAN`, `VARCHAR`), written as versioned **Parquet** files to your object/blob store via in-process DuckDB, and indexed as `KbTable` records in Postgres.
   - When search results include `tables: [{tableId, title, documentId}]` (or `table_summary` / `table_row` chunks), call `POST /v1/tables/:id/query` for exact sums, filters, and group-bys instead of asking an LLM to sum prose snippets.
   - `kb-server` validates the typed query against the Postgres manifest, downloads the Parquet file once into `KB_CACHE_DIR` (or `<temp>/rantai-kb-tables`), executes it in **embedded in-process DuckDB** with a wall-clock timeout, and enforces `tablePrivacy=aggregate-only` suppression where configured. No separate DuckDB container is needed.
4. **Inspect `GET /health` before bulk uploads:** Check `backends.ocr.state`, `backends.embedder.state`, and `backends.reranker.state` (`ready` / `starting` / `unavailable`) before kicking off large batch backfills.
5. **Tune retrieval systematically:** See [`docs/retrieval.md`](../../retrieval.md) for `KB_RRF_K`, `KB_RERANK_MAX_K` (default `10`), `KB_MAX_PER_DOCUMENT` (default `0` = off; per-document answer diversity, RET-17), and score floors before tweaking prompts.

---

## 6. Concurrency, Seats & Scaling

- **`KB_SEATS` (`extract=2,embed=4,store=2` default):** Controls per-stage concurrency inside each `kb-server` process so heavy PDFs do not starve the VM or overload local embedders/Qdrant.
  - Local `extract` seats are released as soon as a PDF hands off to remote OCR, allowing the next file to parse locally while the first awaits OCR I/O.
  - Size `extract` to physical CPU cores / RAM (typically `2` on 4 GB RAM, `4` on 16 GB+ RAM) and `embed` to match your embedding server's batch concurrency.
- **Upload size limit:** Default `KB_MAX_FILE_BYTES` is 256 MiB (`268435456`). Make sure any reverse proxy (Nginx, Caddy, Cloudflare, ALB) allows at least the same body limit and appropriate timeouts for large uploads.
- **Re-indexing:** When switching embedding models or dimensions, run `kb-server reindex` — Qdrant collections are keyed by model + dimension so the new collection builds cleanly alongside the old one before swapping the active alias.

---

## 7. Operational Checklist

- [ ] Persistent volumes configured for Postgres, Qdrant, S3/blob store, and `KB_CACHE_DIR`.
- [ ] `GET /health` wired to container readiness/liveness probes (note: `backends.*.state` is informational — an offline optional OCR backend keeps `/health` HTTP `200`).
- [ ] Scoped keys minted per consumer (`kb:read` vs `kb:read,kb:write`); no keys exposed to frontend clients.
- [ ] Caller handles `409 Conflict` (dedupe), `waiting_for_ocr` (in-progress), and `degrades[]` on search responses.
- [ ] Automated Postgres + Qdrant snapshot backups enabled on your infrastructure.
