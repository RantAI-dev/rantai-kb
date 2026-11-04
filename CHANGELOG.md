# Changelog

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
