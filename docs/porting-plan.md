# rantai-kb porting plan

Two independent tracks:

- **Python** — port rantai-kb's retrieval and formation techniques into
  [moco-kf](https://github.com/dirac-AI/moco-kf) (cloned at `/home/shiro/moco-kf`).
- **Rust** — port the standalone rantai-kb service to Rust, in this repo under `rust/`.

Written 2026-10-07 against rantai-kb `5e317c3` and moco-kf `9c7ce17`.

## Findings in rantai-kb that shape both ports

These come from reading the code. None was reproduced against a running stack.

1. **Probable cross-tenant leak in hybrid search.** `POST /v1/search` with
   `hybrid: true` and no `knowledgeBaseIds` or `category` runs an unscoped
   SurrealDB query: `resolveScopedDocumentIds` in `src/lib/rag/hybrid-search.ts`
   returns no scope, `document_chunk` has no tenant column, and the hybrid path
   skips the `DocumentStore` join the default path uses. The smoke test checks
   isolation only on the default path. **Still unfixed in TypeScript.**
2. **The vector-store port is not store-agnostic.** `VectorStore.query(sql)`
   passes raw SurrealQL, written inline across search, BM25, ingest and figure
   code.
3. **Tenant filtering happens after the top-K cut.** Vector and BM25 search all
   tenants and then drop foreign rows, so recall falls as tenants are added.
4. **Two retrieval pipelines, not one.** The default path has BM25, query
   expansion and rerank; the hybrid path has the entity arm, neighbour window and
   figure co-retrieval. They disagree on defaults (figure rerank floor 0.2 vs
   0.001). No single path matches the README diagram.
5. **Hard-coded language assumptions.** Indonesian caption words in figure
   matching, an English stemmer for BM25, a sentence splitter that needs a
   capital letter after punctuation.

## Track 1 — Python, into moco-kf

This is not a file-by-file port. moco-kf already has a governed retrieval fabric
(Qdrant, OpenSearch, ArangoDB, MOCO-KB, an OPA gate) and binding rules that
forbid about half of what rantai-kb does. The plan ports techniques into
moco-kf's existing stages and router.

### What is ported

| rantai-kb piece | moco-kf today | Action |
|---|---|---|
| Page map (chunk → page) | Docling path records no page; citations are page-less and the tree plane resolves coarsely | Carry page on blocks, write it to citations |
| Text-layer signals, PDF splitter | Every PDF goes through Docling at ~9 s/page | Skip table/OCR models per page; parse page ranges in parallel |
| Parallel search arms | Planes run one after another | Run planes concurrently |
| Neighbour window | None | Add, with neighbours passing the same OPA gate and activation pin |
| Query expansion | None | Add through the gateway, opt-in, recorded in the trace |
| TEI/vLLM embedding and rerank clients | Local CPU only | Add as on-platform backends behind existing interfaces |
| Figures | Images are deliberately not knowledge units | Phase 3, only after a design decision |
| Knowledge base grouping | Datasets and packs | Map knowledge base → dataset; no new code |

### What is not ported

- Answer prompt formatting, citation grounding, standalone-query rewriting,
  vision-at-answer: these belong to MocoAI (the fabric stops before reasoning).
- The SurrealDB chunk store and BM25, the Postgres job queue, API keys, the
  entity arm: moco-kf already replaces each.
- The 800/200 overlapping chunker: overlap would duplicate attributed text.
- Hosted MinerU and Mistral OCR: content would leave the platform.

### Phases

**Phase 1 — modules and wiring (in progress).** Six agents build new modules in
parallel, touching no existing file; one serial step wires them in.

| Stream | New file under `src/moco_kf/` |
|---|---|
| A. Page map | `workflows/page_map.py` |
| B. PDF routing signals and page-range split | `workflows/pdf_routing.py` |
| C. Neighbour window | `services/retrieval/neighbors.py` |
| D. Query expansion | `services/retrieval/query_expansion.py` |
| E. On-platform embedding and rerank clients | `shared/remote_inference.py` |
| F. Benchmark comparator for the rantai-kb service | `bench/rantai_kb_comparator.py` |
| G. Wiring (serial) | edits `normalization.py`, `activities.py`, `retrieval/router.py`, config, contract, design docs |

Everything new defaults to off. Output is a local branch
`feat/rantai-kb-baseline`; nothing is pushed.

**Phase 2 — measure and switch on.** Run the benchmark, formation and load tests
with each feature on and off, and against the rantai-kb comparator. Turn on only
what improves the numbers. Re-form existing sources so their citations get page
numbers. Needs the live stack.

**Phase 3 — figures as knowledge units (optional).** Crops in storage, anchoring
to surrounding text, co-retrieval, a vision gate. Reverses a stated moco-kf
design choice and raises image-rights questions; needs steward sign-off.

## Track 2 — Rust port of the standalone service

Lives in this repo under `rust/` on branch `feat/rust-port`. The HTTP API, the
Postgres schema and the SurrealDB schema stay as they are, so the Rust binary
can run against the same data as the Bun service during cutover.

### Design rules

- **Typed store traits.** No raw query strings cross a trait boundary.
- **Tenant scope is not optional.** Every vector and BM25 operation takes the
  tenant's visible document ids and filters inside the query. There is no
  unscoped search method to call by mistake. This fixes findings 1 and 3 in the
  Rust port without changing the stored schema.
- **One retrieval pipeline**, the one the README describes: expansion → vector
  and BM25 in parallel → RRF → rerank → neighbour window → figure co-retrieval →
  source numbering. The `hybrid` request field is accepted and ignored until the
  entity arm is ported.
- **Explicit context, no global registry.** The engine receives its stores as
  arguments.

### Workspace

| Crate | Contents | Heavy dependencies |
|---|---|---|
| `kb-core` | Chunker, RRF, neighbour weave, source numbering, figure policy, text-layer signals, page map, figure anchors, ingest policy, progress | none |
| `kb-ports` | Store and inference traits, shared types | none |
| `kb-inference` | Embedding and rerank HTTP clients | reqwest |
| `kb-store-pg` | Documents, knowledge bases, API keys, jobs | sqlx |
| `kb-store-surreal` | Typed vector, BM25, neighbour and figure operations over SurrealDB's HTTP `/sql` endpoint | reqwest |
| `kb-engine` | Retrieval orchestration (ingest later) | none |
| `kb-server` | axum routes, auth | axum |

### Structure and code standards

**Layering.** Dependencies point one way, and the Cargo graph enforces it (this
replaces the TypeScript `check:kb-boundary` script):

```
kb-core  ←  kb-ports  ←  kb-inference / kb-store-pg / kb-store-surreal
                      ←  kb-engine  ←  kb-server (composition root)
```

- `kb-core` is pure domain logic: no I/O, no async, no environment reads.
- `kb-ports` holds traits and domain types only.
- Adapters implement ports and know one external system each.
- `kb-engine` orchestrates through ports and never names an adapter.
- `kb-server` is the only crate that constructs adapters, reads configuration
  and knows HTTP.

**Workspace hygiene.** Edition 2024, resolver 3, `[workspace.package]`,
`[workspace.dependencies]` (every version declared once) and
`[workspace.lints]` inherited by every crate: `unsafe_code = forbid`,
`missing_docs = warn`, clippy `all` plus `pedantic` as warnings, and
`unwrap_used`, `expect_used`, `panic`, `todo`, `dbg_macro`, `print_stdout`
denied outside tests.

**Types carry the rules.**
- Newtype ids (`TenantId`, `DocumentId`, `ChunkId`, `KnowledgeBaseId`) so they
  cannot be swapped.
- A `DocumentScope` value that only `DocumentStore` can construct, required by
  every `ChunkStore` search method. A tenant-unscoped search does not compile.
- Enums, not strings, for chunk type, figure mode and job status.
- Wire DTOs live in `kb-server` and are separate from domain types; conversion
  is explicit.

**Errors.** One `thiserror` enum per library crate, no `anyhow` in libraries,
no panics. Degradable failures (rerank, expansion, figures) are values the
engine records, not errors it propagates. HTTP mapping happens in one
`IntoResponse` impl.

**Configuration.** One typed `Config` struct, loaded and validated once at
startup, failing fast with the variable name. No environment reads anywhere
else. A test asserts the struct covers the documented variable list.

**Async and I/O.** One shared `reqwest::Client`; a timeout on every outbound
call; search arms run with `tokio::join!`; bounded concurrency for batches; no
blocking work on the runtime; graceful shutdown.

**Server.** Thin handlers over a typed `AppState`; authentication as an axum
extractor producing `AuthContext`; `tower-http` tracing; structured `tracing`
spans on every port call.

**Queries.** Every value bound, never interpolated. SQL and SurrealQL
statements are named constants in one module per adapter.

**Tests.** Unit tests beside the code, table-driven. Engine tests run against
in-memory fakes of the ports. Server tests call the router in-process with
`tower::ServiceExt::oneshot`, with no network.

**Readability.** Module docs say why the module exists; comments explain
constraints, not mechanics; modules stay under roughly 400 lines; `rustfmt`
defaults.

### Build cost

Rust builds are slow and this machine has 6 cores and about 2 GB of free memory
while other work runs. The port is organised around that:

- The pure crates carry no heavy dependencies, so checking them takes seconds.
- SurrealDB is reached over HTTP with `reqwest` rather than the `surrealdb`
  crate, which is one of the slowest crates to compile. S3 uses a light client,
  not the AWS SDK.
- `sqlx` uses runtime queries, not compile-time-checked macros, so no database
  is needed to compile.
- Dependencies are pinned once at scaffold time; adding one later forces a
  rebuild and needs a reason.
- **Phase R1 has a hard budget of 9 compiling cargo runs**: at most 6
  `cargo clippy --workspace --all-targets --keep-going` and at most 3
  `cargo test --workspace`. Clippy is used in place of `cargo check` for every
  round, so lint findings cost no extra run. `cargo fmt` does not compile and
  is free. Nothing is compiled until
  all code for the phase is written and reviewed by reading. If the budget runs
  out, the agent stops and reports the remaining errors.
- No release builds, no `cargo clean`, no Docker image builds, one cargo
  process at a time, capped at two jobs.

### Phases

**R1 — core and read path.** Scaffold, then six parallel streams (three in
`kb-core`, plus `kb-inference`, `kb-store-pg`, `kb-store-surreal`), then serial
`kb-engine` and `kb-server` for `/health`, `POST /v1/search`,
`GET /v1/documents`, `GET /v1/documents/:id`, `GET /v1/knowledge-bases`,
`GET /v1/formats`.

**R2 — verify against live data.** Run the Rust read path beside the Bun service
on the same stores and compare responses. First point at which the SurrealDB
HTTP client is proven against a real instance.

**R3 — ingest.** Worker loop (job claiming is done in Postgres, so Rust workers
can run beside Bun API replicas), extractor clients, local parsers, figures,
embedding and storing. Local parsers (docx, pptx, epub, rtf) are the largest
piece.

**R4 — remaining routes and cutover.** Writes, SSE, CLI scripts, multi-arch
image, retire Bun.

Known traps: the sentence splitter uses lookbehind, which the `regex` crate
lacks; JS counts chunk sizes in UTF-16 units; the router thresholds were tuned
on pdf.js output and need re-validation on a pdfium-based extractor. The legacy
local OCR pipeline and the unused conversation-memory table are dropped.

## Open decisions

1. **Fix the hybrid-search leak in TypeScript.** Independent of both tracks and
   not assigned to anyone yet.
2. **Figures in moco-kf** (Python phase 3): approve the design change or stop
   after phase 2.
3. **Rust target.** MOCO-KB is already a Rust, axum and SurrealDB knowledge
   engine in the same ecosystem. Its repo was not reachable when this was
   written, so the overlap is unknown. This plan assumes a standalone port.
4. **Rust vector store.** This plan keeps SurrealDB v2 to avoid a data
   migration. Qdrant or pgvector would be a later change behind the same trait.
