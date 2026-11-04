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

- **Relevance floor (`RET-2`).** `POST /v1/search` accepts an optional
  `minScore` (number) and the server accepts a default from `KB_MIN_SCORE`;
  both are unset by default, so an unconfigured server is rank-identical to
  before. The floor compares a score that is comparable across queries — the
  reranker score when a reranker ran, otherwise the dense cosine similarity —
  never the fused RRF score, which is rank-only. An exact-match / identifier
  query (a quoted phrase, or a token that mixes letters and digits or has a
  `-`/`_`/`/` separator) is exempt when the keyword arm matched the hit and the
  hit's text literally contains the token. If the arm that supplies the score
  degraded, the floor is skipped and `floor_skipped` is reported instead of
  returning a silently empty answer; if the floor removes every hit, the
  response is empty with a `note` and `stats.floorScoreKind` /
  `stats.floorDropped`. `chunks[].retrieval.rerankScore` is now exposed
  (additive) so thresholds can be calibrated offline.
- **Off-topic evaluation sets and a floor calibrator (`RET-2`).**
  `rust/crates/kb-eval/sets/off-topic/` holds 55 Indonesian and 55 English
  questions written to have no answer in MIRACL-id and ViDoRe v3 HR/CS;
  `rust/crates/kb-eval/scripts/floor_calibrate.py` sweeps thresholds over
  per-query search responses and reports off-topic empty rate against in-domain
  nDCG@10. The thresholds themselves are **not** shipped as a default until the
  calibration has been run.

- **Recency and authority boosts (`RET-4`).** `POST /v1/search` accepts an
  additive, optional `boost` object:
  - `recency: {field, halfLifeDays, weight}` — `field` is `createdAt`,
    `updatedAt` or `metadata.<key>`; `halfLifeDays` is a number `> 0`; `weight`
    is in `(0, 1]`.
  - `authority: {field, weight}` — `field` is `metadata.<key>` holding a JSON
    number; `weight` is in `(0, 1]`.

  A hit's base relevance is its `rerankScore` when a reranker scored it (passed
  through a sigmoid only when the request's rerank scores are logits outside `[0, 1]`),
  otherwise `1/(1+rank)`; the final value is
  `base * (1 - w_r + w_r * 0.5^(age_days/halfLife)) * (1 - w_a + w_a * normalised_authority)`.
  Authority is min-max normalised per request over the candidate documents
  that carry the value. A missing or unparseable field means factor `1.0` —
  never an error. The re-sort breaks ties on content, page and chunk index
  (never a UUID). The boost runs after fusion, the rerank and the relevance
  floor, and before the final `maxChunks` cut; the floor keeps comparing the
  raw rerank/dense score. Absent (or an empty object) means **off**: no factor
  is computed and no re-sort happens, so the ranking is identical to a server
  before this release. `stats.boost: {recency, authority}` reports how many
  returned hits carried a usable field; it is absent when the boost was off.
  Metadata keys follow the `RET-3` rule (`[A-Za-z0-9_.-]{1,64}`); a bad shape,
  field, half-life or weight is a **400**.
- **Upload metadata (`RET-4`).** `POST /v1/documents` accepts an additive
  multipart field `metadata`: a JSON object stored as `Document.metadata`, so
  a caller can set the `effectiveDate`/`authority` (or any other) values the
  boosts and filters read. Absent keeps the stored metadata as before; a value
  that is not a JSON object is a **400**.
- **Dated evaluation set (`RET-4`).**
  `rust/crates/kb-eval/sets/dated-versions/` holds 55 document families with
  2023/2024/2025 editions and 55 "current/latest" questions, judged only on
  the newest edition; `generate.py` (Python stdlib, deterministic) rebuilds it.
  `kb-server eval` gains `--boost <json>` so one tenant can be measured with
  the boost off and on without re-ingesting, and dataset metadata sidecars
  (`metadata/<key>.json`) are uploaded with each document.
- **Adaptive arm choice (`RET-5`, off by default).** `POST /v1/search` accepts an
  optional `adaptiveArms` (boolean) and the server accepts a default from
  `KB_ADAPTIVE_ARMS`. When on, the query is classified as `exact`, `factual`,
  `structural`, `relational`, `aggregative` or `topical` (English and Indonesian
  cues), and the arms that class cannot use are not launched: an exact ID or
  quoted phrase skips the page-image and entity-graph arms; a structural
  question ("chapter 2", "page 12") skips the graph; an aggregative one ("main
  themes", "list every", "how many documents") skips the page-image and figure
  arms. Factual, relational and topical queries run every arm, exactly as
  before. Skipping means the arm is not called at all — no query embed, no
  store round trip — which is where the latency win comes from. The response
  names the decision, additively and only in adaptive mode:
  `stats.queryClass`, `stats.skippedArms` (`[{ "arm": "pageImage", "reason":
  "exact query" }]`) and `stats.armPlanFallback`. If the plan returns nothing
  and a skipped arm could have contributed candidates, the full plan runs once
  and `armPlanFallback: true` says so; a failed search is never reported as an
  empty one. Off, the pipeline is the one every earlier release ran. The eval
  runner accepts `--adaptive-arms` / `--no-adaptive-arms` (or
  `KB_EVAL_ADAPTIVE_ARMS`), writes `latency.json` per query, and
  `kb-server eval classify --queries FILE` prints the class distribution using
  the same rules.

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
