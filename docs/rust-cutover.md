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
- **One ledger, and Rust reads Prisma's rows**: Prisma creates `_prisma_migrations` with `timestamptz` timestamp columns, so the Rust runner asks for its applied/unapplied decision in SQL (`"finished_at" IS NOT NULL AND "rolled_back_at" IS NULL`) and decodes only the `TEXT` checksum. Decoding those timestamps in Rust cannot work against both engines — `DateTime<Utc>` fails on a timestamp without a zone and `NaiveDateTime` fails on one with — and the failure was silent, because the error was swallowed into "not applied" and every run re-applied every migration. `rust/scripts/mixed-fleet.sh` now runs Prisma's deploy **first**, on an empty database, and requires the Rust runner to then apply zero migrations, so a regression here fails the gate instead of a rolling deploy.

### Entity graph tables (`20261017120000_graph_tables`)

The entity graph moves out of SurrealDB into four plain Postgres tables — `KbEntity`, `KbMention`, `KbRelation`, `KbGraphVersion` — with the traversal done in Rust over a CSR (`docs/architecture-target.md` §11, step L.3).

- **Additive, and proven to be.** The migration creates four new tables and their indexes. It alters no existing table, column, index or constraint. `rust/scripts/mixed-fleet.sh` asserts this rather than assuming it: Prisma's own engine applies both migrations from an empty database, the four graph tables are counted inside the container, and the Rust runner must then apply none of them.
- **Bun does not read them yet.** Nothing in `src/` touches these tables, so a Bun instance and a Rust instance write to the same Postgres without seeing each other's graph rows. `docs/eval-baseline.md` §"GraphStore (L.3a)" has the measurements.
- **The scope rule is enforced in SQL, not in Rust.** Both graph reads carry `"documentId" = ANY(<scope>)`, so a relation extracted from a document the caller cannot see is never fetched and therefore cannot become a *path* through the graph. Filtering results instead of rows would still leak the existence and shape of an out-of-scope document: two visible entities joined only by an out-of-scope relation would become connected. `kb_ports::graph_contract` tests that case directly (`no_path_through_an_out_of_scope_document`) rather than inferring it.
- **Ids are derived, not generated.** An entity id is `sha256("kb-entity", tenant, key)` and a relation id `sha256("kb-relation", tenant, src, dst, type, document)`, so re-ingesting a document replaces rows instead of adding a second set, and two runs of the same ingest write byte-identical rows. A `uuid()` default would make every re-ingest a different row set.

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

### 5. Additive `retrieval` Provenance on Search Chunks

- **What changed**: every chunk in a `POST /v1/search` response may now carry one
  extra key:

  ```json
  "retrieval": { "arms": ["dense", "sparse"], "denseRank": 3, "sparseRank": 1, "fusedScore": 0.0325 }
  ```

  `arms` is the store-agnostic vocabulary: `dense` (vector), `sparse` (BM25 /
  keyword), `neighbour` (a chunk the engine wove in from an adjacent chunk of a
  hit's document), `figure` (a figure co-retrieved with a hit). `denseRank` /
  `sparseRank` are the 0-based ranks inside each arm's own candidate list, and
  `fusedScore` is the score the answer was ordered by (an RRF score with hybrid
  search, which is not comparable across queries). A phase that does not score —
  `neighbour`, `figure` — reports no score at all rather than `0`.
- **Why**: before this, a caller (and the red-team matrix in `kb-eval`) could not
  tell a keyword-only hit from a vector hit: both were "a chunk with content".
  Attribute a ranking change or a leak to an arm requires knowing which arm
  produced the hit. See step S.1c in `docs/eval-baseline.md` §7.
- **Additive, not a change**: every field Bun emits is byte-for-byte unchanged,
  including the keyword-hit shape (`chunkType`, `page` and `assetKey` omitted,
  `contextualPrefix: null`). The key is **omitted** when the engine could not
  attribute the hit, so `retrieval` absent means "this server reported no arm" and
  never means `null`.
- **`search-parity.sh`**: the parity diff now deletes `.chunks[].retrieval` from
  both answers before comparing (a no-op for Bun, which has no such key). The
  script exists to prove the two runtimes agree on every field they share; a field
  only one of them emits is not a divergence to fix.
- **`mixed-fleet.sh`** is unaffected: it asserts chunk counts and ingest
  interoperability, not response text.

### 6. Deliberate Differences: BM25 Scope Isolation & Access-Control Leak Fix
- **TypeScript Reference Bug**: In `src/lib/rag/retriever.ts` (~lines 160–195), the BM25 arm queries the entire `document_chunk` table unscoped in SurrealDB and resolves candidates through `DocumentStore.findAliveMetaByIds(docIds)`. Because `findAliveMetaByIds` only checks tenant ownership and non-deleted status, the BM25 arm completely ignores `knowledgeBaseIds` (including API key restrictions via `restrictKnowledgeBases`), `category`, and `documentIds` filters. When a query is narrowed to Knowledge Base A, BM25 can leak chunks from Knowledge Base B or other categories of the same tenant.
- **Rust Implementation (Deliberate Divergence)**: In `rust/crates/kb-engine/src/retrieval.rs`, the BM25 arm keeps a candidate hit only if its parent document is visible to the tenant AND present within the resolved `scope.document_ids()`. When a search is narrowed by knowledge base, category, or document IDs, out-of-scope BM25 hits are safely dropped.
- **Parity Preservation**: Unnarrowed searches retain 100% exact parity with TypeScript, verified across the 33-case search parity matrix (`rust/scripts/search-parity.sh`), which explicitly tests and reports the leak divergence on restricted queries while asserting exact match on all other cases.

- **Leak matrix evidence**: `kb-server eval leak` (`rust/crates/kb-eval`) probes
  every retrieval arm × scope type (tenant, knowledge base, category,
  `documentIds`) from outside, each probe paired with a positive control, and
  exits non-zero on any cross-scope hit. Baseline in `docs/eval-baseline.md`:
  0 hits over 240 probes, with the `documentIds` branch (vector-only in both
  runtimes) and the figure arm reported UNTESTED rather than passing.

### 7. Rank Fusion (RRF) and the Embedded Text

**a. Fusion: the same k=60 as Bun (the k=2 change was reverted).**
- **Bun**: `src/lib/rag/hybrid-merge.ts` fuses with the RRF paper's constant
  `k = 60`, 1-based ranks: `1/(60 + rank)`.
- **Rust**: `kb-store-qdrant` takes Qdrant's server-side fused list (its own
  k=2) as the candidate set and orders it by `1/(60 + rank)` per arm, the same
  arithmetic as Bun. The in-memory index, SurrealDB adapter and fakes compute
  the same.
- **History**: on 2026-10-22 Rust switched to Qdrant's k=2 order (`f8c0639`),
  on the strength of an offline replay with e5-large vectors. Measured live on
  `miracl-id-200` with the local embedder (Qwen3-Embedding-0.6B) it lost
  **0.021 nDCG@10 [−0.038, −0.003] and 0.031 MRR@10**
  (`docs/eval-ab/miracl-id-200/README.md`), and it was reverted (`4c01a7d`).
  An offline sweep puts every RRF k from 20 to 60 within 0.002 of each other
  (`docs/eval-tune/miracl-id-200`).
- **Parity**: the candidate set comes from Qdrant's top of the k=2 list, so a
  chunk at the edge of the pool can differ from Bun's; the order of the shared
  chunks follows the same k=60. `search-parity.sh` compares by set of chunk
  ids and counts order differences without failing on them.

**b. The embedded text carries the document's title and the chunk's heading
path.**
- **Bun**: `src/lib/rag/chunker.ts` embeds
  `"Category: …\nTopic: …\nSection: …\nContext: …\n\n<content>"`.
- **Rust (deliberate)**: `kb-engine/src/ingest.rs` embeds
  `"{title}\n\n{content}"`, where the title is the upload's title, else the
  document's own H1 (a markdown or text file that starts with `# Heading`),
  else the filename stem; and the chunk's heading path follows the title when
  the chunker found one (`"{title} > {heading path}\n\n{content}"`).
- **Why**: the retrieval quality of the corpus depends on the text that is
  embedded, and the title is what says *which document* a chunk belongs to.
  Measured live on `miracl-id-200` it is worth +0.003 nDCG@10
  [−0.001, +0.008], i.e. nothing measurable on Wikipedia passages that
  already start with their title. It is kept because it costs nothing and
  matters for chunks of long documents whose text never names the document.
- **What it changes in the API**: nothing in the response shape. It changes
  the *vectors*, so **an existing corpus has to be re-indexed** for the gain —
  the old vectors were made from the old text. The keyword arm's text is
  unchanged (the ablation showed a title in the keyword arm did not help).

### 8. Deliberate Difference: The Reranker Runs Whatever the Caller Asks For
- **Bun**: `src/lib/rag/retriever.ts` reranks only when the pool holds more
  chunks than `maxChunks`; a caller asking for at least as many chunks as the
  pool holds gets the fused order.
- **Rust (deliberate, 2026-10-23)**: when the reranker is enabled it orders
  the head of the pool (`KB_RERANK_CANDIDATE_LIMIT`, 0 = all) whenever there
  are at least two candidates, then the result is cut to `maxChunks`.
- **Why**: the reranker is the largest measured gain in the engine
  (`docs/eval-ab/README.md`: +0.037 nDCG@10 on `miracl-id-200`, +0.108 on
  `hotpot-200-test` with bge-reranker-v2-m3 on the top 20), and the number of
  chunks a caller asks for says nothing about whether their order matters.
- **What it changes in the API**: with the reranker enabled, a request for
  many chunks comes back in reranked order instead of fused order, and takes
  the reranker's latency. Shape unchanged.

### 9. Deliberate Difference: PDFs Are Chunked Page by Page
- **Bun**: chunks the whole extracted text, then tags each chunk with a page
  by searching for its first characters in the per-page text.
- **Rust (deliberate, 2026-10-23)**: when the extracted content is exactly
  its pages joined by blank lines (the PDF text-layer path), each page is
  chunked on its own (`kb_core::chunk::smart_chunk_pages`): no chunk crosses
  a page break and every chunk carries its true page. Other extractors
  (merged layout markdown, block-level page maps) keep Bun's behaviour.
- **Why**: on the 14 ViDoRe v3 HR PDFs the old tagging put 950 of 4,929
  chunks (19%) on the wrong page, nearly all one page early (running headers
  and the 200-character overlap mislead the prefix search), and 761 chunks
  straddled a page break (`cargo run -p kb-server --example page_assign_check`).
  After the change: 0 wrong, 0 straddling. Page-level retrieval measured on
  per-page units: nDCG@10 0.477 → 0.566 with the reranker
  (`docs/eval-ab/vidore3-hr-en-ours`).
- **What it changes in the API**: `page` on search chunks and sources is
  right far more often; chunk boundaries (and so chunk ids' content) differ
  from Bun's for PDFs, so **re-index PDFs** for the gain.

### 10. Deliberate Difference: Keyword Hits Carry Their Page
- **Bun**: a chunk found only by the keyword (BM25) arm is serialised without
  `page`, `chunkType` or `assetKey`; the keyword index had no page.
- **Rust (deliberate, 2026-10-23)**: the store holds the page of every chunk,
  so a keyword-only hit now carries `page` when it is known (omitted, as
  before, when it is not). `chunkType`, `assetKey` and `contextualPrefix: null`
  keep Bun's shape.
- **Why**: in a sampled ViDoRe v3 HR query, 9 of the 20 chunks returned were
  keyword-only; without a page they cannot be cited, and a client ranking
  pages silently loses them. The context text and `sources` already carried the page.
- **Parity**: `search-parity.sh` drops `page` from keyword hits before diffing.

---

## 6. Shared Known Issues

### Duplicate Chunks Across Retrieval Arms
- **Behavior**: When both the vector search arm and the BM25 search arm match the exact same chunk, the chunk can appear twice in the returned `chunks` array (once with its cosine vector similarity score and once with `similarity: 0.0` from the BM25 arm).
- **Root Cause**: In TypeScript (`src/lib/rag/retriever.ts`), vector chunks and BM25 chunks arrive as separate SurrealDB `RecordId` object instances. JavaScript `Map.has()` and `scores.get()` in the reciprocal rank fusion (RRF) logic perform object reference identity equality (`===`) rather than ID value equality. Consequently, cross-arm deduplication does not merge the two hits.
- **Rust Parity Alignment**: Rust intentionally mirrors this behavior in `kb-engine::retrieval` to preserve search result parity with TypeScript.
- **Next Step**: A deduplication pass by normalized chunk ID (`chunk.id.0` / `RecordId.toString()`) should be implemented simultaneously in both Bun and Rust runtimes in a future release.
