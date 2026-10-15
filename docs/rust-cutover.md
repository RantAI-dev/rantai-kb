# Runbook: Rust Cutover & Migration (`rantai-kb`)

This runbook guides operators through rolling out the Rust implementation of `rantai-kb` into production, operating in a mixed fleet alongside the Bun service, and rolling back if necessary.

---

## 1. Prerequisites

Before starting cutover, ensure the following infrastructure and settings are verified:

1. **Databases**:
   - **PostgreSQL 16**: Accessible via `DATABASE_URL`.
   - **SurrealDB v2 (2.x)**: Accessible via `SURREAL_DB_URL` over HTTP (e.g. `http://surreal:8000`). Note: The Rust service communicates via SurrealDB's HTTP `/sql` endpoint, whereas Bun uses WebSocket `/rpc`. SurrealDB v2 serves both on port 8000.
2. **Object Storage**:
   - S3-compatible bucket (`S3_BUCKET`, default `rantai-kb`) with read/write access.
3. **Inference Providers**:
   - `KB_EMBEDDING_BASE_URL`, `KB_EMBEDDING_API_KEY`, and `KB_EMBEDDING_DIM` (default: 4096).
4. **Binary & Container Images**:
   - Docker image: `rantai-kb:rust` (or release binary `kb-server`).

---

## 2. Migration Ownership

The PostgreSQL schema and SurrealDB vector definitions are shared between Bun and Rust.

### PostgreSQL Migrations

- The Rust migration runner (`kb-server migrate` or `kb_store_pg::migrate`) executes `prisma/migrations/*/migration.sql` in directory order.
- Each migration is recorded in Prisma's native `_prisma_migrations` table using the identical column schema and SHA-256 hex checksum over the migration file.
- **Locking**: A PostgreSQL session advisory lock (`7241331885`) prevents race conditions between multiple booting containers.
- **Idempotency**: Already-applied migrations are safely skipped.
- **Ownership**: Both `bunx prisma migrate deploy` and `kb-server migrate` agree on what is applied. Either binary can manage migrations.

### SurrealDB DDL

- The vector schema is applied via `kb-server apply-schema` (or `scripts/apply-vector-schema.ts`).
- All statements use `IF NOT EXISTS` and are idempotent.
- **Important**: Vector index dimension is set at creation time by `KB_EMBEDDING_DIM`. Do not change embedding dimensions without re-indexing chunks.

---

## 3. Mixed-Fleet Rolling Deployment

Rust and Bun services are fully bidirectional-compatible and can run concurrently against the same databases and object storage during a phased rollout.

### Phase 1: Deploy Rust Ingest Workers

Deploy Rust instances configured as worker-only or dual API/worker:

```bash
# Example Rust Worker instance
export KB_WORKER_ENABLED="true"
kb-server serve
```

Meanwhile, Bun instances can have worker disabled if desired (`KB_WORKER_ENABLED=false`).
Jobs enqueued by Bun API instances are claimed via `FOR UPDATE SKIP LOCKED` by the Rust worker, extracted with Rust parsers and PDFium, embedded, and stored in SurrealDB.

### Phase 2: Route Read & Search Traffic to Rust

Route HTTP traffic (`GET /v1/*`, `POST /v1/search`) through your load balancer to Rust replicas (`PORT=8080`).

- Validated: SurrealDB chunks created by either Bun or Rust are retrievable by both runtimes.
- Validated: API keys created by either Bun (`bun scripts/create-api-key.ts`) or Rust (`kb-server create-api-key`) authenticate seamlessly on both runtimes.
- **Search Parity Verification**: Run `./rust/scripts/search-parity.sh` to execute the full search parity test matrix (29 test cases covering queries, `hybrid: true/false/omitted`, `maxChunks` 1/3/8/50, `format: "context"`, `documentIds`, `knowledgeBaseIds`, and `category` filters) verifying zero JSON diffs between Bun and Rust on the same storage state.

### Phase 3: Route Write Traffic to Rust

Route `POST /v1/documents`, `PATCH`, and `DELETE` requests to Rust replicas.
Verify SSE progress frames on `GET /v1/events`.

### Phase 4: Retire Bun

Once 100% of traffic is handled by Rust replicas and no errors are observed in metrics, Bun containers may be stopped and removed.

---

## 4. Rollback Plan

If an unforeseen issue occurs with the Rust runtime:

1. **Zero Data Transformation Needed**: Because Rust writes standard PostgreSQL rows, identical SurrealDB `document_chunk` structures, and standard S3 object keys (`documents/{tenantId}/{documentId}/raw`), the data stored by Rust is 100% readable by Bun.
2. **Immediate Traffic Switch**: Update the load balancer or DNS to route incoming requests back to the Bun container instances (`bun src/server.ts`).
3. **Worker Switch**: Ensure `KB_WORKER_ENABLED=true` on Bun instances so the Bun ingest loop claims any pending jobs.
4. **No Schema Rollback**: No database downgrades or schema rollbacks are required.

---

## 5. What Changed: Architectural & Behavioral Differences

### 1. Dropped Legacy Ollama OCR Pipeline
The legacy Ollama local OCR provider (`src/lib/ocr/ollama-provider.ts`) was dropped in favor of the OpenRouter / OpenAI vision API provider (`google/gemini-2.5-flash` or `OCR_FALLBACK_MODEL`).

### 2. Legacy Binary Office Formats Unsupported
In TypeScript, `officeparser` v6 only supports modern ZIP-based XML formats (`.docx`, `.pptx`, `.xlsx`, `.odt`, `.ods`) and throws `EXTENSION_UNSUPPORTED` on legacy binary `.doc`, `.xls`, and `.ppt` files. The Rust port matches this exact behavior by returning `ExtractError::Unsupported`.

### 3. PDFium vs pdf.js Smart Router Thresholds
- Bun uses `unpdf` (Mozilla `pdf.js`), which renders text streams through JS code.
- Rust uses dynamically loaded `libpdfium`, which extracts glyphs directly from native PDF operators.
- While PDFium provides superior speed and lower memory usage, subtle differences exist in whitespace collapse and hyphenation.
- **Action Required**: The smart router thresholds (`min_chars_per_page = 300`, `max_columnar_lines = 5`, `max_currency_matches = 10`) should be re-validated against the 40-document benchmark suite under staging conditions.

### 4. Intentional API Response Shape Refinements
- **Envelopes and Casing**:
  - Bun wraps responses in camelCase envelopes (e.g. `{ documents: [...] }`, `{ knowledgeBases: [...] }`).
  - Rust supports these shapes for compatibility while adopting standard snake_case field naming in domain DTOs.
  - Query parameters accept both camelCase (e.g. `knowledgeBaseIds`) and snake_case (`knowledge_base_ids`).
- **Strict Scope Enforcement**:
  - Every authenticated route in Rust requires `kb:read` (and `kb:write` for mutating endpoints), answering 403 to under-scoped keys.
- **Explicit-Empty Knowledge Base Filter**:
  - In TypeScript, `POST /v1/search` with `knowledgeBaseIds: []` falls through to all knowledge bases. In Rust, an explicit empty list correctly returns a message that the key is not bound to any of the requested knowledge bases, preventing data leakage.
- **Degrades Field**:
  - Rust adds a top-level `degrades` list to `POST /v1/search` results when degradable backends (e.g. reranker or query expander) fail, allowing clients to detect partial degradations without failing the entire search.
- **Lenient Query Parameter Parsing**:
  - Query parameters like `limit=abc` fall back gracefully to default `100` rather than causing an unhandled 500 error (`take: NaN` in Prisma).
- **Additive Delete Response**:
  - `DELETE /v1/documents/:id` in Rust returns additive fields (`chunks_removed` and `blob_key_pending`) in addition to `{ "ok": true }`.
