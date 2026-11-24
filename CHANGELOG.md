# Changelog

## Unreleased

### Fixed

- **Passage prefixes are applied on the batch path (`ING-9`).** `embed_many` (used by ingest and
  stateless `/v1/process`) now sends every text through the same prefix rule as the single-text
  path: models that need it see `passage: ` before each chunk, so `/v1/encode mode=passage` and
  `/v1/process` produce vectors in the same space. Prefixes are applied exactly once, and providers
  whose instruction travels out of band (`input_type`, Voyage / OmniEmbed) still send no text
  prefix; the embedding disk cache keys only the prefixes actually applied.
  **Corpora indexed with `lite` (e5-small) before this fix are missing the prefix and must be
  re-indexed with `kb-server reindex`** to get the fix. Queries and other tiers are unaffected.
  The embedding disk cache (`KB_CACHE_DIR`) no longer reuses passage vectors cached under a
  non-empty passage prefix (they were stored unprefixed), so that re-index really re-embeds.
  One query-side change: a custom config that sets `KB_EMBEDDING_QUERY_PREFIX` **and** sends the
  instruction out of band (`KB_EMBEDDING_INPUT_TYPE=true` or `KB_EMBEDDING_FORMAT=omniembed`) no
  longer prefixes queries either. No shipped tier preset combines the two.

### Changed

- **`documentIds` searches now run the full retrieval pipeline (RET-11).** A search that names
  documents previously answered from a private dense-only branch: no BM25, reranker, page, tree,
  neighbour or figure arms. It now passes the same narrowed scope into the standard retrieval
  pipeline as any other search, so a caller searching inside chosen documents gets the same
  answer quality. The scope is still the intersection of the requested ids and what the key may
  see; it can never widen the key's tenant, knowledge-base or metadata limits. The response still
  carries `chunks`, and now also carries `sources` and `context` in the standard format (previously
  `sources` was absent and `context` was a plain `"[i] content"` list, and only when
  `format: "context"` was requested). `hybrid: true` is unchanged.

### Added

- **`KB_RERANK_BREADCRUMBS=off|on` (RET-12).** With the flag on, the reranker sees
  `"{document title} › {section path or section} · p.{page}\n"` before the chunk content, so a
  bare "berlaku 12 hari kerja" is judged with its "Kebijakan Cuti › Bab IV Cuti Tahunan · p.12"
  context. `KB_RERANK_CHAR_BUDGET` still truncates only the content at a UTF-8 boundary; the
  breadcrumb is never clipped. `false`/`true` are accepted too, an unknown value is a startup
  error, and the **default is `off`** until the A/B (`docs/core/specs/v1-roadmap.md` RET-12).
  additive: the response shapes and chunk content are unchanged and no re-index is needed.
- New chunk metadata `charStart` / `charEnd` / `overlapPrefixLen` on newly ingested chunks,
  carrying each chunk's character span and the length of the overlap prefix it repeats. Chunks
  ingested before this change (and non-prose units such as figures and table summaries) have no
  spans and read as absent, never as zero.
- Integer payload indexes on `page` and `chunkIndex` in Qdrant, so page- and index-scoped reads
  use range filters instead of scanning. Index creation is idempotent on an existing collection.
- Old chunks get spans on `kb-server reindex` (rebuild from the originals); no store migration is
  needed.
- **Outline v2 on `GET /v1/documents/:id/structure` (`RET-13`).** The response additively carries
  `outlineVersion: 2`, `sections` (each with an id, title, level, path, page/chunk ranges and a
  detected marker), document-level `estimatedChars` and `hasSpans`. Section sources are the stored
  outline, chunk headings, Indonesian legal markers (`BAB` / `Bagian` / `Pasal` / `Paragraf`) and
  a per-page fallback. A new scoped, vector-free `RetrievalIndex::list_chunk_meta` port method
  backs this and the upcoming content route. No existing response field changed.

## 0.8.0 — 2026-11-23

### Added

- **Stateless processor-only microservice mode (`kb-server serve --stateless` / `KB_MODE=stateless`).**
  Runs `rantai-kb` with **zero** Postgres, Qdrant, or S3 connections so external applications that
  own their own database and Qdrant server can use `rantai-kb` purely for document processing and
  inference:
  - `POST /v1/process` — synchronously extracts text (55 formats, pdfium + optional OCR), splits
    into table- and code-aware chunks, structures typed tables, anchors figures (with optional
    base64 PNGs), computes Qdrant-compatible BM25 sparse vectors, and optionally computes dense
    embeddings and entities/relations in one JSON response.
  - `POST /v1/encode` — encodes query or passage texts using the exact same dense prefixes and
    FNV-1a BM25 sparse tokenizer as `/v1/process` so external Qdrant retrieval matches indexing
    bit-for-bit. `mode: "query"` runs on a search fast lane and never queues behind bulk document
    ingestion holding `KB_SEATS` embed permits.
  - `POST /v1/rerank` — reranks candidate passages against a query through any configured local
    (TEI / vLLM / llama.cpp) or cloud (Cohere / Voyage / OpenRouter) reranker on the search fast
    lane, reusing `KB_RERANK_*` and `KB_TIER` presets (disable explicitly with
    `KB_STATELESS_RERANK=false`).
  - `GET /health` & `GET /v1/health` — reports `mode: "stateless"`, `capabilities` (including
    `embed` and `rerank`), active concurrency `limits` (`extractSeats`, `embedSeats`,
    `maxInflightProcess`), `backends`, `tier`, and `egress`.
  - `kb-server openapi --stateless` (and `GET /openapi.json` / `GET /docs` in stateless mode) —
    dedicated OpenAPI 3.1 contract for stateless deployments, leaving the stateful contract
    (`docs/openapi.json`) unchanged.
- **In-flight backpressure for stateless uploads (`KB_STATELESS_MAX_INFLIGHT`).** Bounds concurrent
  `POST /v1/process` uploads (default `max(extract_seats * 4, 4)`, `0` = unlimited) **before**
  buffering multipart file bytes into RAM, shedding excess bursts cleanly with `503 Service
  Unavailable` + `Retry-After: 2`.
- **Static API key authentication for stateless mode.** Authenticates `Authorization: Bearer` and
  `X-API-Key` against comma-separated keys in `KB_STATELESS_API_KEY` (constant-time SHA-256
  comparison), or requires explicit opt-out via `KB_STATELESS_ALLOW_UNAUTHENTICATED=true` on
  trusted private networks; refuses to boot if neither is set.
- **Engine `DocumentProcessor` API (`kb-engine`).** Exposes the reusable, adapter-free stateless
  processing, encoding, and reranking pipeline directly from `kb-engine`.

### Changed

- Cleaned up and reorganized `README.md` to clearly present both deployment modes (full service vs.
  stateless processor microservice), concise tier/model tables, and updated quick-start examples.

## 0.7.0 — 2026-11-20

### Added

- **An ingest job waits for a backend that is not there instead of failing (`#36`).** A job whose
  pages need OCR while the OCR backend is stopped, still starting, or gone mid-job now parks in
  `waiting_for_ocr` with the reason in the job's `error`, holds no worker slot, and is resumed by
  the worker itself when the wait is over — nobody presses retry, and the pages that were already
  read are not paid for twice. The wait doubles every try and is capped (`KB_OCR_WAIT_BACKOFF_S`,
  default 30 s; `KB_OCR_WAIT_MAX_S`, default 600 s; `KB_OCR_WAIT_MAX_ATTEMPTS`, default 10); after
  the last try the job fails for good, so a document does not wait forever. The document's status
  says `waiting_for_ocr` too, and `POST /v1/jobs/:id/retry` still refuses it (only a failed job can
  be retried). Additive: `IngestJob.status` and `Document.status` are TEXT columns, and one
  nullable column was added (`prisma/migrations/20261120130000_job_waiting_for_ocr`).
- **`GET /health` and `GET /v1/health` report the model backends (`#36`).** The body carries a
  `backends` object: the embedder, the reranker and the OCR backend, each `ready`, `starting` or
  `unavailable`, with the reason as a plain string. A client that uploads a scanned document can
  hold the upload until the OCR backend says `ready`. The status code of the health route is
  unchanged — a missing OCR model does not make the service dead. The probe is a `GET` on the
  endpoint the configuration names; it never reads the body, never runs a model, and its answer is
  reused for ten seconds. A probe that sees the OCR backend answer also closes the outage breaker,
  which is otherwise only closed by a page read, so a job that arrived inside the cooldown window
  was refused every page (`#35`).

- **An extraction quality gate, off by default (`KB_EXTRACT_QUALITY_GATE`, `#29`).** A page whose
  text layer is unusable (too few characters, not mostly letters or words, broken-encoding
  characters, far below the document's median page) is sent to OCR, and a document that still
  produces almost no text ends as `needs_ocr` with the job failed and the reason on the document,
  instead of a silent `ready`. Off by default because a false positive costs an OCR pass over a
  good page (paid, with a hosted provider). The numbers are recorded either way in
  `metadata.quality`. `GET /v1/documents` and `GET /v1/documents/{id}` gain `pages`,
  `charsExtracted` and `degrades` (additive; null or empty for documents indexed before this).
- **`Document.status` and `IngestJob.status` can hold two new values:** `waiting_for_ocr` (`#36`)
  and `needs_ocr` (`#29`, only with the gate on). A calling app that switches on the status should
  treat `waiting_for_ocr` as still in progress and `needs_ocr` as failed.
- **The embedder runs in NVFP4 on Blackwell cards.** `Nemotron-3.5-Embed-1B` ships in two builds of the
  same checkpoint: BF16 (16-bit, every CUDA GPU) and NVFP4 (4-bit, Blackwell cards with native FP4 —
  sm_100, sm_120, sm_121). `kb-server setup` reads the card's CUDA compute capability
  (`nvidia-smi --query-gpu=compute_cap`; the card's name is the fallback when the driver does not know
  the field) and writes the build it picked: NVFP4 on Desk and Studio, BF16 on Station and on every
  card without FP4 (RTX 30/40, Ampere, Ada, Hopper fall back automatically). The tier preset turns the
  new `KB_EMBED_PRECISION` into the model name the server asks for, so the server and the model server
  always name the same build; `--embed-precision nvfp4|bf16` overrides the choice.
- **The embedder's vLLM flags are sized from the card.** `kb-server setup` writes
  `KB_EMBED_MAX_MODEL_LEN`, `KB_EMBED_MAX_NUM_SEQS` and `KB_EMBED_GPU_FRACTION`, and compose reads
  them. The container no longer runs `--max-model-len 32768` with a fixed memory fraction: with
  the old flags the embedder took ~13.8 GiB on a GB10, with the sized flags ~3.7 GiB (issue #37).
- **`GET /v1/documents` is paged.** The response now carries `nextCursor` (an opaque token, `null` on
  the last page), `total` (how many documents match the filter, over every page), `limit` (the page
  size that was applied) and `limitClamped` (`true` when the requested limit was above the cap of 200
  and had to be clamped). Send the cursor back as `?cursor=` to continue the walk. The list is ordered
  by `(createdAt, id)`, so a document is never listed twice or skipped. The existing query parameters
  and the `documents` array are unchanged; the new fields are additive. A cursor this server did not
  issue is a `400`, not an empty page — an empty page is a valid answer that looks like the end of the
  list.
- **An upload of a file the tenant already has is refused (`KB_UPLOAD_DEDUPE`).** The upload's bytes
  are hashed (SHA-256) and compared with the tenant's live documents: the same bytes, in the same
  tenant, filed under the same knowledge bases, that is neither deleted nor failed, answer `409` with
  the usual `error` message plus `existingDocumentId`, the id of the document that already holds the
  file. The message names the toggle. `KB_UPLOAD_DEDUPE=off` stores the file again, as before. The
  hash is kept on the document row and is returned as `sha256` by the document list and detail
  routes. Another tenant's identical file neither refuses the upload nor has its id reported.
- **`KB_RERANK_MAX_K` sets how wide the reranked head is (issue #31).** The head is the set of
  candidates that come back with a `rerankScore` and are ordered by it; the candidates past it
  follow in the fused order, unscored. Before this, the head was always
  `KB_RERANK_FINAL_K` (5), so with the default answer of 8 chunks only 5 of them carried a score.
  The head is now `max(KB_RERANK_FINAL_K, KB_RERANK_MAX_K)` wide: `KB_RERANK_FINAL_K` keeps its
  old meaning ("the head is at least this wide"), so an operator who raised it keeps the head
  they had. `KB_RERANK_MAX_K=0` means no cap — the head is the whole pool. The default is **10**,
  which covers the default answer and stops where the measurement says the reranker's ordering
  stops beating the fused order (`docs/eval-ab/rerank-max-k/README.md`: nDCG@10 +0.0101
  [+0.0030, +0.0182] and Recall@10 +0.0246 [+0.0078, +0.0449] on the 20-candidate pool with
  `nvidia/nemotron-3.5-rerank-8b:free`; nothing gains past 10). A cross-encoder scores every
  document it is sent, so widening the head costs **no extra compute and no extra spend** — it
  only decides how many of the scores already produced are kept. The LLM reranker asks a chat
  model for a top-N list and returns ranks rather than relevance scores, so its head stays at
  `KB_RERANK_FINAL_K`.
- **`stats.rerankedCount` on the search response.** How many of the returned chunks carry a
  `rerankScore` — that is, how much of the answer the reranker scored. Additive: it is absent
  when no reranker ran, so a search without a reranker keeps exactly the earlier shape.

### Changed

- The OCR vLLM containers (`vllm-ocr-desk`, `vllm-ocr-studio`, `vllm-ocr-station`) take extra
  command-line flags from `KB_OCR_VLLM_EXTRA_ARGS`. On an NVIDIA GB10 (arm64, sm_121) the OCR
  model crashed inside a torch.compile/Triton kernel while it started, and `--enforce-eager` fixed
  it; the flag costs throughput, so it is off by default. The OCR model wants about 8 GiB of the
  card, and a job that needs OCR while the container is off now waits instead of failing
  (`rust/deploy/README.md`, GPU notes).
- `docs/kb-tiers.md` describes the stack the preset actually fills (one embedder for text and page
  images, `KB_EMBEDDERS=one` by default) instead of the 0.3–0.5 Qwen3-VL stack, and gives the VRAM
  budget per tier **per build**.
- The compose file's `vllm-embed` service no longer forces `--dtype bfloat16`: vLLM reads the build
  from the checkpoint's own metadata, and forcing a dtype breaks the 4-bit one.
- **The upload limit is 256 MiB, and a `413` says how to move it.** `KB_MAX_FILE_BYTES` still wins when
  it is set, but the default is now `268435456` bytes (256 MiB) instead of the Bun service's inherited
  50 MB (`src/lib/files/mime-types.ts`) — a limit that a 50.9 MB textbook (ViDoRe's) already did not
  fit. The `413` body names the limit in bytes, the limit in MiB and the variable that moves it, so a
  caller can act instead of guessing. The status code and the JSON shape are unchanged.
- **The documents list caps a page at 200.** A caller that asked for 500 used to receive 200 rows
  without being told; it is now told (`limitClamped: true`) and the page size that was applied is
  returned as `limit`. A limit above the cap is still clamped rather than refused, so existing callers
  keep working.
- The engine orders the head by the rerank score itself instead of trusting the provider's
  order (a stable sort, so equal scores keep the provider's order), and the candidates past the
  head keep the fused order.

### Fixed

- **A PDF whose text is drawn as outlines goes to OCR (`#28`).** Such a page has no text and no
  large raster image, so under the old raster-coverage rule no page qualified for OCR and the
  document failed with "the PDF has no readable text" even with a healthy OCR provider. A page with
  no visible characters is now an OCR candidate whatever its raster coverage.
- **A retry right after an OCR outage no longer fails with "no text for any page" (`#35`).** The
  outage breaker is process-wide, so a breaker opened by one document refused every page of the
  next one, and the skipped pages were reported as "no text". A breaker-skipped page and a
  transport error are now a distinct retryable error that names the cause, the job waits
  (`#36`) instead of failing, and pages touched by a transport error are never cached.
- The quality gate's own test counted 36 characters in a 34-character sentence, so the gate's test
  failed while the gate was right.
- The migration check accepted only a migration that creates a table; a migration that adds a
  column (`IngestJob.resumeAt`) was refused.

### Upgrading

- **An existing install is never switched to the 4-bit build.** The two builds are different
  checkpoints with different vectors, and Qdrant's collection name carries the model slug, so a BF16
  index and an NVFP4 index are different collections: switching is a **re-index**, not an upgrade.
  `kb-server setup` keeps the build the env file already names and says so. To switch:
  `kb-server setup <tier> --embed-precision nvfp4`, then `kb-server reindex`. The old collection is
  left alone (`docs/rust-cutover.md` §27).
- **Re-run `kb-server setup <tier>` after upgrading** to get the embedder's sized vLLM flags. Until
  it runs, the container keeps the old fixed flags (`--max-model-len 32768`, 15% of the card), which
  is too small for the embedder's weights on an 8 GB card and too large on a 48 GB one. Setup only
  writes the flags; it does not change the build.
- No re-index and no re-embed. The on-disk rerank cache keys on the ask width, so the first
  query after the upgrade re-runs the reranker for the wider ask and re-populates the cache; the
  provider spend is the same, it is paid once.

## 0.6.0 — 2026-11-18

### Changed

- **One embedder is the default on Desk, Studio and Station (`DEP-6`).** `rantai-kb setup desk`
  (and `studio`, `station`) now runs `Nemotron-3.5-Embed-1B` for text chunks *and* page images in one
  2048-d space, with `Qwen3-Reranker-0.6B`, and no flag. Measured: text 0.826 vs 0.796 (MIRACL-id),
  pages 0.621 vs 0.277 (ViDoRe HR), reranker +0.055 at 1.19 GB. Desk gains page-image search.
  `--embedders two` (`KB_EMBEDDERS=two`) keeps the 0.3–0.5 stack. Not yet measured (`DEP-7`): a page
  reranker on top, ingest throughput at 32k context, the fit on a real 8 GB card, a live Studio run.
- Compose's `kb-desk`, `kb-studio` and `kb-station` default to the `vllm-embed` and `llama-rerank`
  endpoints and wait for them to be healthy; `setup --embedders two` writes the TEI/vLLM endpoints
  of the old stack to `.env`.
- `Nemotron-3.5-Embed-1B` is 🟢: OpenMDW-1.1 was read in full and is permissive (keep the licence
  and notices; rights end for a party that sues over the weights), so it needs no `--accept-licence`.
- Lite and Cloud no longer get a `<tier>-two` compose profile; they have one embedding stack.

### Fixed

- **`KB_EMBEDDERS=one` ran the old models.** In 0.5.0 the tier plan logged Nemotron, but the config
  overlay read the two-embedder table, so the server embedded with `Qwen3-Embedding-0.6B` at 1024-d
  against the one-embedder endpoint. The overlay now resolves the same stack the plan reports.

### Upgrading

- **A GPU-tier deployment from 0.5.0 or earlier must choose.** Its `.env` has no `KB_EMBEDDERS`,
  which now means `one`; kb-server refuses to start while `COMPOSE_PROFILES` still names a
  `<tier>-two` profile and says how to choose: `rantai-kb setup <tier>` then `kb-server reindex`
  (text vectors go 1024 → 2048), or `rantai-kb setup <tier> --embedders two` to keep the old index
  (`docs/rust-cutover.md` §17). Lite and Cloud are unaffected.

## 0.5.0 — 2026-11-17

### Added

- **`rantai-kb setup --embedders one` wires the one-embedder stack into compose.** It writes
  `KB_EMBEDDERS=one`, the service-name endpoints (`vllm-embed:8000/v2/embed` for text *and* pages,
  `llama-rerank:8082/v1/rerank`, provider `cohere`) and the matching `COMPOSE_PROFILES`. The model
  services moved behind `<tier>-one` / `<tier>-two`, so exactly one stack starts — the two-model
  TEI + VL pair, or `Nemotron-3.5-Embed-1B` with `Qwen3-Reranker-0.6B`.
- **Licence entries** for `nvidia/Nemotron-3.5-Embed-1B-BF16` (`openmdw-1.1`, 🟡 opt-in) and
  `Qwen/Qwen3-Reranker-0.6B` (Apache-2.0), so the gate can price and refuse the new stack like any
  other.

### Changed

- `.env.example` documents `KB_EMBEDDERS` where operators look first, with the measured numbers and
  the warning that adopting it is a re-index.
- The README describes one embedder for text and page images on the GPU tiers, not a text encoder
  beside an image encoder.
- `kb-dev.sh` and `kb-server up|down|logs|ps` activate the same `<tier>-one` / `<tier>-two` profile,
  so the script and the binary still start the same containers.

### Fixed

- **The OpenAPI document's version is stamped at runtime**, not inside the `#[openapi(...)]` derive:
  Cargo does not rebuild a macro's output when only the package version changes, so a cached build
  kept emitting the previous version and the drift test failed after a release bump.

## 0.4.0 — 2026-11-17

### Changed

- **One embedder vs two on Studio and Station: measured, and the answer is two (`EVAL-4`).**
  `Qwen3-VL-Embedding-2B` loses **0.143** [−0.179, −0.108] nDCG@10 to `Qwen3-Embedding-0.6B` on
  MIRACL-id text chunks (0.653 vs 0.796), and a single text+page space scores **0.221** on
  ViDoRe HR against the 0.695 floor — worse than its own image-only arm (0.277), so the text
  vectors pollute the page arm rather than supplement it. The fusion weight stays, now with a
  measured reason. Two findings out of it: the VL model's text arm collapses on long input
  (0.208 on whole pages, 0.44–0.60 cut to 300–2,400 characters), and the local page embedder is
  far weaker than the cloud one (0.277 vs 0.575 image-only) — `EVAL-5`.

### Added

- **`KB_EMBEDDERS=one` — one embedder for text and pages on Desk, Studio and Station (`DEP-6`).**
  `Nemotron-3.5-Embed-1B` at 2048-d puts both modalities in one space and `Qwen3-Reranker-0.6B`
  replaces the VL rerankers. Measured: MIRACL-id text **0.826** vs 0.796, ViDoRe HR pages
  **0.621** one space vs 0.277 for the Qwen3-VL stack, reranker **+0.055**; 4.26 GB + 17.5 GB of
  reranker become 1.19 GB, and the tuned fusion weight disappears. Opt-in until the page-reranker
  run and one live tier run land — the default stack is unchanged. New wire shapes:
  `EmbedFormat::OmniEmbed` and `PageEmbedFormat::OmniEmbed` (vLLM `/v2/embed`).

- **The GPU tiers' reranker: `Qwen3-Reranker-0.6B`.** Measured on MIRACL-id (200 queries, dense
  top-50 pool): nDCG@10 **0.795 → 0.850** (+0.055) over no-rerank, at 1.19 GB and Apache-2.0.
  It replaces `bge-reranker-v2-m3` on Desk and the never-measured `Qwen3-VL-Reranker-2B/8B` on
  Studio/Station — 4.26 GB and 17.5 GB become 1.19 GB, one reranker for every GPU tier.
  `eval-ab/rerank-2026-11-17/`; two tools came out of it, `make_candidates.py` (rebuild a rerank
  pool from the engine's chunks) and `serve_cross_encoder.py` (serve an HF cross-encoder as
  `/v1/rerank`, for checkpoints whose GGUF this llama.cpp cannot load).

- **`EmbedFormat::OmniEmbed` in `kb-inference`** — the vLLM `/v2/embed` wire shape
  (`input_type` query/document, `inputs` as message content, `embeddings.float`), with a test that
  asserts the request and the ordering. Selected with `KB_EMBEDDING_FORMAT=openai|omniembed`
  (default `openai`; an unknown value is a startup error). Inert until a tier preset selects it —
  the first half of `DEP-6` (one embedder on the GPU tiers, `docs/core/specs/dep-6.md`).

- **`Nemotron-3.5-Embed-1B` measured: it reopens the one-embedder question (`EVAL-6`).**
  Same harness as `EVAL-4`: MIRACL-id text **0.826** (+0.030 [+0.012, +0.048] over
  `Qwen3-Embedding-0.6B`), and on ViDoRe HR text **0.611**, image **0.626**, one space
  **0.621**, image+text in one vector **0.625** — every pair inside ±0.015, so the space is
  coherent and mixing costs nothing. That is 0.008 under a cross-validated two-space fusion
  (0.629) with no fusion to tune, and it beats the cloud page arm on images (0.626 vs voyage's
  0.575). Corrects two earlier claims: `Qwen3-VL-Embedding-2B`'s long-text collapse is
  model-specific, not a property of VL embedders, and local page embedding is not weak —
  `Qwen3-VL-2B` is (`EVAL-5`). Adopting it needs a page-reranker run, throughput at 32k
  context, the `openmdw-1.1` opt-in and a 2048-d re-index.

- **The November bake-off runs are in the repo.** `docs/eval-ab/bakeoff-2026-11-13/` carries the
  text-embedder, text-reranker and page-embedder runs from the ThinkStation's registry harness,
  with their per-query output, the per-model protocol (query/document prefixes) and an honest
  account of what did not run: the page embedders' endpoints were down, so that run has no data,
  and `nemotron-embed-1b-v2` disagrees between two runs by 0.11 nDCG on MIRACL — unresolved, most
  likely a prefix bug. `docs/benchmarks.md` indexes them.

### Changed

- **The README is a front page; the detail moved into docs.** It now carries the hardware
  (minimum and recommended) and the model inventory for every tier — params, dimensions,
  precision, runtime and weights on disk — and links out. The API reference is
  `docs/api.md`, the retrieval pipeline `docs/retrieval.md`, the security model
  `docs/security.md` and the measured numbers `docs/performance.md`. The claim that a
  knowledge-base-limited key could still read by-ID has been removed: `SEC-1` shipped in
  v0.2.0.
- **`docs/benchmarks.md` indexes every benchmark we have run** — retrieval on ViDoRe and
  MIRACL, the KF V-plane comparison, the cloud tier, ingest and OCR, pictures, tables —
  with the result, the arm that was measured and the committed run folder, plus what was
  measured and *not* shipped and what has not been measured at all.

## 0.3.0 — 2026-11-16

### Added

- **OCR per tier, chosen by measurement (`ING-8`).** Lite reads scans with a new CPU document
  sidecar, `rust/deploy/ocr-cpu` (replaces `ocr-ppocr`): PP-Layout CDLA regions, PP-OCRv6 tiny
  text in XY-cut reading order (multi-column pages read column by column), SLANet-plus tables
  as HTML, figure boxes; page headers and footers dropped. Every model is Apache-2.0 and baked
  into the image, so it starts offline. Desk, Studio and Station read scans with
  Xiaomi-OCR-0 on vLLM 0.33 (`xiaomi` recipe, fp8, a fixed KV cache) instead of
  PaddleOCR-VL-1.6, with the PP-DocLayoutV3 table pass on the CPU as an option
  (`rust/deploy/ocr-layout` now builds with CPU torch and bakes its weights). Measured on
  olmOCR-bench (`docs/eval-ab/ocr-tiers`): Lite 0.69 pages/s on two cores, image-only pages
  52.5 (PP-OCRv6 alone: 31.6), tables 38.8 (was 0); Desk 1.35 pages/s at a 4.4 GB peak,
  text-layer PDFs 87.0, tables 77.9 (PaddleOCR-VL whole-page: 59.8 and 9.4). Lite presets
  render scans at 110 dpi (`KB_OCR_DPI`).
- **Model roles you can move (`DEP-5`).** `kb-server setup` takes `--ocr local|mistral|off|<url>`
  (e.g. Mistral OCR on a Lite box, or a shared Xiaomi server for several Lite boxes),
  `--ocr-tables accurate`, `--vision local|off|<url>` and `--set KEY=VALUE`. Compose runs each
  local model in a role profile (`<tier>-ocr`, `<tier>-vision`, `<tier>-ocr-layout`) that
  setup writes to `.env` as `COMPOSE_PROFILES`, so a role that runs elsewhere starts no
  container. Mistral OCR on a local tier is a 🟡 licence opt-in (pages leave the machine).
- **The HTTP API has an OpenAPI 3.1 document and a Swagger UI.**
  `GET /openapi.json` serves the document and `GET /docs` renders it in Swagger UI,
  both **without an API key** — like `/health`, the contract has to be readable
  before the caller has one. The `/v1` routes keep exactly the authentication they
  always had (`Authorization: Bearer <key>` or `X-API-Key: <key>`), and the document
  states the scheme and the scope each operation needs.
  - **Derived from the handlers:** every route carries a `#[utoipa::path]` attribute
    and the router is built from one route table, so the document and the router
    cannot drift: `tests/openapi_tests.rs` checks the table against the published
    document in both directions — a route missing from the document, or an operation
    that is not a route, fails the build.
  - **Every route is described:** method, path, path/query parameters, the JSON and
    multipart request bodies, and the response schemas with their status codes
    (including the `{"error": "..."}` body of every failure). The wire names are the
    real ones — camelCase included — because the schemas come from the same serde
    attributes the responses are built from.
  - **Offline by construction:** the UI's assets are embedded in the binary
    (`utoipa-swagger-ui`'s `vendored` feature), so the service never reaches a CDN
    at runtime and the docs work on an air-gapped host.
  - **Also printable:** `kb-server openapi` writes the same document to stdout, and
    the generated document is committed as `docs/openapi.json`; a test fails when
    the committed file is out of date.
- **One command installs the whole stack.** The `kb-dev.sh` script is now also a
  command in the binary, so an installed machine needs neither a checkout nor a
  script: `kb-server up|down|logs|ps|status|key` is a Rust port of the script
  (`rust/crates/kb-server/src/stack.rs`) and drives the same compose project
  (`rantai-kb-dev`), the same compose file and the same env file. The binary is
  installed under both names — `kb-server` and `rantai-kb` — and the help and
  usage text use whichever name was typed.
  - **Install:** `install.sh` (POSIX sh) downloads the release tarball for the
    machine, checks it against the release's `SHA256SUMS`, unpacks it into
    `$RANTAI_KB_HOME` (default `~/.rantai-kb`) and links `rantai-kb` and
    `kb-server` into `$RANTAI_KB_BIN` (default `~/.local/bin`). A re-run is an
    upgrade, not a reset: `deploy/.env` and `deploy/.tier` survive it.
    `--uninstall` removes the links and the directory.
  - **Deploy directory:** `$RANTAI_KB_HOME/deploy`, else `rust/deploy` (or
    `deploy`) next to the working directory, else `~/.rantai-kb/deploy`. `setup`
    writes that directory's `.env` and `.tier`; the stack commands read them.
    An installed bundle has no source, so its env file points compose at the
    published images (`KB_IMAGE`, `KB_OCR_CPU_IMAGE`, `KB_OCR_LAYOUT_IMAGE`)
    and `up` pulls them instead of building.
  - **Profiles:** one rule in both places. An env file `COMPOSE_PROFILES` wins
    and no `--profile` flag is passed; otherwise the tier's own profiles are
    activated (`<tier>`, `<tier>-ocr`, `<tier>-vision`).
  - **`serve`:** unchanged for the container (`DATABASE_URL` set → the
    in-process server, which is what the image's CMD runs), and it starts the
    stack when a deploy directory is configured. `serve --foreground` always
    serves in this process.
  - **What `up` prints:** the API URL, the health URL, the key hint, the
    upload command and the stop command. The Swagger UI line is printed only
    when the running server really serves `/docs`, so the summary never points
    at a URL that answers "Unauthorized".
  - **Packaging:** `rust/scripts/package-release.sh <target> <version> <outdir>`
    builds the binary, fetches libpdfium for the target (macOS arm64 included)
    and writes the tarball the installer downloads. The release workflow
    publishes Linux x86_64, Linux aarch64 and macOS arm64 tarballs with their
    checksums, and the OCR sidecar images beside the main one.

- **Tables you can ask questions of (`TBL-1`, work package B: store, query, ingest, API).**
  A document's spreadsheets, CSVs, DOCX tables and OCR/markdown tables are now stored
  as typed grids and answered by a query instead of being summed by a language model
  from flattened text.
  - **Storage:** each grid is Parquet in the blob store at
    `tables/{tenant}/{document}/{tableId}/v{n}.parquet` — one typed column per header
    plus `_row` and `_derived` — and its record is a row in the new **additive**
    Postgres table `KbTable` (`prisma/migrations/20261114120000_kb_tables`, primary
    key `(id, version)`, `ON DELETE CASCADE` from `Document`). A re-ingest writes a
    new version for the document and keeps the previous one; older versions and their
    Parquet objects are deleted. `id` is `{documentId}_t{index}`.
  - **Query:** the new adapter crate `kb-store-tables` implements
    `kb_ports::TableStore` with DuckDB over the one cached Parquet file. A typed
    `TableQuery` is validated against the manifest (unknown columns list the available
    ones; SUM/AVG demand a `Quantity` column; an ungrouped plain column, a privacy
    violation and a bad value each get their `QueryError`), compiled with identifiers
    quoted from the manifest whitelist and **values bound as parameters**, and run
    with `QUERY_TIMEOUT_MS` (interrupt handle) and `MAX_ROWS_RETURNED`. The result
    carries the receipt (`rows_matched`, `excluded_derived_rows`, `suppressed_groups`,
    `truncated`, `warnings`), and `min_group` suppression hides small groups with
    complementary suppression when exactly one would be hidden.
  - **Ingest:** `kb_core::tables::build_manifest` → `put_tables`, then two retrieval
    units per table: `chunk_type=table_summary` and `chunk_type=table_row` (tables of
    at most 60 rows), both carrying `tableId` and the normal tenant/document payload.
    A document with no tables is unchanged, and the whole table stage is off when
    `KB_TABLES=false` or no table store is configured.
  - **API (additive):** `GET /v1/tables?documentId=`, `GET /v1/tables/{id}` and
    `POST /v1/tables/{id}/query` with the document routes' scopes and tenant rules
    (400 carries the `QueryError` JSON; another tenant's table is a 404). Search hits
    of a table chunk type carry `tableId` and `tableTitle`, and a search answer lists
    `tables` for the documents it hit. A hard delete removes the tables and their
    Parquet. The upload accepts `tablePrivacy: "aggregate-only"`, stored in the
    document's metadata, which becomes a `QueryPolicy` at query time.
  - **Isolation:** the leak matrix (`kb-server eval leak`) gains a `table` row that
    lists, gets and counts one document's tables, seeded from a CSV document per
    scope.
- **Tables are read as data (`TBL-1`, work package A).** Spreadsheets (`.xlsx`, `.xls`,
  `.ods`), CSV/TSV, DOCX tables and markdown pipe tables from OCR now become structured
  grids (`ExtractionResult.tables`) with a manifest: column kind and role (a phone number
  and a year are not quantities), the unit from the header, sample values, and the derived
  subtotal rows — proven by spreadsheet formulas where available, guessed from a
  `Jumlah`/`Total` label otherwise. Spreadsheet regions are split by blank rows and columns,
  merged cells are spread, multi-level headers are flattened, hidden sheets are skipped with
  a note, and locale numbers (`1.234,5`, `(1.234)`, `Rp`, `%`) are read correctly. Markdown
  tables that continue on the next page are stitched, and a letterhead repeated on half the
  pages is dropped. Spreadsheets and DOCX tables now render as markdown tables, and a big
  table is chunked at row boundaries with the header repeated in every piece instead of
  becoming one unbounded chunk.

- **Ingest runs several documents at once, through per-stage seats (`ING-6`, part A).**
  The worker used to claim a job and run it to completion before claiming the next
  one, so a 300-page scan held every small file behind it. Now up to `KB_JOB_WORKERS`
  documents (default 6, Lite and Lite-en 2) are in flight, each in its own task with
  its own heartbeat, progress events and terminal events, and each stage of the
  pipeline has a small pool of **seats** (`KB_SEATS="extract=2,embed=4,store=2"`) that
  a document holds only while it is in that stage. When a seat frees, the document
  uploaded first gets it — not the one that happened to ask first — so a one-page memo
  is ready while a big scan is still running. A seat is given back on every exit path,
  including a panic and a cancelled wait. `KB_INGEST_CONCURRENCY` stays accepted as an
  alias for `KB_JOB_WORKERS` and loses to it when both are set.
- **PDF page work can be offloaded to an OCR service (`ING-6`, Part B).**
  `KB_OCR_MODE=off|hybrid|full` routes each PDF page on its own: `hybrid` sends a
  page only when raster images cover ≥ `KB_OCR_SCAN_AREA` (0.90) of it and it
  carries ≤ `KB_OCR_SCAN_TEXT` (50) visible characters — invisible text from an
  existing OCR layer does not count — while `full` sends every page and keeps the
  local text as that page's fallback. Unset means `hybrid` when an OCR provider is
  configured and `off` otherwise, so an installation with no provider behaves as
  before.
  - **Providers:** the local chat adapter reads one rendered page image per
    request; Mistral OCR reads sub-PDFs of `KB_OCR_BATCH` (8) pages built by
    pdfium, and its figures are kept on their document page.
  - **Cache:** one entry per page in kb-cache namespace `ocr-page-v1`, keyed by
    provider, model, prompt, DPI and the page (document SHA-256 + page index for
    a document provider), so a re-upload pays only for pages not yet read.
  - **Limits:** `KB_OCR_CONCURRENCY` is one process-wide pool; a breaker stops
    sending for `KB_OCR_BREAKER_COOLDOWN_S` (60) after `KB_OCR_BREAKER` (3)
    consecutive transient failures; at most `KB_OCR_MAX_PAGES` (300) pages per
    document go to OCR and the rest are listed as skipped.
  - **Outcomes:** a lost page becomes `[page N could not be read]` and is named
    in `metadata.ocr.failedPages`; the document is still `ready`. A document
    where every page failed fails the job instead of indexing as empty.
    `metadata.ocr` records mode, provider, pages, OCR/cached/local/skipped pages,
    failed pages and whether the breaker opened.

- **Xiaomi-OCR-0 is an OCR recipe (`ING-6`).** The local chat endpoint can now be served with
  the vendor's own Xiaomi-OCR-0 recipe instead of PaddleOCR-VL's task prefix.
  - **Recipe:** `KB_OCR_RECIPE=paddle|xiaomi` (default `paddle`; an unknown value is a
    configuration error, like `KB_OCR_MODE`). The Xiaomi recipe renders the page as a PNG at
    150 dpi scaled to 2048 px, sends the **document prompt before the image** at temperature 0
    with thinking off, retries a repetitive answer with `presence_penalty 1.5` (keeping the
    second answer only if it is not repetitive either), and then cuts a truncated repeated tail,
    normalises the math delimiters and renders the tables.
  - **Parity:** the postprocessing is ported to `kb-core` (`kb_core::xiaomi_ocr`) and is
    byte-for-byte the vendor's — over the 1,403 pages of the olmOCR benchmark, 1,403/1,403
    pages come out byte-identical to the vendor pipeline's own output (71/71 for the region
    path), with the vendor's raw answers as the fixture.
  - **Tables:** `KB_OCR_TABLE_FORMAT=markdown|html` (default `markdown`) — a pipe table or the
    vendor's `<table>` with row and column spans.
  - **Layout fallback:** with `KB_OCR_LAYOUT_URL` set, the provider asks a layout detector where
    the page's regions are and re-reads the page region by region **only when the detector says
    the page has a table and the whole-page answer has none** — the reference pipeline's rule,
    which fires on 71 of the 1,403 benchmark pages. The detector is the sidecar in
    `rust/deploy/ocr-layout` (PP-DocLayoutV3, `POST /v1/layout`, `GET /health`); the rule, the
    region prompts, the crops and the merge are Rust. A detector that is down keeps the
    whole-page answer, reports the page in `metadata.ocr.layoutFailed` and does not cache it.
    `metadata.ocr` gains `layoutPages` and `layoutFailed`.
  - **Caching:** the recipe, the table format and the layout path are part of the OCR page
    cache key and of the extraction fingerprint.
  - **Measurement:** `kb-server eval ocr --pdfs <list> --root <dir> --out <dir>` reads the first
    page of every listed PDF through the same router the pipeline uses and writes
    `<out>/<name minus .pdf>.md`, with the wall time and pages/s.
  - **Defaults for the Xiaomi recipe:** `KB_OCR_TIMEOUT_MS` 600 s and `KB_OCR_MAX_TOKENS` 12288
    when unset (the vendor's values; the shared defaults of 120 s and 4096 cut dense pages off).
    `KB_OCR_LAYOUT_TIMEOUT_MS` (default 60 s) for a detector on CPU.
  - **`eval ocr` scores only what the model read:** a page that fell back to the text layer
    (provider failed, breaker open) is counted as failed, not as read.
  - **Measured (2026-11-16, NVIDIA GB10, Xiaomi-OCR-0 under vLLM):** all 1,403 pages of
    olmOCR-bench, official scorer, 1,403/1,403 read by the model. Whole page: text-layer PDFs
    **87.0** (vendor pipeline 87.0), tables **77.9** (76.1), scans **73.7** (73.3), image-only
    PDFs **77.4** (77.7), at **1.89 pages/s**. With the layout fallback: **89.0** (88.9), tables
    **87.9** (87.6), scans **73.1** (73.3), image-only **78.2** (78.5). Mistral OCR on the same
    tracks: 77.4, 85.5, 70.8, 75.3. Run the layout detector off the vLLM GPU: sharing it
    crashed or hung the engine within minutes (`docs/kb-tiers.md` §OCR recipes).
  - **Fixed:** reading a page's text facts loaded the page text once per text object (pdfium
    holds a process-wide lock), 29 s on a dense page; it now loads it once (0.3 s).

- **Pictures are searchable by what they show (`ING-5`).** Before, an image was
  found only by the text OCR read off it: a photo or diagram without text became a
  `ready` document with zero chunks, pictures inside Office and EPUB files were
  dropped (alt text included), and PDF figure crops were indexed by caption and page
  text only. Now one visual stage handles every picture a reader can see:
  - **Sources:** standalone uploads (each TIFF page), pictures in `.docx`, `.pptx`,
    `.xlsx`, `.odt`, `.ods` and `.epub` (with their alt text), PDF figure crops from
    the layout chain, and (opt-in, `KB_IMAGE_PDF=true`) image objects in text-layer
    PDFs.
  - **Filtering:** pictures are normalised (longest side ≤ 1568 px, JPEG) and
    filtered: icons, strips, duplicates and a logo repeated on every slide are
    counted and skipped, with a per-document cap (`KB_IMAGE_MAX_PER_DOCUMENT`, 100).
  - **Reading:** each picture is read by an image describer
    (`KB_IMAGE_DESCRIBE_MODEL`, any OpenAI-compatible chat endpoint) that returns
    its kind, a description and its visible text. Readings are cached on disk by
    model, prompt version and image hash, and rate limits are retried with backoff.
  - **Indexing:** each picture becomes a `figure` chunk with its `assetKey`, so the
    dense, BM25 and figure arms find it. Pictures the describer calls a logo or
    decoration are counted, not indexed.
  - **Outcomes:** `Document.metadata.visuals` records what happened to every
    picture; `metadata.figures` entries gain `origin` and `description`.
  - **Tiers:** a Qwen3-VL-2B-Instruct sidecar on llama.cpp reads pictures on the
    local tiers (CPU on Lite, GPU from Desk); Cloud uses
    `nvidia/nemotron-3.5-super-vl` (about $0.22 per 1,000 images at the listed
    price). `kb-server setup --tier cloud` now also asks for
    `--accept-licence nvidia/nemotron-3.5-super-vl`.
  - **Leak gate:** `kb-server eval leak --docx` seeds pictures whose alt text is
    the figure canary, so the figure arm and the asset route are probed; the CI
    gate uses it.

- **Legacy Office files are extracted (`ING-1`):** `.doc`, `.xls` and `.ppt`
  were accepted at upload but always failed extraction. They are now read by
  pure-Rust parsers, with no LibreOffice or other runtime dependency:
  - `.doc`: Word 97–2003 through the piece table (fast-saved files included;
    field instructions dropped, field results kept; footnotes, endnotes and
    text boxes read; headers and comments skipped, as for `.docx`). Word 6.0
    and Word 95 files are read too, with the 8-bit codepage detected from the
    text (Cyrillic, Central European and other Windows codepages).
  - `.ppt`: PowerPoint 97–2003 through the persist directory, so slides come in
    presentation order, then speaker notes. PowerPoint 95 files are read from
    their 97 dual-storage copy or their `Text_Content` stream.
  - `.xls`: Excel 5.0–2003 (BIFF5/BIFF8) through calamine.
  - A legacy file is routed by its bytes, not its name: RTF, HTML, XML
    (SpreadsheetML), UTF-16 or UTF-8 text, and OOXML packages saved under a
    `.doc`/`.xls`/`.ppt` name are read by the matching parser.
  - Password-protected, Word 2.0, PowerPoint 4.0 and damaged files fail with a
    reason that says which, never with garbage text.
- **TIFF scans (`ING-4`):** `.tif` and `.tiff` are accepted (`GET /v1/formats`
  lists them; `image/tiff` is an accepted mime). Every page of a multi-page
  TIFF is decoded (uncompressed, PackBits, LZW, Deflate, JPEG, CCITT Group 3
  1-D and Group 4) and sent to the configured vision OCR as PNG, one page per
  request, and each page keeps its page number in the page map. Like other
  images, a TIFF needs a vision OCR provider. Palette (indexed-colour) and
  two-dimensional Group 3 pages are refused with a reason.

### Changed

- **The ingest worker is concurrent by default (`ING-6`).** With no
  `KB_JOB_WORKERS` and no tier, a deployment now runs 6 documents at a time instead
  of one at a time. Set `KB_JOB_WORKERS=1` for the old serial behaviour; the Lite and
  Lite-en tiers run 2 documents at a time.

- **An image upload with nothing searchable fails visibly (`ING-5`).** When OCR
  finds no text and no picture reading is possible (no describer configured, or
  the describer failed), the job fails with `nothing searchable in "<name>": …`,
  naming the setting to change, instead of finishing `ready` with zero chunks. An
  image upload without an OCR provider no longer fails before the describer can
  read it. The page arm, when on, still gives such an image a visual embedding,
  so it is not failed there.
- **Hard delete removes picture assets.** `DELETE /v1/documents/:id?hard=true`
  now also deletes the assets listed in `metadata.figures`, limited to the
  document's own asset prefix.

- **An unknown or missing file extension is refused by the extractor
  (`ING-4`).** The upload route already answered 415 for one; a job that
  reached the extractor another way (a job row written by a calling app, a
  `kb-server reindex` of a document stored before the upload check) used to
  have its bytes indexed as UTF-8 text. It now fails with `"<name>" has an
  unsupported file type (.ext); see GET /v1/formats for the accepted list`.
  No HTTP shape changes.

- **Tree arm (`DEC-4`, `RET-6`), off by default.** A structural question
  ("what does section 4.2 say about…", "chapter 3 of the joint employment
  report") can be answered by navigating a document's section hierarchy instead
  of matching text. Opt in with `KB_TREE_ARM=1`; the rest of the switch is
  `KB_TREE_ARM_WEIGHT` (RRF weight, `0.5`) and `KB_TREE_ARM_NODES` (heading
  nodes per query, `5`).
  - **Ingest** reads each PDF's bookmark tree through pdfium, and falls back to
    numbered headings in the page text (`3.4 Title`, `Chapter 3 …`). The result
    is stored additively in `Document.metadata.outline` (JSONB; no schema
    migration): `method` is `bookmarks`, `headings` or `none`, and each node
    carries `title`, `level`, `start_page` (zero-based) and `end_page`.
    Bookmarks and page text are already parsed, so there is no model call and
    no second parse. Existing documents get an outline on their next ingest or
    `kb-server reindex`. A cached extraction from before this release, or one
    from an OCR or layout parser, carries no outline; ingest then calls the new
    `Extractor::outline` port method (a free bookmark/heading read) rather than
    invalidating the extraction cache, so no paid OCR is repeated. The outline lives in
    the document row's `metadata`, which `GET /v1/documents/:id` does not
    return, so no response grows.
  - **Search** runs the arm only when `KB_TREE_ARM` is on **and**
    `kb_core::classify_query` labels the question `structural`. It reads
    outlines only for documents in the caller's `DocumentScope`, scores nodes
    with explicit section/chapter numbers plus BM25 over node titles, reads the
    chunks on the winning nodes' pages through a scoped page read, and fuses
    them into the candidate list by weighted RRF before rerank. Contributed
    chunks carry `tree` in `retrieval.arms`; `stats.treeArm` reports
    `{nodes, pages}`. A failed outline or page read adds the `tree_arm_failed`
    degrade and the other arms answer unchanged.

- **Global community arm (`DEC-5`), off by default.** `KB_GLOBAL_ARM=1` adds a
  retrieval arm that ranks the tenant's graph community summaries by BM25 and
  fuses their member chunks before the rerank (RRF weight
  `KB_GLOBAL_ARM_WEIGHT`, default 0.5), only for queries
  `kb_core::classify_query` labels `aggregative`. A hit the arm contributed
  carries `retrieval.arms: ["global"]`, and `stats.globalArm`
  (`{communities, chunks}`) is present only when the arm ran. **`EVAL-3`
  measured a loss for this design even on the broad questions it targets**
  (aggregative recall@10 −0.067 on HR and −0.103 on CS with an oracle gate), so
  the flag is off and an unconfigured server is byte-identical to before: no
  classification, no community read, no new `stats` field. Communities are
  built by the operator command
  `kb-server communities build --tenant <id> [--min-pages 2] [--max-usd X] [--dry-run]`,
  which prints a `chars/4` cost estimate before any call, refuses to start above
  `--max-usd` (0 by default), turns provider reasoning off, caches every summary
  under `KB_CACHE_DIR` keyed by model + `sha256(prompt)`, and never caches an
  empty answer. It needs the entity graph (`KB_ENTITY_EXTRACTION_ENABLED`,
  `KB_ENTITY_EXTRACTOR=heuristic|llm`, `KB_GRAPH_STORE=pg`); without it the
  command says so and writes nothing. A new additive table `KbCommunity` stores
  the summaries; the summary text is **never** returned to a caller, because it
  may describe documents the caller cannot see. A community read is scoped to
  the caller's `DocumentScope`, a store failure degrades as
  `global_arm_failed` rather than emptying the answer, and a tenant with no
  communities reports `stats.globalArm.communities: 0` without a degrade.
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

### Fixed

- **Settings in `rust/deploy/.env` now reach kb-server.** The compose services listed their
  variables one by one, so `KB_MISTRAL_OCR_KEY` (and any other `KB_*` not in the list) never
  reached the container: Mistral OCR could not be turned on through compose. kb-server now
  loads `.env` (`env_file`); a present-but-empty value in `.env` means "off".

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
