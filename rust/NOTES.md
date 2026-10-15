| 0 | scaffold (no compile) | — | uncompiled |
| 1 | previous agent | — | spent |
| 2 | previous agent | — | spent |
| 3 | previous agent | — | spent |
| 4 | previous agent | — | spent |
| 5 | previous agent | — | spent |
| 6 | previous agent | — | spent |
| 7 | cargo clippy --workspace --all-targets --keep-going --message-format=short | 2.0s | kb-ports unresolved imports (fixed) |
| 8 | cargo clippy --workspace --all-targets --keep-going --message-format=short | 3.5s | queries unwrap and retrieval as_storage (fixed) |
| 9 | cargo clippy --workspace --all-targets --keep-going --message-format=short | 2.0s | retrieval chunk_type Option and fakes test suites (fixed) |
| 10 | cargo clippy --workspace --all-targets --keep-going --message-format=short | 0.7s | kb-engine fakes trait alignment (fixed) |
| 11 | cargo test --workspace | 2.5s | kb-server compilation fixes (fixed) |
| 12 | cargo test --workspace | 8.25s | 95 passed across all crates, 1 assertion mismatch in server_tests (fixed) |
| 13 | cargo clippy --workspace --all-targets --keep-going --message-format=short | 16.8s | kb-engine (lib) 1 error, error text not captured (first run output tail-truncated). Many kb-inference/kb-store-surreal missing_docs and pedantic warnings. kb-engine tests.rs unused imports (ChunkId, ChunkMetadata, DocumentMeta, SearchHit). |
| 14 | cargo clippy --workspace --all-targets --keep-going --message-format=short (second run to capture error text) | 16.8s | same single kb-engine (lib) error; output was grep-filtered for `^error` and the error body still not shown |
| 16 | cargo test --workspace | 38s | config test asserted removed KB_MIN_SIMILARITY (fixed, now 0.30 constant) |
| 17 | cargo test --workspace | ~40s | all pass: kb-core 71, kb-engine 10, kb-inference 10 (+ others), kb-server 4+6+3+5 |
| 15 | cargo clippy --workspace --all-targets --keep-going (full output saved) | ~40s | exit 0, zero `error:` lines. Fixed: Vec<Vec<f32>> ownership in vector arm; test-module lint allow in kb-server config; server_tests EngineSettings fields. Warnings remain (missing_docs, pedantic) and are reported. |
| 16b | cargo clippy --workspace --all-targets --keep-going --message-format=short | 3.8s | exit 0, zero errors; incremental. Remaining warnings: pedantic casts, too_many_lines in embedder.rs:130 |
| 18 | cargo test --workspace | ~25s | all pass after R1 finish: kb-inference 16 (+6: LLM reranker parse/select/prompt/key/short-circuit, LRU recency), kb-engine 10, kb-core 71. Clippy run 16 (3.8s, incremental, exit 0, zero errors). |
| 19 | cargo clippy --workspace --all-targets --keep-going --message-format=short | 9s | kb-inference lib-test: 2 `expect()` in new llm_reranker tests (fixed with test-module allow) |
| 20 | cargo clippy --workspace --all-targets --keep-going --message-format=short | 9s | exit 0, zero errors after fix |
| 21 | cargo test --workspace | ~30s | all pass: kb-core 71, kb-engine 10, kb-inference 16, kb-server 6+4+3+5 |

## Live stack (R2)

| Service | Result |
|---|---|
| postgres:16 | started via `docker compose up -d postgres surrealdb`; healthy |
| surrealdb:v2 (2.7.0) | running; port 8000 not published to host, used `docker compose exec surrealdb /surreal sql` |
| minio (quay.io/minio/minio) | NOT USED. `docker compose pull` fails: `quay.io ... 401 UNAUTHORIZED`. |
| rustfs/rustfs:latest | substitute for minio, run as standalone container `rantai-kb-rustfs` on network `rantai-kb_default` with the compose credentials (kbadmin / kbsecret123), bucket `rantai-kb`. Verified with amazon/aws-cli, path-style: create-bucket, put, get, delete all exit 0 and get returned the uploaded bytes. The TS adapter uses only Put/Get/Delete (src/service/adapters.ts). |
| createbucket | replaced by aws-cli create-bucket against RustFS (same result: bucket exists) |
| kb (Bun/Rust app containers) | not started, per plan |

Postgres schema: `prisma/migrations/20260819110721_init/migration.sql` applied with psql (no Prisma CLI in repo). Tables: ApiKey, Category, Document, DocumentGroup, IngestJob, KnowledgeBase.

SurrealDB schema: `src/lib/rag/store/schema.surql` applied with `surreal sql --multi` (zero parse errors). Vector index is MTREE dimension 4096 and rejects other sizes.

Correction found live: chunks are NOT in Postgres. Chunks live in SurrealDB `document_chunk`. The Rust Postgres store queried a non-existent `"DocumentChunk"` table. Fixed: Postgres `chunk_count` is 0, and the server fills it from `ChunkStore::chunk_counts`; `ordered_chunks` moved from `DocumentStore` to `ChunkStore`.

Live SurrealDB checks (SurrealDB 2.7.0, probe rows removed afterwards):
- `SELECT document_id, count() ... GROUP BY document_id` returns counts; a document with no chunks is absent (maps to 0).
- `ORDER BY chunk_index ASC` returns chunks in index order.

## R2 live results (Rust, kb-server, live Postgres + SurrealDB 2.7)

Bugs found and fixed during the live run:
- chunks_meta.rs: `ordered_chunks` and `chunk_counts` read the raw SurrealDB envelope instead of rows (now `extract_rows`). Detail endpoint returned 400 "bad row".
- routes.rs list_documents: an absent `knowledge_base_ids` param was treated as an empty filter, so an unrestricted key got `[]`. Now absent = unfiltered, matching TS `restrictKnowledgeBases`.
- Added `src/main.rs` binary (kb-server had no entrypoint).

Results against seeded tenant_r2 (2 docs, 3 chunks):
- GET /v1/documents: both docs, chunk_count 2 and 1 (from SurrealDB). PASS
- GET /v1/documents?knowledge_base_ids=kb_r2: only doc_r2a. PASS
- GET /v1/documents?knowledge_base_ids=kb_nope: []. PASS
- GET /v1/documents/doc_r2a: 2 chunks in chunk_index order. PASS
- GET /v1/documents/doc_missing: 404. PASS
- GET /v1/knowledge-bases: kb_r2 with document_count 1. PASS
- GET /health, /v1/formats: 200. PASS
- Tenant isolation: second tenant (tenant_other) gets [] for list and KB list, and 404 for tenant_r2's doc. PASS
- Auth without key: 401. PASS

Not verified: POST /v1/search (no embedding key; embedder is unavailable offline). Bun parity not yet run.

Known difference to check: TS restrictKnowledgeBases treats an explicit empty requested list as "all of the key's KBs"; Rust treats an explicit empty value as no KBs. Recorded for R2 parity.

## R2 parity against Bun (same live stores)

Bun ran from the repo (`bun src/server.ts`, PORT 18081, Surreal ws RPC, prisma generated after `bun install`).

Shape and contract differences found (TS is the reference):
- Envelopes: TS wraps lists and objects (`{documents}`, `{knowledgeBases}`, `{document, chunks}`). Rust returns bare arrays and objects.
- Field casing: TS camelCase (`knowledgeBaseIds`, `documentCount`, `chunkCount`, `createdAt`). Rust snake_case (`chunk_count`, `document_count`).
- Missing fields in Rust list/detail: `fileType`, `fileSize`, `mimeType`, `createdAt`, `updatedAt`, `knowledgeBaseIds`, `figures`. Rust DocumentMeta (kb-ports types.rs) lacks them.
- Query param: TS reads `knowledgeBaseIds`; Rust reads `knowledge_base_ids`. Live Bun with `knowledgeBaseIds=kb_nope` returns []; Rust ignores the TS name.
- Not-found message: TS "Not found", Rust "not found".

Status after the follow-up pass (user: shape may differ if functionality is the same or better):
- FIXED: `knowledgeBaseIds` (Bun name) is read by the Rust list route; `knowledge_base_ids` kept as alias.
- FIXED: empty filter value is "absent" (TS parseList); restricted key with empty filter gets all its KBs (TS restrictKnowledgeBases). Verified live: rust ids == bun ids for all 4 filter cases.
- FIXED: Rust docs carry fileType, fileSize, mimeType, createdAt, updatedAt, knowledgeBaseIds, chunk_count (list and detail). Verified live.
- Intentional shape difference: Rust returns bare arrays and snake_case; Bun uses envelopes and camelCase. Same data, different field names.
- NOT PORTED: detail `figures` (read from document metadata JSON). Bun detail includes it; Rust does not yet.
- Minor: Bun chunk objects carry `id`; Rust chunk views do not. Chunk text, order, and counts match.
- Not-found message: Rust "not found", Bun "Not found" (same status 404).

## Dependencies

Dependencies added across slices:
- `zip` (8.6.0, deflate only): in-memory zip archive extraction for OpenXML (docx/pptx), epub containers, odt/ods containers, and test fixture generation.
- `quick-xml` (0.41.0): streaming event-based XML parsing for docx document bodies, pptx slide text, epub OPF manifests, generic xml text stripping, and odt/ods content.xml parsing.
- `calamine` (0.36.1): pure-Rust spreadsheet parser reading xlsx/xlsm/xlsb sheets into CSV text, matching SheetJS behavior.
- `pdfium-render` (0.9.4, `thread_safe` + `image_latest`): Dynamic binding to libpdfium for PDF text-layer extraction, page count, document splitting, and rendering PDF pages to image bitmaps.
- `image` (0.25, `png` + `jpeg`): Pure-Rust raster image decoding and encoding for PDFium bitmap-to-PNG conversion used in vision OCR data URLs.
- `base64` (0.22.1): Standard Base64 encoding for data URLs sent to OpenRouter / OpenAI vision APIs.

## R4 slice: document DELETE (soft and hard)

Compile/test ledger for this slice (one cargo process at a time):
- clippy29 (empty knowledgeBaseIds fix): exit 0
- clippy31 (figures): exit 101, test fixtures missing `metadata`; fixed
- clippy32: exit 0; test33: all suites pass
- clippy35 (DELETE route + port methods): exit 101, fakes missing new methods; fixed
- clippy36, clippy38 (DELETE tests): exit 0; test37, test39: all suites pass, kb-server 8 tests

Behaviour (TS deleteDocument, src/service/api.ts:475):
- needs kb:write (403 otherwise); tenant-scoped lookup (404 otherwise)
- soft (default): sets deletedAt; chunks and blob kept
- hard (?hard=true): chunks removed first (failure logged, not fatal), then the row (DocumentGroup cascades)

Known gap: the blob (S3) is not removed. There is no blob-store port yet (R3). The
hard-delete response reports the stored key in blob_key_pending so it can be cleaned up.

Live verification (Rust and Bun, same stores, scratch documents):
- read-only key -> 403 on DELETE (Rust)
- write key soft delete -> 200 on both; reads 404 on both; list excludes both; rows keep deletedAt
- hard delete scratch docs -> 200 on both; rows removed
- hard delete doc_r2b on Rust: chunks_removed=1; SurrealDB count 1 -> 0; doc_r2a chunks untouched (2)
- doc_r2b restored afterwards (Postgres row from saved SQL; chunk from seed_surreal.surql line 4); both servers again show 1 chunk

Response shape difference: Rust returns extra fields chunks_removed and blob_key_pending; Bun returns {ok:true}. Additive.
Error text differences (cosmetic, already noted): Rust "not found"/"insufficient permissions" vs TS "Not found"/"API key lacks scope kb:write".

## R3 slice 1: job claiming, chunk writes, worker loop

Cargo ledger for this slice (one process at a time, budget 8):

| # | command | result |
|---|---|---|
| 22 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. Errors: `kb_core::progress` private (6 unresolved imports), `tick` match arm returned `JobOutcome`, `jobs.rs` `map_or` could not unify `Result<JobRecord, _>` with `Ok(None)`. Warnings only elsewhere. |
| 23 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101, 5 errors left: three `BlobStore::upload` calls missing `.await` in the new worker tests, two `ChunkToStore.metadata` literals typed `ChunkMetadata` instead of `serde_json::Value`. |
| 24 | `cargo test --workspace` | did not compile: `harness()` was not `async` but awaited `blobs.upload`. |
| 25 | `cargo test --workspace` | compiled; 3 failures — the three tests that called `run_once()` without queueing a job, so the fake claim returned `None`. |
| 26 | `cargo test --workspace --no-fail-fast` | 5 failures. `sign_v4_matches_aws_get_vanilla_vector` caught a real bug: `hmac_sha256_raw` was called `(data, key)` instead of `(key, data)`, so the derived signing key was wrong. Two test-authoring errors: a raw-string literal had lost its backslashes, and the harness never inserted the fixture document. |
| 27 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0, zero errors. Remaining warnings are `missing_docs`/`pedantic`, the same class the earlier phases accepted. |
| 28 | `cargo test --workspace --no-fail-fast` | all suites pass: kb-core 76, kb-engine 25, kb-inference 16, kb-ports 3, kb-store-blob 6, kb-store-pg 9, kb-store-surreal 14, kb-server 9. |

SigV4 is now pinned to the AWS `get-vanilla` test vector, so a signing regression fails
a unit test rather than a live S3 call.

Not wired to a binary yet: there is no `Extractor` adapter in this slice, so a worker
process would have nothing to extract with. `IngestWorker` is the composition seam and
takes every port as a constructor argument.

## R3 slice 2: heartbeat, independent reclaim, extraction dispatch

Cargo ledger (continuing numbering from 31):

| # | command | result |
|---|---|---|
| 30 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. One error: `ingest_tests.rs` could not see the new `HEARTBEAT_INTERVAL` (it is re-exported from `ingest`, not from the crate root). Everything else warnings. |
| 31 | same | exit 0, zero errors after the import fix. |
| 32 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. 47 errors across kb-extract (test unwrap lints, trait bounds, elided lifetimes) and kb-server state.rs (build_worker / build_ingest_worker signature). |
| 33 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. 2 errors: `zip_read.rs` open return lifetime and `rtf.rs` copied() type mismatch. |
| 34 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0, zero errors. Clean clippy across workspace. |
| 35 | `cargo test --workspace` | exit 101. 15 test failures in kb-extract (html collapse whitespace, docx text run tracking & blank lines, pptx tag matching, epub self-closing tags & spine order, rtf index advancement & latin-1 chars, gltf json root object check). |
| 36 | `cargo test -p kb-extract` | exit 101. 1 error: `kb-ports` re-export of `scope::adapter` failed when compiled without `scope-adapter` feature. |
| 37 | `cargo test --workspace` | exit 101. `kb-ports` scope-adapter fix verified. 1 failure in `rtf::tests::ignorable_destinations_are_dropped` where `\*` did not mark the enclosing group ignorable. |
| 38 | `cargo test --workspace` | exit 101. `rtf.rs` had incorrect imports `use crate::error::ExtractError` and `ExtractionResult::text_only`. |
| 39 | `cargo test --workspace` | exit 101. `rtf.rs` import path and return signature fixed to `Result<String, ExtractError>`. |
| 40 | `cargo test --workspace` | exit 0. All 238 tests pass across all workspace crates (kb-chunker: 76, kb-engine: 27, kb-extract: 68, kb-inference: 16, kb-ports: 3, kb-server: 9 unit + 10 integration, kb-store-blob: 6, kb-store-pg: 9, kb-store-surreal: 14). |
| 41 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0. Zero errors across workspace. |

### Architecture and Parity Details
- **kb-extract crate**: pure-Rust extraction library implementing `kb_ports::Extractor` via `LocalExtractor`. Dispatches by MIME type / extension mapping ported from `src/lib/files/mime-types.ts`, `src/lib/rag` (`detectFileType`), and `src/lib/ingest/extract.ts`.
- **Pure-Rust parsers**:
  - `docx`: parses `word/document.xml` using `quick-xml`, matching `mammoth.extractRawText` (blank line between paragraphs, document order, skips field codes, note references, and DrawingML pictures).
  - `xlsx`: parses workbook sheets using `calamine`, formatting each sheet as `=== SheetName ===\n` followed by SheetJS-compatible CSV rows with quote escaping.
  - `pptx`: parses slides in numeric order (`slide1.xml`, `slide2.xml`, ...) followed by slide notes, paragraphs delimited by newlines).
  - `epub`: parses `META-INF/container.xml`, locates OPF package file, reads manifest and spine itemrefs to extract chapters in linear reading order, then strips HTML tags.
  - `rtf`: pure-Rust byte scanner stripping control words, parsing `\'hh` hex escapes and `\uN` unicode escapes, collapsing whitespace, and dropping `{\*...}` ignorable groups.
  - `gltf` / `glb`: parses GLTF JSON metadata and GLB binary chunks, formatting an inventory of asset info, scenes, nodes, meshes, materials, and animations matching the TS template.
  - `html` / `xml` / `text` / `code`: HTML strips script/style tags, converts tags to spaces and collapses whitespace without entity decoding (matching TS); XML strips tags with entity decoding; code blocks wrap in markdown fences.
- **Legacy binary formats & unported inputs**:
  - Legacy `.doc`, `.xls`, `.ppt`, `.odt`, `.ods` formats return explicit `ExtractError::Unsupported` naming the format and explaining unported `officeparser`.
  - PDF and image files return explicit `ExtractError::Unsupported` noting they are not yet ported in this slice.
- **UTF-16 reason clipping**: error message clipping follows JavaScript's UTF-16 code unit semantics (`slice(0, 500)`).
- **IngestWorker wiring**: `kb-server::state::build_worker` and `build_ingest_worker` instantiate `LocalExtractor::shared()`. Integration test `test_ingest_worker_with_real_local_extractor` verifies end-to-end ingest execution with the real extractor.

## R3 slice 3: PDF text layer, scanned detection, smart router, layout extractor chain, vision OCR, ODT/ODS parity

Cargo ledger (continuing numbering from 41):

| # | command | result |
|---|---|---|
| 42 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. 5 errors in `kb-server` config (PgSettings/SurrealSettings/S3Settings missing fields, EngineSettings vector_knn field mismatch, expansion api_key type) and unwrap_err T: Debug in `kb-extract` `builder.rs`. |
| 43 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. Trailing backslash in `builder.rs:129`, raw string hash lint in `openrouter_vision.rs`. |
| 44 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. `clippy::panic` lint in `builder.rs` test module, unused imports in `extract_tests.rs`. |
| 45 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0, zero errors. Clean clippy across workspace. |
| 46 | `cargo test --workspace` | exit 101. 1 failure in `kb-extract` `hybrid_merge::tests::substitutes_prose_while_keeping_table` (anchor words typo location in test). |
| 47 | `cargo test --workspace` | exit 101. 1 failure in `kb-extract` `extract_tests::test_smart_router_decision` (`is_unpdf_sufficient` 300 chars/page default threshold vs 10 chars fixture). |
| 48 | `cargo test --workspace` | exit 0. All 304 tests pass across all workspace crates (kb-chunker: 76, kb-core: 38, kb-engine: 28, kb-extract: 86 unit + 9 integration, kb-inference: 16, kb-ports: 3, kb-server: 9 unit + 10 integration, kb-store-blob: 6, kb-store-pg: 9, kb-store-surreal: 14). |
| 49 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0. Zero errors across workspace. |

### Architecture and Parity Details
- **PDF text layer via PDFium**: Dynamically loaded `libpdfium` replaces `unpdf`. Paths are resolved via `PDFIUM_DYNAMIC_LIB_PATH` or auto-located in `rust/.vendor/pdfium/lib/libpdfium.so` and system paths. Script `rust/scripts/fetch-pdfium.sh` automatically downloads prebuilt binaries for linux-x64 and arm64.
- **Output difference & Smart Router Re-validation Trap**: PDFium extracts text glyphs directly from PDF operators, which differs subtly from Mozilla's `pdf.js` (used by `unpdf` in Bun) in whitespace collapse, hyphens, and ligatures. As anticipated in `docs/porting-plan.md`, the smart router thresholds (`min_chars_per_page = 300`, `max_columnar_lines = 5`, `max_currency_matches = 10`) need re-validation against the 40-doc benchmark suite once end-to-end evaluation runs.
- **Scanned-PDF detection**: `is_pdf_scanned` ports `src/lib/ocr/pdf-converter.ts` threshold (`non-whitespace characters < 100 * pages`).
- **Smart router**: `SmartRouterExtractor` uses `kb_core::signals::is_unpdf_sufficient` to evaluate text-layer quality, falling through to the configured layout chain or vision model on scans or tables. Infinite recursion (`smart -> smart`) is rejected.
- **Layout extractor chain**: `LayoutExtractorChain` tries on-prem MinerU sidecar (`KB_EXTRACT_MINERU_BASE_URL`), hosted MinerU API (`KB_MINERU_API_KEY`), and Mistral OCR (`KB_MISTRAL_OCR_KEY`) in the order specified by `KB_LAYOUT_EXTRACTOR_ORDER` (default `sidecar,mineru-api,mistral`), falling back to vision OCR if all layout extractors fail.
- **Dropped legacy Ollama OCR pipeline**: Dropped the legacy Ollama OCR provider (`src/lib/ocr` ollama-provider) in favor of the OpenRouter / OpenAI-compatible vision provider (`src/lib/ocr/providers/openrouter-provider.ts`).
- **Vision OCR & image inputs**: Scanned PDF fallback and standalone image files (`png`, `jpg`, `jpeg`, `gif`, `webp`, `heic`) are processed via `OpenRouterVisionProvider` using `google/gemini-2.5-flash` (or `OCR_FALLBACK_MODEL`). Image extractions are formatted as `[Image: {filename}]\n\n{text}` exactly matching `src/lib/ingest/extract.ts`.
- **Hybrid prose merge**: `HybridExtractor` and `merge_structural_with_text_layer` preserve MinerU/Mistral structural elements (markdown/HTML tables, headings, LaTeX formulas, code fences) while anchoring and substituting high-fidelity text-layer prose.
- **ODT and ODS parsers**: Added pure-Rust parsers for OpenDocument Text (`.odt`) via `content.xml` `text:p`/`text:h` and OpenDocument Spreadsheet (`.ods`) via `content.xml` `table:table-row` to CSV.
- **Investigation of legacy binary formats**: Inspected `officeparser` v6 (`node_modules/officeparser`). Confirmed that `officeparser` v6 only supports ZIP-based XML formats (`.docx`, `.pptx`, `.xlsx`, `.odt`, `.ods`, `.pages`, etc.) and explicitly rejects legacy binary `.doc`, `.xls`, and `.ppt` with error code `EXTENSION_UNSUPPORTED`. Rejecting legacy binary formats with `ExtractError::Unsupported` in Rust maintains full parity with TypeScript.
- **Production worker wiring**: `kb-server::state::build_worker` and `build_ingest_worker` wire `build_production_extractor(&config.extract)` as the active `Extractor` implementation.

## R3 slice 4: Full IngestWorker & Retrieval Parity (Runner, Policy, Chunker, Entities/Relations, Figures, Embeddings, Figure Gate, Error Prefix)

Cargo ledger (continuing numbering from 49):

| # | command | result |
|---|---|---|
| 50 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. Corrupted edits and missing fields across kb-extract, kb-inference, kb-store-surreal, kb-engine. |
| 51 | `cargo check -p kb-engine` | exit 0. Engine and core compiled cleanly. |
| 52 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. Single test error in kb-extract (missing `document_type` field in `ExtractionOptions` fixture). |
| 53 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0. Zero errors across all workspace targets. |
| 54 | `cargo test --workspace` | exit 0. All 331 tests pass across all workspace crates (kb-chunker: 76, kb-core: 54, kb-engine: 32, kb-extract: 86 unit + 10 integration, kb-inference: 20, kb-ports: 3, kb-server: 9 unit + 10 integration, kb-store-blob: 6, kb-store-pg: 9, kb-store-surreal: 16). |
| 55 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0. Zero errors across all workspace targets. |

### Architecture and Parity Details
- **IngestWorker line-by-line parity** (`kb-engine/src/ingest.rs`):
  - Checks tenant ("job has no tenant"), missing documentId/s3Key with identical error text.
  - S3 blob download failure error text identical to TypeScript.
  - Parameter parsing: `figureMode`, `forceOCR`, `documentType`, `title`, `categories`, `subcategory`, `useCombined` default true.
  - Policy resolution: `resolve_ingest_policy` matching `src/lib/ingest/policy.ts`.
  - Title fallback to filename without extension.
  - NUL character stripping (` ` removed) matching TypeScript ingest pipeline.
  - Pre-indexing document update (`content`, `fileType`, `fileSize`) before chunking and embedding.
  - Progress reporting sequence matching TS exactly: `extracting` -> `chunking` -> `extracting_entities` (0/N and step_current/step_total) -> `processing_figures` -> `embedding` (0/N, N/N) -> `storing` -> `ready` status -> `done` step -> job success.
  - Non-fatal error handling for entities and figures: errors logged and pipeline proceeds to completion.
  - Document and job marked `failed` with matching error message on fatal errors.
- **Smart chunking parity** (`kb-core`):
  - IngestWorker invokes `smart_chunk_document` with `{max_chunk_size: 800, overlap_size: 200, preserve_code_blocks: true, respect_heading_boundaries: true}`.
  - Validated chunk metadata fields: `title`, `category`, `subcategory`, `chunk_index`, `page`.
- **Entities & Relations** (`kb-core/src/intelligence/`, `kb-inference/src/llm_extractor.rs`, `kb-store-surreal/src/entities.rs`):
  - Pure-Rust pattern extractors for universal (emails, URLs, phones, dates), financial (currencies, percentages), and technical (API endpoints, functions, env vars, error codes) with deduplication and pattern relation extractors.
  - LLM combined extractor and entity extractor with verbatim TypeScript prompts, JSON fence stripping, thinking tag stripping, stack-based truncated JSON repair, and OpenRouter client settings.
  - SurrealDB storage: `UPSERT entity:<documentId>_<sanitizedName>` with sanitized record IDs and parameter-bound values; `RELATE source->RELTYPE->target` with relation type sanitization (`.toUpperCase().replace(/[^A-Z0-9_]/g, "_").replace(/^_+|_+$/g, "") || "RELATED_TO"`).
  - Respects `KB_ENTITY_EXTRACTION_ENABLED=false` and `policy.entities`.
- **Page map assignment** (`kb-core/src/page_map.rs`):
  - Invokes `assign_chunk_pages` to tag chunks with their 1-based page numbers whenever a page map is produced during extraction.
- **Figure assets processing** (`kb-core/src/figure_assets.rs`):
  - Uploads figure crops to `BlobStore` under `documents/{tenantId}/{documentId}/assets/fig-p{page}-{index}.png`.
  - Figure chunks created with caption fallback from neighbouring text blocks and `pages_blocks` reading-order anchoring.
  - Monotonic `chunk_index` reindexing across combined text and figure chunks.
  - Document metadata `figures` patch persisted via `DocumentStore::update_metadata`.
- **Embedding & Storage parity** (`kb-engine/src/ingest.rs`):
  - Embedding input formatted as `${title}

${chunk.content}`.
  - Records `embedding_model` and metadata on chunks (`chunkType`, `assetKey`, `anchorChunkIndex`).
  - Delete-then-store idempotency pattern matching TS `storeChunks`.
- **Figure gate veto parity** (`kb-core/src/figure_gate.rs`, `kb-inference/src/vlm_gate.rs`, `kb-engine/src/retrieval.rs`):
  - Pure figure gate config and verdict parsing ported from `src/lib/rag/figure-gate.ts`.
  - VLM figure gate evaluator using OpenAI-compatible vision completion endpoint with strict prompt.
  - Wire into `retrieval.rs` to filter candidate figures up to `top_n` and `max_keep`.
- **Single OCR error prefix bug fix** (`kb-extract/src/chain.rs`, `kb-extract/src/dispatch.rs`):
  - Fixed double prefix bug on scanned-PDF fallback failure; prevents `OCR failed for "x": OCR failed for "x": ...`.
  - Verified by integration test `test_scanned_pdf_chain_failure_single_error_prefix`.

## R4 slice 1: inherited port methods reconciled (documents, jobs, KB, categories, intelligence)

Cargo ledger (continuing numbering from 55):

| # | command | result |
|---|---|---|
| 56 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101. Inherited uncommitted work had two literal `\n`/backslash corruptions (`kb-store-surreal/src/entities.rs`, `kb-ports/src/traits.rs`), a `{` on the wrong line in `fakes.rs`, and fakes written against a different trait signature than the ports. |
| 57 | same | exit 101, 5 error groups: `Ordering` imported twice in fakes, `patch_document` missing from `FakeDocumentStore`, `KnowledgeBaseId` vs `String` comparisons, `ChunkToStore.metadata` is raw JSON (not `ChunkMetadata`). |
| 58 | same | exit 101, 2 errors: unbalanced delimiters in the `KnowledgeBaseRecord` constructions in `kb-store-pg/src/store.rs`. |
| 59 | same | exit 101, 1 error: same delimiters after the first fix. |
| 60 | same | exit 101, 1 error. A regex repair of the previous edits deleted most of `store.rs`; reverted to `HEAD` and rewrote the block by hand. |
| 61 | same | exit 101, 1 error: `kb-store-pg/src/auth.rs` spawns the best-effort `lastUsedAt` stamp, so the crate needs `tokio`. |
| 62 | same | exit 0, zero errors. |
| 63 | `cargo test --workspace` | exit 0, every suite passes. |

Decisions taken in this slice:
- `RetryJobOutcome::Success { job_id, document_id }`: resetting the job and flipping the
  document back to `processing` are separate operations in `retryJob`
  (src/service/api.ts:667-683), and the document update is explicitly allowed to fail.
  Keeping them apart in the port is what preserves that.
- `JobStore::create_job` takes a `CreateJobInput` (no tenant argument): the tenant is on the
  input, so the fake and the Postgres store cannot disagree about whose queue a job lands in.
- Progress frames are throttled inside the worker (`PROGRESS_EMIT_MIN_GAP`, 900 ms, always on a
  step change) so the SSE stream gets the same frames the Bun worker emits.
- `IntelligenceStats` is computed in the HTTP layer, not the port: the TypeScript handler
  derives it from the rows it already has.

## R4 slice 2: every remaining route, CORS, the process model

Cargo ledger (continuing numbering from 63). This slice had no new dependencies and
no budget cap, so the runs are listed with the errors each one fixed:

| # | command | result |
|---|---|---|
| 64 | `cargo clippy -p kb-server --all-targets --keep-going --message-format=short` | exit 101. Wrong `axum::extract::middleware` path, leftover exports, `EngineError` or-pattern binding `msg` in one arm only, an `expect` where the deny lint forbids one. |
| 65 | same | exit 101, 19 errors: `SearchRequestDto.query` is now raw JSON (the route owns the 400), `FromRequest` not imported for `Multipart`, `ProgressSink` not implemented for `Arc<EventBus>`. |
| 66 | same | exit 101, 5 errors: `Multipart::from_request` needs state, `Bytes` vs `Vec<u8>`, `Engine::retrieve` takes `Option<u32>`. |
| 67 | same | exit 101, 1 error: `Default for EventBus` returned `Arc<Self>`; a tokio interval also needs `reset()` so the first ping is not immediate. |
| 68 | same | exit 101, 1 error: `Default` impl removed, interval `reset()` added. |
| 69 | same | exit 101, 1 error: `StreamExt` not in scope for `chain`. |
| 70 | same | exit 101, 1 error: the SSE `unfold` closure captured the tenant by move and the `async move` block moved it out of an `FnMut`; the tenant now travels in the stream state. |
| 71 | same | exit 0, lib clean. |
| 72 | same | exit 101, 4 errors: envelope structs returned by `taxonomy` were private. |
| 73 | same | exit 101, 1 error: `Maintenance` fields are set by the composition root. |
| 74 | same | exit 101, 1 error: one test still referenced the old `doc_store` binding. |
| 75 | same | exit 0, zero errors. |
| 76 | `cargo test -p kb-server` | exit 101, 1 failure: `extensions_are_matched_case_insensitively` asserted `archive.tar.gz` was accepted. The route is right and the test was wrong — only the final extension counts and `.gz` is not on the list. |
| 77 | `cargo test -p kb-server` | exit 0, all suites pass. |
| 78 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0, zero errors across all workspace targets. |
| 79 | `cargo test --workspace` | exit 0, every suite passes. |

## R4 slice 3: integration tests for every route, SSE and shutdown

Cargo ledger (continuing numbering from 79):

| # | command | result |
|---|---|---|
| 80 | `cargo test -p kb-server --test upload_tests` | exit 101, compile error: the harness helper compared `&str` with `str`, and `FakeBlobStore`'s map is `objects`, not `blobs`. |
| 81 | same | exit 0, 8 tests pass. |
| 82 | `cargo test -p kb-server --test document_tests` | exit 101, 1 failure: `FakeDocumentStore::list` ignored the knowledge-base filter, so the route could not be tested. The fake now filters the way `SELECT_DOCS_WITH_KB` does. |
| 83 | `cargo test -p kb-server --test media_tests` | exit 101, 1 failure: the expected `content-length` was wrong (14 bytes, not 15). |
| 84 | same | exit 0, 9 tests pass. |
| 85 | `cargo test -p kb-server --test catalog_tests` | exit 101, 1 failure: the cross-tenant fixture seeded the knowledge base under the wrong tenant, so the route correctly found it. |
| 86 | same | exit 0, 16 tests pass. |
| 87 | `cargo test -p kb-server --test events_tests` | exit 0, 9 tests pass. |
| 88 | `cargo test -p kb-server --test runtime_tests` | exit 101, 2 failures: `serve` only stopped on an OS signal, so a test could not cancel it. `shutdown_signal` now waits on the cancel token as well as SIGINT/SIGTERM, which is also what an embedder of this library needs. |
| 89 | same | exit 0, 4 tests pass. |
| 90 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 101, 2 findings: an `await_placeholder()` left in the runtime fixture, and a dead helper referencing a removed type. |
| 91 | same | exit 0, zero errors. |
| 92 | `cargo test --workspace` | exit 0, every suite passes (see the per-crate counts in the R4 summary). |
| 93 | `cargo fmt -p kb-server -p kb-ports -p kb-store-pg -p kb-store-surreal -p kb-engine -p kb-inference` | no compile cost; the drift committed as a separate style commit. |
| 94 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0, zero errors. |
| 95 | `cargo test --workspace` | exit 0, every suite passes. |

### Deviations from the TypeScript service, on purpose
- **A restricted key that asks for knowledge bases it does not hold gets `[]`**, where
  `listDocuments` would fall through its `kbFilter?.length` test and return the whole
  tenant. Reproducing that would hand a restricted key documents it may not see, so the
  empty answer is kept.
- **Every authenticated route requires `kb:read`.** The `Authed` extractor enforces it
  before the handler, so `GET /v1/jobs/:id` and `GET /v1/knowledge-bases`, which check no
  scope at all in TypeScript, answer 403 (with the same message) to a write-only key.
- **`degrades` is an extra field on search results**, present only when a degradable backend
  actually failed. Everything else about the search response is unchanged.
- **`limit` is parsed leniently** (`?limit=abc` falls back to 100) instead of producing the
  500 that `take: NaN` produces in TypeScript.

## R4 slice 4: delete-after-soft-delete, config coverage, formats/health auth

| # | command | result |
|---|---|---|
| 96 | `cargo test -p kb-server --test document_tests` | exit 101, 1 failure: a hard delete could not follow a soft delete, because `DocumentStore::find_meta_by_id` excluded `deletedAt` and the fake's `hard_delete` only looked at live rows. The port method now takes `include_deleted` (TS's delete route does not filter on `deletedAt`) and the fake removes soft-deleted rows too. |
| 97 | `cargo test -p kb-server` | exit 0, all suites pass. |
| 98 | `cargo fmt -p kb-server -p kb-ports -p kb-store-pg -p kb-engine` | free (no compile). |
| 99 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0, zero errors. |
| 100 | `cargo test --workspace` | exit 0, every suite passes. |

Configuration covered by `config::tests`: `KB_WORKER_ENABLED`, `KB_INGEST_CONCURRENCY`,
`KB_INGEST_POLL_MS`, `KB_INGEST_STALE_MS`, `KB_INGEST_MAX_ATTEMPTS`, `KB_INGEST_RECLAIM_MS`,
`KB_INGEST_REAP_DAYS`, `KB_INGEST_REAP_MS`, `KB_VERSION`, `ENTITY_EXTRACTION_*`, including the
`Math.max` floors and the fact that only the exact string `"false"` disables the worker.

Worker concurrency: `KB_INGEST_CONCURRENCY` is applied as jobs claimed per tick, which the worker
runs one after another. With the default of 1 this is identical to the TypeScript worker; above 1
the Rust process claims a wider batch per poll but still indexes one document at a time.

## R4 slice 5: search parity for the explicit-empty knowledge-base list

| # | command | result |
|---|---|---|
| 101 | `cargo test -p kb-server` | exit 101, 2 compile errors in `server_tests.rs`: the shared helpers were imported without the module declaration landing in the same edit. |
| 102 | `cargo test -p kb-server` | exit 0, all suites pass, 12 in `server_tests`. |
| 103 | `cargo clippy --workspace --all-targets --keep-going --message-format=short` | exit 0, zero errors. |
| 104 | `cargo test --workspace` | exit 0, every suite passes. |

`restrict_kbs` now returns an explicit empty list as `Some([])` instead of
collapsing it to "no filter". In TypeScript `[]` is truthy and has length zero,
so `POST /v1/search` with `knowledgeBaseIds: []` answers with the note
"API key is not bound to any of the requested knowledge bases" even for a
full-access key; the list route is unaffected because an absent parameter still
parses to "no filter".
