# rantai-kb (Rust Port)

High-performance Rust rewrite of the RantAI Knowledge Base (`rantai-kb`) service for document ingest, chunking, indexing, and multi-arm hybrid retrieval.

## Overview

The Rust workspace under `rust/` implements the full standalone service, completely replacing the legacy TypeScript/Bun runtime while maintaining 100% data and schema compatibility against PostgreSQL 16, SurrealDB v2, and S3-compatible object storage.

### Workspace Crates

```text
kb-core  ←  kb-ports  ←  kb-inference · kb-store-pg · kb-store-surreal · kb-store-blob
                      ←  kb-extract
                      ←  kb-engine  ←  kb-server (composition root)
```

- **`kb-core`**: Pure domain logic: chunking (smart, table-aware, code-block-aware), Reciprocal Rank Fusion (RRF), neighbour window expansion, source numbering, figure policy, text-layer signals, page maps, ingest policy, and progress tracking. Zero I/O, no network.
- **`kb-ports`**: Shared domain interfaces and types (`DocumentStore`, `ChunkStore`, `BlobStore`, `JobStore`, `EntityStore`, `Extractor`, `Embedder`, `Reranker`, `FigureGate`, `DocumentScope`).
- **`kb-inference`**: HTTP clients for embeddings (OpenAI-compatible), rerankers (Cohere, Jina, Voyage, TEI, LLM-as-reranker), query expansion, and LLM entity/relation extraction.
- **`kb-store-pg`**: PostgreSQL adapter via `sqlx`: documents, knowledge bases, categories, API keys (`rkb_` generation and sha256 verification), ingest job queue (`FOR UPDATE SKIP LOCKED`), and Prisma-compatible migration runner.
- **`kb-store-surreal`**: SurrealDB adapter via HTTP `/sql`: vector search (HNSW cosine), full-text search (BM25), figure co-retrieval, neighbour lookups, entity graph (`UPSERT entity` / `RELATE`), and DDL vector schema manager.
- **`kb-store-blob`**: S3-compatible object storage client (AWS SigV4 with path-style and virtual-host addressing): document raw files, figures, and asset uploads.
- **`kb-extract`**: Extraction engine: pure-Rust parsers (DOCX, PPTX, XLSX, ODT, ODS, EPUB, RTF, GLTF/GLB, HTML, XML, Markdown, Code, Plaintext), dynamic PDFium text-layer extraction, smart router, layout extractor chain (MinerU sidecar, MinerU API, Mistral OCR), and OpenRouter vision OCR fallback.
- **`kb-engine`**: Pipeline orchestrator for ingestion and retrieval. Knows only ports, zero adapter names.
- **`kb-server`**: Axum HTTP service (REST API, SSE progress stream), worker process loop, CLI subcommands, and configuration loader.

---

## Building and Running

### Prerequisites

- Rust 1.85+ (Edition 2024)
- PostgreSQL 16
- SurrealDB v2 (2.x)
- S3-compatible store (MinIO, RustFS, AWS S3)
- `libpdfium.so` (run `./rust/scripts/fetch-pdfium.sh ./rust/.vendor/pdfium`)

### Build

```bash
# Debug build
cargo build --workspace

# Release binary
cargo build --release --bin kb-server
```

### Run Server

```bash
# Serves HTTP on $PORT (default: 8080) and runs the ingest worker
cargo run --bin kb-server
# or explicitly:
cargo run --bin kb-server -- serve
```

---

## CLI Subcommands

`kb-server` provides subcommands for all operational tasks:

```bash
# 1. Apply database migrations (Prisma-compatible, tracked in _prisma_migrations)
kb-server migrate

# 2. Apply SurrealDB DDL schema at configured embedding dimension
kb-server apply-schema

# 3. Mint API key (matches TypeScript scripts/create-api-key.ts)
kb-server create-api-key <tenantId> [name] [--scopes kb:read,kb:write] [--kb <id,id>]

# 4. Verify crate layering boundary (fails if domain/engine imports adapter)
kb-server check-boundary

# 5. Start HTTP server and ingest worker (default)
kb-server serve
```

---

## Environment Variables Reference

| Variable | Description | Default |
|---|---|---|
| **Database & Storage** | | |
| `DATABASE_URL` | PostgreSQL connection URL (e.g. `postgresql://user:pass@host:5432/db`) | *Required* |
| `SURREAL_DB_URL` | SurrealDB endpoint URL (HTTP for Rust, e.g. `http://localhost:8000`) | *Required* |
| `SURREAL_DB_USER` | SurrealDB username | `None` |
| `SURREAL_DB_PASS` | SurrealDB password | `None` |
| `SURREAL_DB_NAMESPACE` | SurrealDB namespace | `rantai` |
| `SURREAL_DB_DATABASE` | SurrealDB database name | `knowledge` |
| `S3_ENDPOINT` | S3-compatible object storage endpoint | `http://localhost:9000` |
| `S3_REGION` | S3 region | `us-east-1` |
| `S3_BUCKET` | S3 bucket name | `rantai-kb` |
| `S3_ACCESS_KEY_ID` | S3 access key ID | `""` (anonymous if empty) |
| `S3_SECRET_ACCESS_KEY` | S3 secret access key | `""` (anonymous if empty) |
| `S3_ENABLE_PATH_STYLE` | `1` to enable S3 path-style addressing (`0` for virtual-host) | `1` (path-style) |
| **Server & General** | | |
| `PORT` | HTTP server port | `8080` |
| `KB_VERSION` | Version string surfaced by `/health` endpoint | `dev` / Cargo version |
| `KB_SKIP_MIGRATE` | Set to `1` to skip automatic migrations and schema on container boot | `0` |
| `KB_MIGRATIONS_DIR` | Directory containing Prisma migration directories | `prisma/migrations` |
| `RUST_LOG` | Logging filter directive (tracing subscriber) | `info` |
| **Ingest Worker** | | |
| `KB_WORKER_ENABLED` | Set to `false` to disable worker in API process | `true` |
| `KB_INGEST_CONCURRENCY` | Jobs claimed per poll batch | `1` (min: 1) |
| `KB_INGEST_POLL_MS` | Job polling interval in milliseconds | `3000` (min: 500) |
| `KB_INGEST_STALE_MS` | Inactivity duration before a job is considered stale | `300000` (min: 60000) |
| `KB_INGEST_MAX_ATTEMPTS` | Maximum retry attempts for a failed job | `3` (min: 1) |
| `KB_INGEST_RECLAIM_MS` | Interval for stale job reclaimer loop | `60000` (min: 15000) |
| `KB_INGEST_REAP_DAYS` | Retention duration in days for failed job artifacts | `7` |
| `KB_INGEST_REAP_MS` | Interval for failed job artifact cleanup | `3600000` (min: 1000) |
| **Embeddings & Search** | | |
| `KB_EMBEDDING_BASE_URL` | OpenAI-compatible embedding API endpoint | `https://openrouter.ai/api/v1/embeddings` |
| `KB_EMBEDDING_MODEL` | Embedding model identifier | `qwen/qwen3-embedding-8b` |
| `KB_EMBEDDING_API_KEY` | API key for embeddings | `OPENROUTER_API_KEY` |
| `KB_EMBEDDING_DIM` | Embedding vector dimensions | `4096` |
| `KB_EMBEDDING_QUERY_PREFIX` | Prefix prepended to query text before embedding | `""` |
| `KB_EMBEDDING_PASSAGE_PREFIX`| Prefix prepended to passage text before embedding | `""` |
| `KB_HYBRID_BM25_ENABLED` | Set to `false` to disable BM25 keyword search arm | `true` |
| `KB_QUERY_EXPANSION_MODEL` | Model for query expansion | `None` (disabled) |
| `KB_QUERY_EXPANSION_API_KEY` | API key for query expansion | `OPENROUTER_API_KEY` |
| `KB_QUERY_EXPANSION_BASE_URL`| Base URL for query expansion completions | OpenRouter URL |
| `KB_RERANK_PROVIDER` | Reranker provider (`cohere`, `tei`, `jina`, `voyage`, `llm`) | `llm` |
| `KB_RERANK_MODEL` | Model identifier for reranker | Provider default |
| `KB_RERANK_BASE_URL` | Base endpoint for TEI or self-hosted reranker | `None` |
| `KB_RERANK_API_KEY` | API key for reranker | Provider default / `OPENROUTER_API_KEY` |
| **Extraction, OCR & PDFium** | | |
| `PDFIUM_DYNAMIC_LIB_PATH` | Explicit absolute path to `libpdfium.so` | Auto-detected from `.vendor` or system |
| `KB_EXTRACT_PRIMARY` | Primary PDF extraction engine (`smart`, `pdfium`, `sidecar`, `mineru-api`, `mistral`) | `smart` |
| `KB_EXTRACT_FALLBACK` | Fallback extractor if primary fails | `vision` |
| `KB_EXTRACT_SMART_FALLBACK` | Fallback when smart router detects low text quality/scanned PDF | `sidecar` |
| `KB_EXTRACT_MINERU_BASE_URL` | Endpoint for MinerU sidecar extraction service | `None` |
| `KB_EXTRACT_MINERU_TIMEOUT_MS`| MinerU sidecar request timeout in ms | `300000` |
| `KB_MINERU_API_KEY` | API token for hosted MinerU API | `None` |
| `KB_MINERU_API_BASE` | Base URL for hosted MinerU API | `https://mineru.net/api/v4` |
| `KB_MINERU_API_LANG` | Language hint for MinerU OCR | `en` |
| `KB_MINERU_API_TIMEOUT_MS` | Hosted MinerU API timeout in ms | `300000` |
| `KB_MISTRAL_OCR_KEY` | API key for Mistral OCR | `None` |
| `KB_MISTRAL_OCR_BASE` | Base URL for Mistral OCR | `https://api.mistral.ai/v1` |
| `KB_MISTRAL_OCR_MODEL` | Model identifier for Mistral OCR | `mistral-ocr-latest` |
| `KB_LAYOUT_EXTRACTOR_ORDER` | Ordered comma-separated list of layout extractors | `sidecar,mineru-api,mistral` |
| `KB_EXTRACT_VISION_API_KEY` | API key for vision OCR | `OPENROUTER_API_KEY` |
| `KB_EXTRACT_VISION_BASE_URL`| Base URL for vision OCR | OpenRouter URL |
| `OCR_FALLBACK_MODEL` | Vision model for scanned PDF and image OCR | `google/gemini-2.5-flash` |
| **Intelligence & Vision Gate**| | |
| `KB_ENTITY_EXTRACTION_ENABLED`| Set to `false` to disable entity and relation extraction | `true` |
| `ENTITY_EXTRACTION_LLM_MODEL` | Model for LLM entity extraction | `google/gemini-2.5-flash` |
| `ENTITY_EXTRACTION_LLM_BASE_URL`| Base URL for entity extraction | OpenRouter URL |
| `ENTITY_EXTRACTION_LLM_API_KEY` | API key for entity extraction | `OPENROUTER_API_KEY` |
| `ENTITY_EXTRACTION_CONCURRENCY` | Concurrent chunk entity extraction requests | `3` |
| `ENTITY_EXTRACTION_TIMEOUT_MS` | Request timeout for entity extraction in ms | `30000` |
| `KB_FIGURE_VLM_ENABLED` | Enable VLM visual relevance filter on figure co-retrieval | `false` |
| `KB_FIGURE_VLM_BASE` | Base URL for figure gate vision endpoint | `None` |
| `KB_FIGURE_VLM_MODEL` | Model name for figure gate vision judge | `None` |
| `KB_FIGURE_VLM_TOPN` | Top candidate figures evaluated by figure gate | `5` |
| `KB_FIGURE_VLM_MAX` | Maximum number of candidate figures accepted | `2` |
| `KB_FIGURE_VLM_TIMEOUT_MS` | Figure gate evaluation timeout in ms | `15000` |

---

## Docker

Multi-stage build with `cargo-chef` dependency caching:

```bash
# Build local amd64 image
docker build -t rantai-kb:rust -f rust/Dockerfile .

# Run container
docker run -p 8080:8080 --env-file .env rantai-kb:rust
```

The runtime image is built on `debian:bookworm-slim`, runs as non-root user `appuser` (uid 10001), includes liberation fonts and CA certificates, and bundles `libpdfium.so` dynamically loaded per `TARGETARCH`.

---

## Testing & Parity Verification

### 1. Workspace Unit & Integration Tests
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

### 2. Live Smoke Test
Runs Postgres 16, SurrealDB v2, S3 (RustFS), and stub embeddings to execute an end-to-end ingest, chunking, SSE events, search, detail, list, and soft/hard delete cycle:
```bash
./rust/scripts/smoke.sh
```

### 3. Bidirectional Search Parity Matrix
Validates 100% search response parity between TypeScript (Bun) and Rust running against the same database and storage:
```bash
./rust/scripts/search-parity.sh
```
Runs a matrix of search bodies (queries, hybrid true/false/omitted, maxChunks 1/3/8/50, format "context", documentIds, knowledgeBaseIds, and categories), diffing normalized JSON responses (`jq -S`, rounded to 4 decimal places) and requiring 0 diffs.

