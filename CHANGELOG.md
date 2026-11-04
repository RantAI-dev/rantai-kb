# Changelog

## Unreleased

### Added

- **Search-time metadata filters (`RET-3`).** `POST /v1/search` accepts three
  additive, optional fields:
  - `externalRefs: string[]` — only documents whose `Document.externalRef` is in
    the list (`[]` selects nothing).
  - `language: string` — only documents whose JSONB `metadata` key `language`
    equals the value. There is no dedicated `Document.language` column; the
    metadata key is the stored language.
  - `metadata: { "<key>": <scalar or [scalar]> }` — top-level JSONB metadata
    equality; an array value matches any of its entries, and a document whose
    JSONB value is an array matches when it contains one of them. Keys must
    match `[A-Za-z0-9_.-]{1,64}` and values must be strings, numbers, booleans
    or arrays of those; anything else is a 400.

  All three are applied by `DocumentStore::build_scope`, so the narrowed
  `DocumentScope` reaches dense, BM25, page-image, figure, neighbour, graph and
  entity retrieval before ranking — never as a post-filter. They compose with
  `knowledgeBaseIds`, `documentIds` and the key's knowledge-base limit by
  intersection, so a filter can only narrow what the key may see. `documentIds`
  now also respects the key's knowledge-base limit (before, that branch built a
  tenant-wide scope).
- **Document structure read (`ING-3`).** `GET /v1/documents/:id/structure` returns
  `documentId`, `externalRef`, `title`, `pages` (`number`, `hasTextLayer`, `ocrText`,
  `imageAssetKey`), `figures` (`key`, `page`, `bbox`, `caption`, `ocrText`, `assetKey`)
  and `chunks` (`chunkId`, `chunkIndex`, `page`) — the chunk → page map. It is additive:
  every value comes from a record ingest already wrote. Pages come from the page-image
  index's page units plus every page a text or figure hit can reference; figures come
  from the figure chunks retrieval can return, merged with the document row's
  `metadata.figures` for `bbox` and `caption`, and include figure rows that have no
  chunk (keyed by their asset key). The route enforces the tenant and the key's
  knowledge-base limit exactly like the other by-ID routes (`SEC-1`): another tenant,
  a document outside the key's knowledge bases, and a document in no knowledge base
  all answer the same 404.
- **Page provenance at ingest.** Page units now carry `hasTextLayer`, `ocr`, and
  `image` (whether the page JPEG was written), derived from the extraction result, so
  the structure read does not infer them from text. The text index is unchanged.

### Notes

- **Structure caps.** The route reads at most 20,000 text units and 2,000 page units
  (the adapter scrolls in 512-point windows) and sets `truncated: true` when either cap
  is reached, instead of silently returning a short list.
- **Nulls, not guesses.** A document ingested before this release has no per-page
  provenance or image-success marker, so its `hasTextLayer`, `ocrText` and
  `imageAssetKey` are `null`; its pages and figures are still listed. Figure OCR is not
  extracted or stored today, so `figures[].ocrText` is always `null` (the field stays
  so a caller's shape does not change when it is). `ocrText` is the page's stored text
  (at most 8,000 characters) with the caption stage's `[Visual] …` description
  stripped. `pages[].number` is 0-based, matching `chunks[].page`.

## 0.2.0 — 2026-11-04

The first Rust release. `v0.1.0` remains the last Bun release.

### Added

- **Rust service.** `kb-server` is the production runtime: HTTP API, ingest worker,
  retrieval engine and CLI (`serve`, `create-api-key`, `migrate`, `apply-schema`,
  `reindex`, `setup`, `eval`, `bench`).
- **Qdrant chunk store.** Chunks, dense vectors, BM25 sparse vectors and page images
  live in Qdrant.
- **Page-image retrieval.** Whole pages are embedded as images and fused with text
  retrieval: +0.035 page nDCG@10 on ViDoRe v3 HR and +0.044 on CS over text-only,
  both with 95% CIs above zero (`docs/eval-ab/rk-cloud-vs-kf/`).
- **Deployment tiers.** `lite`, `lite-en`, `desk`, `studio`, `station` and `cloud` share
  one API and are selected with `KB_TIER`. `rust/scripts/kb-dev.sh setup` detects the
  hardware, checks the budget and model licences, and writes `rust/deploy/.env`.
- **Content-addressed disk cache.** Embeddings, OCR output and captions are cached under
  `KB_CACHE_DIR`, keyed by model and content hash.
- **Measured results.** Cloud tier scores 0.845 nDCG@10 on MIRACL-id-200 against KF
  main's 0.813; the CPU lite tier scores 0.802 on MIRACL-id-50 with a 42 ms median
  search on two cores.

### Fixed

- **Knowledge-base limits on by-ID routes (`SEC-1`).** A key limited to some knowledge
  bases now gets 404 for any document outside them on `GET`/`PATCH`/`DELETE
  /v1/documents/:id`, `/raw`, `/assets`, `/intelligence`, `/v1/jobs/:id`, retry and
  `/v1/events`. The v0.1.0 Bun service did not check this.
- **Deterministic ranking ties.** Equal fused scores are broken on the chunk's text, page
  and index instead of the per-ingest document id, so re-ingesting the same corpus gives
  the same order.

### Changed

- Container image `ghcr.io/rantai-dev/rantai-kb` is built from `rust/Dockerfile` for
  amd64 and arm64.
- `rust/deploy/compose.yaml`, driven by `rust/scripts/kb-dev.sh`, runs the stack. The image
  runs `kb-server migrate` and `apply-schema` on boot (skip with `KB_SKIP_MIGRATE=1`),
  then `kb-server serve`.

### Removed

- **Bun/TypeScript service.** `src/`, `tests/`, `scripts/*.ts`, `package.json`,
  `bun.lock`, `tsconfig.json`, `vitest.config.ts`, the root `Dockerfile` and the root
  `docker-compose.yml` are gone. Check out tag `v0.1.0` to run the Bun service.
- **SurrealDB.** The Rust service is Qdrant-only; no `SURREAL_*` setting is read.
- Bun CI jobs (typecheck, boundary, unit tests, image build, smoke test) and the Bun
  release image.

### Breaking

- **Chunk-store migration.** A v0.1.0 deployment moves its index with
  `kb-server reindex`, which re-ingests the S3 originals into Qdrant. Follow
  `docs/rust-cutover.md`; do not point Rust at SurrealDB.
- **Configuration.** `SURREAL_*` variables are ignored; Qdrant is configured with
  `QDRANT_GRPC_URL` / `QDRANT_URL`.
- **Deployment files.** The root `Dockerfile` and `docker-compose.yml` no longer exist.
  Use `rust/Dockerfile` and `rust/deploy/compose.yaml` (or `kb-dev.sh`).
