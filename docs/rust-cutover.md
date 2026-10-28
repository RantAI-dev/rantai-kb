# Runbook: Rust Cutover & Migration (`rantai-kb`)

This runbook guides operators through rolling out the Rust implementation of `rantai-kb` into production, operating in a mixed fleet alongside the Bun service, and rolling back if necessary.

> **2026-10-25: SurrealDB was removed from the Rust service.** Rust is
> Qdrant-only for chunks, vectors, BM25 and page images. Bun still uses
> SurrealDB and is unchanged. The two runtimes share Postgres (documents, jobs,
> KBs, keys, entity graph) and the HTTP API, **not** the chunk store. Read §0
> before using any older section of this runbook.

## 0. SurrealDB removed (2026-10-25)

The decision is final: Qdrant is the only chunk store for Rust. `KB_CHUNK_STORE`
is no longer a switch — leaving it unset and setting `qdrant` both mean Qdrant,
and any other value fails at boot with a message naming the removal and
`kb-server reindex`. The `kb-store-surreal` crate, the SurrealDB entity write in
ingest, every `SURREAL_*` variable, the compose service and the Rust CI
dependency are gone.

Why a reindex instead of a data migration: the originals are in S3 and the
document rows are in Postgres, so the chunks can be rebuilt exactly by
re-ingesting them. Reading SurrealDB from Rust would keep two chunk-store
clients alive indefinitely to move data once.

### Cutover procedure

1. **Point Rust at the same Postgres and S3** the Bun service uses, and at a
   reachable Qdrant (`QDRANT_GRPC_URL`). Unset `KB_CHUNK_STORE` (or set
   `qdrant`). Start Rust with the worker on (`KB_WORKER_ENABLED=true`).
2. **Dry run.** `kb-server reindex --dry-run` prints live documents per tenant,
   how many will be queued, and how many are skipped because they have no
   `s3Key` (no original to rebuild from) or already have a pending/processing
   job. Resolve the blocked rows before a real run.
3. **Reindex.** `kb-server reindex` (optionally `--tenant <id>`) queues one
   normal ingest job per queueable document. The existing worker claims them,
   reads the S3 original, and writes chunks/vectors/BM25/page images to Qdrant.
   Nothing about the ingest path is special-cased.
4. **Watch the queue** through `GET /v1/jobs/:id` or the worker log until the
   jobs finish; then run `GET /v1/search` on the documents you expect and check
   `degrades` is empty.
5. **Switch search traffic to Rust** once the index answers. Keep Bun running
   (and its SurrealDB store intact) until the new answers are verified.

**Cost.** On a paid tier step 3 re-embeds every chunk. That is charged per
input unless `KB_CACHE_DIR` already holds those embeddings; set the cache before
the first run and keep `KB_EMBEDDING_MODEL`/`KB_EMBEDDING_DIM` unchanged. Local
tiers pay in CPU time instead.

**What was deleted with the shared store.** `rust/scripts/mixed-fleet.sh` and
`rust/scripts/search-parity.sh` are gone: both asserted that Bun and Rust read
and wrote one shared chunk store, and no such store exists. References to them
below are historical and describe how the shared-Postgres contract was checked
while it existed. The `KB_CHUNK_STORE=surreal` path is not a fallback.

---

## 1. Prerequisites

Before starting cutover, ensure the following infrastructure and settings are verified:

1. **Databases**:
   - **PostgreSQL 16**: Accessible via `DATABASE_URL`.
   - **Qdrant**: Accessible via `QDRANT_GRPC_URL` (default `http://127.0.0.1:16334`). The Rust service creates its collections at boot; `kb-server apply-schema` does the same without serving. Existing Bun data is rebuilt from S3 with `kb-server reindex` (§0), not read from SurrealDB.
2. **Object Storage**:
   - S3-compatible bucket (`S3_BUCKET`, default `rantai-kb`) with read/write access.
3. **Inference Providers**:
   - `KB_EMBEDDING_BASE_URL`, `KB_EMBEDDING_API_KEY`, and `KB_EMBEDDING_DIM` (default: 4096).
4. **Binary & Container Images**:
   - Docker image: `rantai-kb:rust` (or release binary `kb-server`).

---

## 2. Migration Ownership

The PostgreSQL schema is shared between Bun and Rust. The chunk store is not: Bun writes SurrealDB, Rust writes Qdrant.

### PostgreSQL Migrations

- The Rust migration runner (`kb-server migrate` or `kb_store_pg::migrate`) executes `prisma/migrations/*/migration.sql` in directory order.
- Each migration is recorded in Prisma's native `_prisma_migrations` table using the identical column schema and SHA-256 hex checksum over the migration file.
- **Locking**: A PostgreSQL session advisory lock (`7241331885`) prevents race conditions between multiple booting containers.
- **Idempotency**: Already-applied migrations are safely skipped.
- **Ownership**: Both `bunx prisma migrate deploy` and `kb-server migrate` agree on what is applied. Either binary can manage migrations.
- **One ledger, and Rust reads Prisma's rows**: Prisma creates `_prisma_migrations` with `timestamptz` timestamp columns, so the Rust runner asks for its applied/unapplied decision in SQL (`"finished_at" IS NOT NULL AND "rolled_back_at" IS NULL`) and decodes only the `TEXT` checksum. Decoding those timestamps in Rust cannot work against both engines — `DateTime<Utc>` fails on a timestamp without a zone and `NaiveDateTime` fails on one with — and the failure was silent, because the error was swallowed into "not applied" and every run re-applied every migration. Before it was removed with the shared chunk store, `rust/scripts/mixed-fleet.sh` ran Prisma's deploy **first**, on an empty database, and required the Rust runner to then apply zero migrations, so a regression here failed the gate instead of a rolling deploy.

### Entity graph tables (`20261017120000_graph_tables`)

The entity graph lives in four plain Postgres tables — `KbEntity`, `KbMention`, `KbRelation`, `KbGraphVersion` — with the traversal done in Rust over a CSR (`docs/architecture-target.md` §11, step L.3). Since 2026-10-25 it is the only entity store in Rust; ingest extraction and the intelligence route both use it.

- **Additive, and proven to be.** The migration creates four new tables and their indexes. It alters no existing table, column, index or constraint. Before its removal with the shared chunk store, `rust/scripts/mixed-fleet.sh` asserted this rather than assuming it: Prisma's own engine applied both migrations from an empty database, the four graph tables were counted inside the container, and the Rust runner had to apply none of them.
- **Bun does not read them yet.** Nothing in `src/` touches these tables, so a Bun instance and a Rust instance write to the same Postgres without seeing each other's graph rows. `docs/eval-baseline.md` §"GraphStore (L.3a)" has the measurements.
- **The scope rule is enforced in SQL, not in Rust.** Both graph reads carry `"documentId" = ANY(<scope>)`, so a relation extracted from a document the caller cannot see is never fetched and therefore cannot become a *path* through the graph. Filtering results instead of rows would still leak the existence and shape of an out-of-scope document: two visible entities joined only by an out-of-scope relation would become connected. `kb_ports::graph_contract` tests that case directly (`no_path_through_an_out_of_scope_document`) rather than inferring it.
- **Ids are derived, not generated.** An entity id is `sha256("kb-entity", tenant, key)` and a relation id `sha256("kb-relation", tenant, src, dst, type, document)`, so re-ingesting a document replaces rows instead of adding a second set, and two runs of the same ingest write byte-identical rows. A `uuid()` default would make every re-ingest a different row set.

### Qdrant collections

- `kb-server apply-schema` creates the chunk collection (`chunks__<model>__<dim>`) and, when
  `KB_PAGE_EMBEDDING_MODEL` is set, the page collection (`pages__chunks__<model>__<dim>`).
  `serve` does the same on boot, so a fresh deployment needs no separate DDL step.
- The dense width comes from `KB_EMBEDDING_DIM` (or the tier preset); the sparse vector and
  payload indexes are created with the collection. Creation is idempotent.
- **Important**: the width is fixed when the collection is created. Changing embedding models
  or dimensions creates a new collection; re-ingest the corpus (`kb-server reindex`) into it.

---

## 3. Rolling deployment (Qdrant-only, from 2026-10-25)

The shared-Postgres contract still holds: Rust and Bun can run against the same
Postgres (documents, jobs, KBs, keys) and the same S3 bucket, and either worker
claims jobs with `FOR UPDATE SKIP LOCKED`. The chunk index is separate, which
changes the phase order.

### Phase 1: Build the Qdrant index

Deploy Rust with the worker on and run the §0 reindex. Do **not** run a Bun
worker during this phase: a Bun worker would claim the reindexed jobs and write
their chunks to SurrealDB, leaving Rust with no index for those documents.
The API can stay on Bun (`KB_WORKER_ENABLED=false` there) while Rust only runs
the worker.

### Phase 2: Route read/search traffic to Rust

Point `GET /v1/*` and `POST /v1/search` at Rust. Check that documents ingested
before the cutover answer from Qdrant and that `degrades` is empty. API keys
created by either runtime authenticate on both, because they live in Postgres.

There is no live Bun-vs-Rust parity matrix any more: the two runtimes no longer
share a store, so a diff would compare two different indexes. Rust's own
retrieval and leak gates are `rust/scripts/ci-eval-gate.sh` and
`cargo test -p kb-store-qdrant --features integration`.

### Phase 3: Route write traffic to Rust

Move `POST /v1/documents`, `PATCH` and `DELETE` to Rust and verify SSE progress
on `GET /v1/events`. New uploads are ingested into Qdrant from here on.

### Phase 4: Retire Bun

Once all traffic is on Rust and the Qdrant index has been verified, stop the Bun
containers. Keep the SurrealDB data until the rollback window in §4 closes; it
is the only copy of Bun's chunk index.

---

## 4. Rollback Plan

1. **Before Rust has ingested anything new**, roll back by switching traffic
   back to Bun and re-enabling its worker. Bun's SurrealDB store is untouched,
   so its answers are exactly what they were.
2. **After Rust has ingested or reindexed documents**, Bun cannot serve those
   chunks: Rust wrote them to Qdrant and Bun cannot read Qdrant. The documents
   and originals are safe in Postgres and S3, but rolling back means Bun has to
   re-ingest them (`POST /v1/documents` again, or a rebuild into SurrealDB),
   which may re-embed. Decide the rollback window before starting §0 and keep
   the SurrealDB volume until it closes.
3. **No schema rollback**: the Postgres migrations are additive and shared; do
   not downgrade them.

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
  - Rust adds a top-level `degrades` list to `POST /v1/search` results when degradable backends (e.g. reranker or query expander) fail, allowing clients to detect partial degradations without failing the entire search. Labels are snake_case: `vector_arm_failed`, `bm25_arm_failed`, `expansion_unavailable`, `rerank_unavailable`, `neighbours_partial`, `figures_partial`, `graph_arm_unavailable`, `page_arm_unavailable`, and `vision_rerank_unavailable` (the opt-in live vision rerank failed or timed out; the page order is unchanged).
- **Lenient Query Parameter Parsing**:
  - Query parameters like `limit=abc` fall back gracefully to default `100` rather than causing an unhandled 500 error (`take: NaN` in Prisma).
- **Additive Delete Response**:
  - `DELETE /v1/documents/:id` in Rust returns additive fields (`chunks_removed` and `blob_key_pending`) in addition to `{ "ok": true }`.
- **Chunk ids are Bun's record form**:
  - Every chunk id the API emits — `chunks[].id` on `POST /v1/search`, `chunks[].id` on `GET /v1/documents/:id` — is `document_chunk:⟨{document_id}_{chunk_index}⟩`, the string Bun's `RecordId.toString()` produces (`String(row.id)` in `src/service/api.ts`). Internally the port's `ChunkId` stays the bare ident; the wrapper is applied once, at the wire boundary (`kb-server/src/dto.rs::chunk_id_for_client`), so an id a client stored from a Rust answer is the same string it would have stored from Bun, and `chunk_id_from_client` accepts either form (wrapped, bare, or the backticked rendering a different SurrealDB endpoint may produce). Before 2026-10-24 Rust answered the bare ident, which broke clients and the parity matrix.

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
- **Page-image arm row** (Rust only; Bun has no page arm): run on a stack with
  the arm on (`KB_STACK_PAGE_ARM=1 rust/scripts/stack-up.sh`)
  and a PDF corpus (`kb-server eval leak --pdf …`). The stub's
  `/page-embeddings` gives every page and query one vector, so every foreign
  page is a perfect match and only the scope keeps it out. 2026-10-23: 0 leaks
  over 336 probes; page PASS in tenant / knowledge base / category (positive
  10/12), UNTESTED in `documentIds` (that branch never runs the page arm). With
  the page index *and* the chunk fetch unscoped in a throwaway change, the page
  row reported 224 leaks. With only the page index unscoped it reported 0: the
  chunk behind a page is fetched under the caller's scope, so the black box
  cannot see a page-index hole on its own — `kb-engine/src/page_arm_tests.rs`
  and `kb-server/tests/leak_tests.rs` watch that layer.

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

### 10. Deliberate Difference: Keyword Hits Carry Their Page and Section
- **Bun**: a chunk found only by the keyword (BM25) arm is serialised without
  `page`, `chunkType` or `assetKey`, and with `section: null` and
  `contextualPrefix: null`; the keyword arm's query selects neither the
  chunk's page nor its metadata.
- **Rust (deliberate, 2026-10-23)**: the store holds the page and the metadata
  of every chunk, so a keyword-only hit now carries `page` and `section` when
  they are known (each omitted/null, as before, when it is not). `chunkType`,
  `assetKey` and `contextualPrefix: null` keep Bun's shape.
- **Why**: in a sampled ViDoRe v3 HR query, 9 of the 20 chunks returned were
  keyword-only; without a page they cannot be cited, and a client ranking
  pages silently loses them. The same argument holds for the heading: it is
  what a source card shows next to the document title, and dropping it would
  make a keyword-only hit look like a chunk from a document with no headings.
  The context text and `sources` already carried both.
- **Parity**: `search-parity.sh` drops `page` and `section` from keyword hits
  before diffing. Neither field can be compared there — Bun's keyword arm never
  fetched them — so the matrix compares the fields both runtimes can produce
  for that chunk.

### 11. Deliberate Difference: Neighbour Chunks Follow the Ranked Hits
- **Bun**: neighbour chunks (`KB_NEIGHBOR_WINDOW`, default 1) are woven into
  the `chunks` list next to their hit, in chunk order, so a preceding
  neighbour can come before the hit it belongs to, even the top one.
- **Rust (deliberate, 2026-10-23)**: `chunks` lists the ranked hits first,
  in rank order, then the neighbours and figures. The `context` text keeps
  the woven reading order, so the LLM still reads a neighbour next to its hit.
- **Why**: a client ranks and cites `chunks` in order. Woven in, neighbours
  cost 0.033 page nDCG@10 on `vidore3-hr-en` (0.435, against 0.468 with
  neighbours off), and the top-ranked hit could be returned second.
- **Parity**: chunk order is already compared as a set (§7).

### 12. Deliberate Difference: Cross-Arm Duplicate Chunks Are Merged
- **Bun**: when the dense and the keyword arm match the same chunk, the
  response can list that chunk twice — once with its cosine `similarity`, and
  once as a keyword hit with `similarity: 0`, `section: null` and
  `contextualPrefix: null`.
- **Root cause (Bun)**: both arms run `SELECT id, … FROM document_chunk`, but
  the SurrealDB driver returns a fresh `RecordId` object per row.
  `src/lib/rag/retriever.ts` keys its candidate pool (`pool.has(b.id)`) and its
  RRF accumulator on those objects, and JavaScript `Map` compares them by
  reference, so the same chunk's two `RecordId` instances are two keys and are
  never merged. The ids are equal as *strings* — `document_chunk:⟨…⟩` — which
  is why the duplicate surfaces in the response and not in the store.
- **Rust (deliberate, 2026-10-24)**: `ChunkId` is a value, not an object
  reference, so `RetrievalIndex::search` merges the arms by ident
  (`kb-store-surreal/src/retrieval_index.rs::fuse`, the in-memory index and the
  fakes do the same). A chunk both arms found is one entry carrying both arms'
  ranks and scores, reported in `retrieval` (§5).
- **Why**: a duplicate is never the better answer. It spends one of the
  caller's `maxChunks` slots, renders the same passage twice in `context`, and
  gives the client a second copy of a source with `similarity: 0`, which ranks
  as if it were irrelevant. Nothing is lost by merging: the additive
  `retrieval` block says which arms found the chunk.
- **Parity**: `search-parity.sh` drops Bun's duplicate before diffing — it
  groups the normalised `chunks` by id and keeps the copy with the highest
  `similarity`. The dedupe is applied to Bun's answer only, so a Rust
  regression that started emitting duplicates again would still fail the
  matrix.

---

### 13. Additive: `externalRef` and health egress (2026-10-28)

MOCO-KF uses rantai-kb as an alternative vector-plane engine. It uploads each
Knowledge Unit as a small markdown document with `externalRef = <ku_id>`, maps
search hits back to KUs, re-indexes a KU idempotently, and refuses to send
content to a deployment that would forward it off-host. Three additive fields
serve that, with no schema change:

- **`GET /v1/documents?externalRef=<ref>`** returns only the caller tenant's
  live documents whose `Document.externalRef` is exactly `<ref>`. The filter
  composes with `limit` and `knowledgeBaseIds`; the tenant and `deletedAt`
  predicates stay in the same SQL query, so the ref can never widen scope.
  Every list row now carries `externalRef: string | null`.
- **`POST /v1/search`** adds `externalRef: string | null` to every chunk. It is
  resolved from the document record at the same point as `documentTitle`
  (`kb-engine/src/retrieval.rs::enrich_titles_cached`, and the `documentIds`
  branch in `kb-server/src/routes.rs`), not stored in the Qdrant payload, so old
  points need no reindex and yield `null` rather than an error.
- **`GET /health`** (and `/v1/health`) adds `tier` (`KB_TIER`, or `null`) and
  `egress`: the sorted, de-duplicated hosts of configured outbound model
  endpoints that are **not** local. Local means loopback, RFC1918,
  link-local, `fc00::/7`, or a single-label hostname such as `tei-lite`;
  disabled features contribute nothing. The classifier is pure
  (`kb-server/src/egress.rs`) and tested per address family and URL shape.
  `cloud` reports `["openrouter.ai"]`; `lite` and `lite-en` report `[]`.

Bun produces none of these fields. A client diffing the two runtimes should
ignore them, as it already ignores `retrieval` (§5).

---

## 6. Shared Known Issues

### Duplicate Chunks Across Retrieval Arms (Bun only; Rust merges — §12)
- **Behavior**: When both the vector search arm and the BM25 search arm match the
  exact same chunk, Bun can list that chunk twice in the returned `chunks` array
  (once with its cosine vector similarity score and once with `similarity: 0.0`,
  `section: null` and `contextualPrefix: null` from the keyword arm). Rust does
  not: it merges the two hits into one (Deliberate Difference §12).
- **Root Cause**: In TypeScript (`src/lib/rag/retriever.ts`), vector chunks and
  BM25 chunks arrive as separate SurrealDB `RecordId` object instances.
  JavaScript `Map.has()` and `scores.get()` in the reciprocal rank fusion (RRF)
  logic perform object reference identity equality (`===`) rather than ID value
  equality. Consequently, cross-arm deduplication does not merge the two hits,
  even though both ids stringify to the same `document_chunk:⟨…⟩`.
- **Rust**: merged by `ChunkId` in the store adapter's fusion, reported in the
  additive `retrieval` block (§5). See §12 for the difference and its rationale.
- **Parity**: `search-parity.sh` groups the normalised `chunks` by id and keeps
  the highest-`similarity` copy of Bun's answer before diffing; the dedupe is
  applied to Bun only, so a Rust duplicate would still fail the matrix.
- **Next Step**: Bun is frozen. If it is ever unfrozen, key the pool and the RRF
  accumulator on `String(id)` instead of the `RecordId` instance.

