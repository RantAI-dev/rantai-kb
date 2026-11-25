# Stateless Processor Mode — Integration & Operations Guidelines

Use **Stateless processor mode** (`kb-server serve --stateless` or `KB_MODE=stateless`) when your application **already owns its own database and Qdrant server** and wants `rantai-kb` purely as a disposable, horizontally scalable document-processing and inference microservice.

In this mode `rantai-kb` makes **zero Postgres, Qdrant, or S3 connections**. It extracts, chunks, parses tables, anchors figures, computes BM25 sparse vectors, and optionally embeds, reranks, and extracts entities — returning everything synchronously as JSON.

If you want `rantai-kb` to manage storage, async jobs, and search end-to-end, see [`docs/modes/full/guidelines.md`](../full/guidelines.md).

---

## 1. Architecture & Division of Responsibility

| Concern | `rantai-kb --stateless` | Your application |
|---|---|---|
| Extraction, OCR, chunking, tables, figures | **Owns** (`POST /v1/process`) | Sends uploaded file, persists returned JSON |
| Sparse BM25 & dense vector encoding | **Owns** (`POST /v1/process`, `POST /v1/encode`) | Upserts vectors to Qdrant; calls `/v1/encode` at search time |
| Cross-encoder / cloud reranking | **Optional** (`POST /v1/rerank`) | Calls `/v1/rerank` after Qdrant RRF prefetch (or calls reranker directly) |
| Document metadata, dedup, tenant RBAC | None | **Owns** (your DB: Postgres, MySQL, SQLite, etc.) |
| Vector storage & hybrid search execution | None | **Owns** (your external Qdrant server) |
| Raw file / figure image storage | None | **Owns** (your S3 / R2 / MinIO / disk, if needed) |
| Async job queue & retries for large files | None (synchronous HTTP) | **Owns** (BullMQ, Celery, Sidekiq, Temporal, Postgres `SKIP LOCKED`, etc.) |

---

## 2. Booting & Configuration

### Authentication (required by default)
Stateless mode has no Postgres `ApiKey` table and **refuses to boot** unless authentication is explicitly configured:
- **Standard:** Set `KB_STATELESS_API_KEY=secret1,secret2` (comma-separated for zero-downtime key rotation; compared in constant time via SHA-256). Callers pass `Authorization: Bearer <key>` or `X-API-Key: <key>`.
- **Trusted private network / sidecar only:** Set `KB_STATELESS_ALLOW_UNAUTHENTICATED=true` only when network policy guarantees no untrusted traffic can reach the port.

### Common deployment profiles

```bash
# 1. Pure local processor (zero external APIs, zero GPU — extraction + tables + BM25 sparse)
KB_MODE=stateless \
KB_STATELESS_API_KEY=change-me \
  rantai-kb serve

# 2. Cloud APIs (Voyage multimodal embeddings + Voyage/Cohere rerank + Mistral OCR)
KB_MODE=stateless \
KB_TIER=cloud \
KB_STATELESS_API_KEY=change-me \
OPENROUTER_API_KEY=sk-or-... \
MISTRAL_OCR_API_KEY=... \
  rantai-kb serve

# 3. Self-hosted local models (Ollama / vLLM / TEI)
KB_MODE=stateless \
KB_STATELESS_API_KEY=change-me \
KB_EMBEDDING_BASE_URL=http://vllm-embed:8000/v1/embeddings \
KB_EMBEDDING_MODEL=nvidia/Nemotron-3.5-Embed-1B-BF16 \
KB_EMBEDDING_DIM=2048 \
KB_RERANK_ENABLED=true \
KB_RERANK_PROVIDER=vllm \
KB_RERANK_BASE_URL=http://rerank:8080 \
KB_RERANK_MODEL=Qwen/Qwen3-Reranker-0.6B \
  rantai-kb serve
```

### Selective opt-outs
- `KB_STATELESS_EMBED=false` — disable dense embeddings even when `OPENROUTER_API_KEY` or a tier preset is set (e.g., OCR-only or sparse-only deployments).
- `KB_STATELESS_RERANK=false` — disable `POST /v1/rerank` even when a tier preset enables reranking.
- `KB_CACHE_DIR=/var/cache/rantai-kb` — optional local disk cache for extraction, OCR pages, embeddings, and rerank scores across retries or re-processing.

---

## 3. What Your App Needs in Its Own DB & Qdrant

### Minimal app database table
You do **not** need a `chunks` table if you store `content` and metadata inside Qdrant's point payload. One `documents` table is enough for most apps:

```sql
CREATE TABLE documents (
    id              UUID PRIMARY KEY,
    tenant_id       TEXT NOT NULL,
    kb_id           TEXT,
    title           TEXT NOT NULL,
    filename        TEXT NOT NULL,
    file_type       TEXT NOT NULL,          -- ProcessOutput.fileType
    file_size       BIGINT NOT NULL,        -- ProcessOutput.fileSize
    sha256          TEXT NOT NULL,          -- ProcessOutput.sha256 (for dedup)
    status          TEXT NOT NULL,          -- pending | processing | ready | failed
    chunk_count     INT NOT NULL DEFAULT 0,
    embedding_model TEXT,                   -- ProcessOutput.embeddingModel
    quality         JSONB,                  -- ProcessOutput.quality (optional)
    page_map        JSONB,                  -- ProcessOutput.pageMap (optional)
    outline         JSONB,                  -- ProcessOutput.outline (optional)
    degrades        JSONB,                  -- ProcessOutput.degrades (optional)
    storage_key     TEXT,                   -- S3/R2 path if you retain the original file
    error_message   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, kb_id, sha256)
);
```

Optional tables only if your UI/product needs them:
- **Tables (no Parquet/DuckDB in stateless mode):** `POST /v1/process` extracts every detected table into `ProcessOutput.tables[]` (typed `header`, typed `rows`, and inferred `manifest`) plus `table_summary` and `table_row` chunks in `ProcessOutput.chunks[]`, without writing Parquet files or running DuckDB. Choose the storage/query pattern that fits your app:
  1. **Qdrant only (simplest):** Index the returned `table_summary` / `table_row` chunks alongside prose chunks for semantic + BM25 search over table text.
  2. **App DB (`JSONB`):** Persist `ProcessOutput.tables[]` (`header`, `rows`, `manifest`) in Postgres/MySQL/SQLite for UI grid rendering and lightweight filtering.
  3. **Parquet + DuckDB on your side:** Write `ProcessOutput.tables[]` to Parquet in your own S3/R2 bucket and query them with embedded DuckDB (or Polars/Arrow) if you need exact SQL aggregations over large spreadsheets. *(If you want `rantai-kb` to manage Parquet + embedded DuckDB + `POST /v1/tables/:id/query` automatically, use [Full service mode](../full/guidelines.md).)*
- **Figures:** Pass `includeFigureBytes=true`, decode each `figures[].pngBase64` to S3/R2, and store `{document_id, figure_index, page, kind, caption, bbox, anchor_chunk_index, image_key}`.
- **Entities & relations:** Pass `entities=true` (uses free local heuristic extractor by default; set `KB_ENTITY_EXTRACTOR=llm` only if you want LLM extraction) and store `entities[]` / `relations[]`.

### Required Qdrant collection configuration
`rantai-kb` produces BM25 term-frequency sparse vectors (sorted FNV-1a `u32` term IDs) built for **Qdrant server-side IDF**. Always configure `modifier: "idf"` on the sparse vector:

```json
PUT /collections/kb_chunks
{
  "vectors": {
    "dense": {
      "size": 1024,
      "distance": "Cosine"
    }
  },
  "sparse_vectors": {
    "sparse": {
      "modifier": "idf"
    }
  }
}
```
*(Match `dense.size` to your configured embedding model — e.g. `1024` for `voyage-multimodal-3.5` / `bge-m3`, `2048` for `Nemotron-3.5-Embed-1B`, `384` for `lite` — or omit `vectors.dense` for sparse-only search.)*

**Create Qdrant payload indexes** immediately so tenant filters and document deletes stay O(log N):
- `tenant_id` (`keyword`, `is_tenant: true` for multi-tenant workloads)
- `document_id` (`keyword`)
- `kb_id` (`keyword`, if using folders/collections)
- `chunk_type` (`keyword`: `text`, `table_summary`, `table_row`, `figure`)
- `category` (`keyword`, optional)

**Upsert guidelines:**
- Use a **deterministic UUIDv5** per `(document_id, chunk.chunkIndex)` as the Qdrant point ID so worker retries overwrite idempotently instead of creating duplicate chunks.
- Batch upserts (e.g., 100–256 points per request) and delete old points by filter `document_id == <id>` before re-indexing a revised version.

---

## 4. End-to-End Integration Flows

### Ingest flow (background worker recommended)
1. User uploads file to your app → app computes `sha256` → checks `documents` table for dedup → stores raw file (optional) → enqueues a background job (`status = 'processing'`).
2. Worker sends file to `POST /v1/process` on `rantai-kb` (with generous HTTP timeout for large scanned PDFs, e.g. 5–10 minutes).
3. Worker batches `output.chunks` into Qdrant (`dense` + `sparse` vectors + payload), saves optional `tables`/`figures`/`entities`, and marks `documents.status = 'ready'`.
4. **Error handling:**
   - `400` / `413` / `415` / `422` → permanent caller/file error; mark document `failed` with `error` message (do **not** retry blindly).
   - `503` → transient overload (`Retry-After` header) or offline model/OCR backend; retry with exponential backoff + jitter respecting `Retry-After`.

### Hybrid search + rerank flow
1. Call `POST /v1/encode` with `{"texts": [userQuery], "mode": "query"}` → returns matching `embeddings[0]` (with the exact model query prefix) and `sparseVectors[0]` (with the exact FNV-1a BM25 tokenizer).
2. Query your Qdrant collection with native hybrid `prefetch` (`dense` top-40 + `sparse` top-40) fused via `rrf`, filtered by `tenant_id` (and `kb_id` / `document_id` when scoped).
3. *(Optional, recommended for accuracy)* Send the top 15–20 fused candidate texts to `POST /v1/rerank` with `{"query": userQuery, "documents": texts, "topK": 10}` and reorder your results using the returned `results[].index` and `results[].score`.

### Parity guarantees (tested)

`API-5` pins stateless `/v1/encode` and `/v1/rerank` to the vectors and ordering that full mode produces, driving both modes against the same recording fake embedder and reranker (`rust/crates/kb-server/tests/stateless_parity_tests.rs`):

| Guarantee | Test |
|---|---|
| `/v1/encode mode=passage` dense vector is bit-for-bit the vector full-mode ingest stores for a chunk with that exact text, with no prefixes, and the provider sees the same text | `encode_passage_matches_full_mode_ingest_bit_for_bit_without_prefix` |
| The same with `KB_EMBEDDING_PASSAGE_PREFIX="passage: "`, provider text included | `encode_passage_matches_full_mode_ingest_bit_for_bit_with_prefix` |
| `/v1/encode mode=query` dense vector equals the vector full mode uses in `/v1/search`, with and without `KB_EMBEDDING_QUERY_PREFIX` | `encode_query_matches_full_mode_search_bit_for_bit_without_prefix`, `encode_query_matches_full_mode_search_bit_for_bit_with_prefix` |
| `/v1/encode` BM25 sparse vector equals the `kb_core::sparse` vector full mode indexes (`encode_document`) and queries (`encode_query`) with | `encode_sparse_matches_full_mode_bm25_indexer_and_query` |
| `/v1/rerank` returns the same order and scores as the full-mode rerank stage for the same candidates, with `KB_RERANK_BREADCRUMBS=off`, and hands the reranker the same candidate text | `rerank_order_and_scores_match_full_mode_with_breadcrumbs_off` |
| The exact breadcrumb a stateless client must prepend when `KB_RERANK_BREADCRUMBS=on` | `full_mode_breadcrumb_text_is_client_side_for_stateless_rerank` |

Passage text composition: full mode embeds `kb_core::title::embedding_text(title, heading_path, content)` — `"{title > heading path}\n\n{content}"`, truncated at `KB_EMBEDDING_MAX_CHARS` when set — while `/v1/encode mode=passage` embeds exactly the text you send. Send the composed text in `texts[]` to match a titled or headed chunk; for an untitled, headingless chunk the content alone is already identical. Prefixes are applied by the configured embedder on both sides, so keep `KB_EMBEDDING_QUERY_PREFIX`, `KB_EMBEDDING_PASSAGE_PREFIX`, `KB_EMBEDDING_INPUT_TYPE` and `KB_EMBEDDING_FORMAT` the same on the stateless service and on the full-mode service whose Qdrant index you are querying.

#### Reranker breadcrumbs: the one deliberate difference

With `KB_RERANK_BREADCRUMBS=on`, full mode prepends a breadcrumb to the text the reranker sees (the response content is unchanged). Stateless `/v1/rerank` only receives `documents[]` strings — it has no document title, section or page — so it sends the bare text. This is a caller responsibility, not a parity bug: **if the flag is on and you want full-mode ordering, prepend the same breadcrumb yourself.**

The exact format is:

```
"{document title} › {section path or section}" + (page ? " · p.{page}" : "") + "\n" + content
```

Empty title/section parts are skipped, so no dangling `›` or ` · ` is left behind. It is the same pure function full mode uses, `kb_core::rerank::breadcrumb(title, section, page)`:

```python
crumb = title.strip()
section = (section or "").strip()
if section:
    crumb = f"{crumb} › {section}" if crumb else section
if page is not None:
    crumb = f"{crumb} · p.{page}" if crumb else f"p.{page}"
# Full mode caps only the content at KB_RERANK_CHAR_BUDGET, then prepends the crumb.
document = f"{crumb}\n{content}" if crumb else content
```

With the flag off (the default) send the bare content; no prepending is needed.

---

## Progressive disclosure in stateless mode

`POST /v1/process` now returns everything needed to rebuild the full-service
document map and byte-exact reading experience in your own stack, additively:
`outlineVersion: 2` and `sections[]` (the same shape as
`GET /v1/documents/:id/structure`), per-chunk `charStart` / `charEnd` /
`overlapPrefixLen` (absent, not `0`, on chunks without spans), and `pages[]`
with a `pageSha256` per page.

### Qdrant payload recipe

Store the span fields on every point payload next to `document_id` and
`tenant_id`, and create **integer** payload indexes for `page` and
`chunkIndex` so section/page/chunk-range reads use range filters, not scans:

```json
PUT /collections/kb_chunks/index
{ "field_name": "page",       "field_schema": "integer" }

PUT /collections/kb_chunks/index
{ "field_name": "chunkIndex", "field_schema": "integer" }
```

```python
payload = {
    "tenant_id": tenant_id,
    "document_id": document_id,
    "chunkIndex": chunk["chunkIndex"],   # INTEGER index
    "page": chunk.get("page"),           # INTEGER index (null when pageless)
    "text": chunk["content"],
}
if "charStart" in chunk:
    payload["charStart"] = chunk["charStart"]        # INTEGER
    payload["charEnd"] = chunk["charEnd"]            # INTEGER
    payload["overlapPrefixLen"] = chunk["overlapPrefixLen"]  # INTEGER
# ... plus the dense/sparse vectors and any keyword-index fields above
```

### Client-side stitching (byte-exact reading)

The chunks' `content` fields already contain the overlap prefix, so skip it.
When a chunk carries no span (`overlapPrefixLen` absent), keep it whole and
join with a blank line instead of guessing an overlap. Read chunks in
`chunkIndex` order:

```python
def stitch(chunks, from_document_start=True):
    out = []
    for i, chunk in enumerate(chunks):
        text = chunk["content"]
        overlap = chunk.get("overlapPrefixLen")
        if i == 0:
            # A window that starts mid-document (a section, a page range)
            # drops the first chunk's overlap: it repeats a chunk outside
            # the window.
            if not from_document_start and overlap:
                text = "".join(list(text)[overlap:])
            out.append(text)
        elif overlap is not None and overlap > 0:
            out.append("".join(list(text)[overlap:]))  # skip repeated prefix
        else:
            out.append("\n\n" + text)              # no span: keep whole
    return "".join(out)
```

This is the same rule as `GET /v1/documents/:id/content`: a section from
`sections[]` is the inclusive chunk range `chunkStart..chunkEnd`, so
`/content`-equivalent text is
`stitch(chunks[chunkStart:chunkEnd + 1], from_document_start=(chunkStart == 0))`.

### Cheap change detection with `pageSha256`

`pages[i].pageSha256` is the lowercase hex SHA-256 of the **UTF-8 bytes of
`pageMap[i].text`** — the page's extracted text exactly as returned in the
response. Store it beside the page in your DB and, on a re-process, only
re-embed/re-index pages whose digest changed:

```python
new = {p["page"]: p["pageSha256"] for p in output["pages"]}
for page, digest in new.items():
    if stored.get(page) == digest:
        continue  # unchanged: keep existing points, embeddings and captions
    reindex_page(page, output)  # only this page's chunks need new vectors
    stored[page] = digest
```

A changed file-level `sha256` is not enough to localise the edit; the per-page
digest is. Pages are matched by their zero-based `page` number, the same
numbering used in chunk payloads and the integer `page` index.

---

## 5. Concurrency, Backpressure & Scaling

Stateless `rantai-kb` protects its VM and downstream model servers automatically at three layers:

1. **Pre-read admission cap (`KB_STATELESS_MAX_INFLIGHT`):**
   - Checked **before** buffering multipart upload bytes into RAM, preventing OOM under upload bursts.
   - Default: `max(extract_seats * 4, 4)` (e.g. `8` with default `extract=2`). Excess `/v1/process` requests immediately receive `503 Service Unavailable` with `Retry-After: 2`.
2. **Stage seats (`KB_SEATS`, default `extract=2,embed=4`):**
   - `extract` caps CPU/RAM-heavy local PDF/Office parsing and releases early on remote OCR handoff so the VM stays busy without thrashing.
   - `embed` caps concurrent bulk passage embedding batches (`POST /v1/process` and `POST /v1/encode` with `mode: "passage"`).
3. **Search fast lane:**
   - `POST /v1/encode` (`mode: "query"`) and `POST /v1/rerank` bypass bulk ingest seats so live user searches stay low-latency even during heavy document ingestion.

### Sizing & horizontal scaling
- **Single replica:** Keep `KB_SEATS=extract=<vCPU/2>,embed=4` and match `KB_STATELESS_MAX_INFLIGHT` to your RAM budget (`max_inflight × typical_file_size` comfortably below container memory limit).
- **Multiple replicas:** Stateless `rantai-kb` needs **no sticky sessions and no shared database**. Put N identical replicas behind a standard round-robin / least-connections load balancer and autoscale on CPU or `503` rate.
- **Reverse proxy timeouts:** Set upstream read timeouts ≥ 300 s for `/v1/process` (large OCR PDFs) and keep `/v1/encode` + `/v1/rerank` tight (15–30 s).

---

## 6. Operational Checklist

- [ ] `KB_STATELESS_API_KEY` set (or `KB_STATELESS_ALLOW_UNAUTHENTICATED=true` strictly inside a private VPC/mesh).
- [ ] Qdrant collection created with `sparse_vectors.sparse.modifier = "idf"` and payload indexes on `tenant_id` and `document_id`.
- [ ] Caller uses deterministic UUIDv5 point IDs (`document_id + ":" + chunkIndex`) for idempotent Qdrant upserts.
- [ ] Background worker retries `503` respecting `Retry-After` and does **not** retry `400`/`413`/`415`/`422`.
- [ ] Search path uses `POST /v1/encode` (`mode: "query"`) rather than re-implementing sparse tokenization or prefixes in client code.
- [ ] `GET /health` checked for `ok: true`, `capabilities.{embed, rerank}`, `limits`, and `backends.*.state`.
