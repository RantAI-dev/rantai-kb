# v1.0 roadmap: v0.9.0 → v0.10.0 → v1.0.0

Planned 2026-11-24 from the two-architect review (Gemini 4 Neon and Claude Opus 5.5, swarm
board `#1`–`#10`) and the owner's cuts. The planner and reviewer is Claude. The coders are
OpenCode runners (`opencode/hy4-flash-free`), one task each. Each task is one branch and one
PR. The planner re-runs every gate before merging.

## Goal

- **The literal ask:** ship the roadmap the review agreed on.
- **The goal behind it:** a calling app (a RantAI product or an agent) gets **the right
  passage of the right version of the right document, cited correctly**. It can read further
  in that document without re-searching, and it never sees another tenant's or user's data,
  at 100k documents per tenant.
- **v1.0 means** the HTTP contract and the Postgres schema are frozen: from then on, changes
  are additive only.

## Release train

| Release | Contents | Gate |
|---|---|---|
| **v0.9.0** | Bug fixes (`RET-10`, `RET-11`, `ING-9`), reranker breadcrumbs A/B (`RET-12`), progressive disclosure v1 (`ING-13`, `RET-13`, `RET-14`, `RET-15`, `ING-14`, `EVAL-7`), eval debt (`TBL-1` re-score, `DEP-7`) | All P0 rows done or measured, with their numbers in the release notes |
| **v0.10.0** | Enterprise precision: versioning + `asOf` + `multiVersionWarning` (`RET-16`), `maxPerDocument` (`RET-17`), calibrated confidence (`RET-18`), passage breadcrumbs if `RET-12` won (`RET-19`), freeze of the graph and global arms (`DEC-6`) | Each row's target measured |
| **v1.0.0** | Scale and agents: predicate scope with a fail-closed Postgres verify (`SEC-2`), thin `/mcp` (`API-4`), stateless parity tests (`API-5`), `aclPrincipals` (`SEC-3`), Qdrant quantization (`RET-20`), API freeze (`REL-7`) | Leak matrix 0 with stale payloads injected; 100k-document scope p95 ≤ 1.2× the 1k scope |

**Owner's cuts, not built:**
- MMR;
- the page-diff engine (the content-hash cache already skips unchanged pages);
- LLM contextual prefixes;
- conflict detection inside the server;
- server-side query rewriting;
- HippoRAG, community summaries and GraphRAG (`DEC-6`);
- ColQwen2 + MUVERA.

**Parked, with the rule for reopening it:**
- **ColQwen2 + MUVERA:** reopen only when the same search p95 (≤ +10%) gives page nDCG@10
  above the shipped 0.705 on ViDoRe v3 HR, with a 95% CI above 0. Our single-vector page arm
  (Nemotron, 0.626 image-only) already beats ColQwen2 (0.547).
- **Near-duplicate collapse:** reopen only if `EVAL-8` shows near-identical chunks in top-10 for
  ≥ 10% of its questions.

## Rules for every task (runner brief preamble)

1. **Branch and checkout:** work in the worktree you were given, on branch `<task-branch>` cut
   from `main`.
2. **Build:**
   - Use your own target dir, `export CARGO_TARGET_DIR=/home/shiro/kb-target-<task-id>`, and
     delete it when you finish. Never share one between branches.
   - Build with `-j2` (`export CARGO_BUILD_JOBS=2`). The box has 6 cores and other agents.
   - libpdfium is at `/home/shiro/rantai-kb/rust/.vendor/pdfium`: set
     `PDFIUM_DYNAMIC_LIB_PATH=/home/shiro/rantai-kb/rust/.vendor/pdfium/lib/libpdfium.so` (the file, not the directory) before
     `cargo test`.
3. **Gates, run before each commit that touches Rust:**
   - `cargo fmt --all --manifest-path rust/Cargo.toml --check`
   - `cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings`
   - `cargo test --manifest-path rust/Cargo.toml --workspace`
   - `cargo run --manifest-path rust/Cargo.toml -p kb-server -- check-boundary`
4. **Evidence:** paste the raw tail of each gate in your final report. If you did not run
   something, write "not run". Never invent a number.
5. **Commits:** commit early, with conventional messages (`fix(retrieval): …`). Do not push and
   do not open a PR; the reviewer does that.
6. **Boundaries:**
   - `kb-core` does no I/O.
   - `kb-engine` names no adapter crate.
   - `/v1/*` changes are additive. If a shape must change, write it in `CHANGELOG.md` under
     `## Unreleased`.
   - Every retrieval arm stays filtered to the caller's scope. A new arm or route gets a leak
     test.
7. **Docs in the same PR:**
   - update this task's row in `docs/core/BACKLOG.md` (Status → Done or In Acceptance);
   - add a `CHANGELOG.md` `## Unreleased` entry;
   - update `docs/api.md` and `docs/openapi.json` (`kb-server openapi > docs/openapi.json`) when
     the API changes.
8. **If stuck:** stop at the best state you can measure. Commit it as `wip(...)` and say in
   your report exactly what is missing. A partial, honest result beats a complete, invented one.

---

## v0.9.0 — P0

### RET-10 · Citations keep documents apart (bug)

- **Why:** `source_dedupe_key` (`rust/crates/kb-core/src/context.rs:42`) keys a text source by
  `title-section`. Two documents with the same title (a 2023 and a 2024 "Kebijakan Cuti") then
  merge into one `[N]` that carries the wrong `documentId` and page.
- **Change:**
  - The key becomes `doc:{document_id}|{section}` when `document_id` is present. Without a
    `document_id`, keep the old key.
  - Figures stay keyed by asset.
  - `numbered_source_list` and `build_context` (same file) must use the new key consistently.
  - Check every caller (`grep -rn source_dedupe_key rust/crates`) and the source DTO in
    `kb-server/src/dto.rs`.
  - If a source card collapses several chunks of one document and section, it must carry the
    page of its first chunk as today. Add an **additive** `pages: [u32]` (sorted, unique) to the
    source DTO only if the struct already has room; otherwise leave pages alone.
- **Tests (in `context.rs`):**
  - two documents with the same title and section → two sources, with distinct numbers and
    their own `documentId`s;
  - the same document and section from two chunks → one source;
  - the existing tests keep passing. Update the one asserting `"Apple 10-K-Income Statement"`
    only if it passes a `document_id`.
- **Done when:** the gates are green and the new tests fail on `main` and pass on the branch.
  Prove the failing half by running the new test against the old function body.

### RET-11 · `documentIds` searches use the full pipeline (bug)

- **Why:** `search` in `rust/crates/kb-server/src/routes.rs` has a "Branch 1"
  (`if let Some(doc_ids) = body.document_ids…`, about line 599). It runs **dense only**: no
  BM25, no reranker, no page/tree/neighbour arms and no figures. Searching inside chosen
  documents is therefore worse than searching everything, and the BM25 × `documentIds` leak row
  was never tested.
- **Change:**
  - Delete Branch 1's private pipeline.
  - Keep its scope logic, which is the security-relevant part:
    1. `find_alive_meta_by_ids`, then an early empty answer when none are alive.
    2. Build the filtered scope with `build_scope(filter)`, then
       `filtered_scope.narrowed_to(&requested)`.
    3. Pass that narrowed scope into the same `state.engine.retrieve_inner(…)` call that
       Branch 3 uses, with the same `max_chunks`, `min_score`, `boost` and `adaptive_arms`.
  - Factor Branch 3's tail into a helper so both paths share it.
  - The `hybrid: true` stub (Branch 2) is unchanged.
- **Compatibility:**
  - The response still has `chunks`; it now also has `sources` and `context`, as Branch 3
    does.
  - Before, Branch 1 built `context` only when `format == "context"`, as `"[i] content"`. Now
    `context` follows Branch 3's format. Note this under `### Changed` in `CHANGELOG.md` and in
    `docs/rust-cutover.md` (new numbered section, "documentIds searches use the full pipeline").
- **Tests (`rust/crates/kb-server/tests/`; look at `leak_tests.rs` and `harness/mod.rs`):**
  1. `documentIds=[A]`, where document B holds the exact keyword and A does not → B never
     appears (this is the BM25 arm leak test).
  2. `documentIds` naming another tenant's document → empty.
  3. `documentIds` plus a KB-limited key that excludes the document → empty.
  4. `documentIds=[A]` with a keyword-only match inside A → it is found (BM25 now runs).
- **Also:**
  - in `rust/crates/kb-eval` (`leak.rs` / `leak_http.rs`), flip the `bm25 × document_ids` row
    from UNTESTED to a real probe if the matrix has such a row (`grep -rn UNTESTED
    rust/crates/kb-eval`);
  - update the `docs/api.md` text for `documentIds`.

### ING-9 · Passage prefixes on the batch path (bug)

- **Why:** `EmbedderImpl::embed_one` prefixes text, but `embed_many` → `embed_openai_batch`
  (`rust/crates/kb-inference/src/embedder.rs`, about lines 398–430) sends the batch
  unprefixed. So e5-small (`lite`) and any model with a `passage_prefix` is indexed without
  `passage: ` on ingest. Stateless `/v1/process` → `embed_passages` (about line 560) is
  affected too.
- **Change:**
  - Apply `passage_prefix` to every text in `embed_many` exactly once, and never twice.
  - Make sure `kb_cache` keys still match what is sent; its `with_prefixes` keys the cache.
  - Providers whose input type goes out-of-band (`input_type`, Nemotron / OmniEmbed) must
    keep **no** text prefix. Read how `embed_one` decides, and mirror it.
- **Tests:**
  - a fake HTTP embedder records the request bodies. Single and batch calls must both carry
    `passage: x` when a prefix is set, and neither when the provider uses `input_type`;
  - `/v1/encode mode=passage` and `/v1/process` embeddings are equal for the same text
    (`kb-engine/src/processor_tests.rs`).
- **Ops note:** an index built with e5-small on `lite` before this fix is missing prefixes. Add
  a note to `docs/rust-cutover.md` and `CHANGELOG.md`: re-index `lite` corpora
  (`kb-server reindex`) to get the fix. Measuring the gain is the reviewer's job, on
  MIRACL-id-50, before and after.
- **Also:** a different item was wrongly given the same id: the second `ING-9` row in
  `BACKLOG.md`, about blank scanned pages. It becomes `ING-12`.

### RET-12 · Reranker sees the breadcrumb (flag + A/B)

- **Why:** the reranker receives the bare `h.chunk.content` (`rust/crates/kb-engine/src/retrieval.rs`,
  `RerankCandidate { text: … }` about line 1405). A chunk saying "berlaku 12 hari" loses the
  "Bab 4 > Cuti Tahunan" and document-title context that decides relevance. This change needs
  **no re-index**.
- **Change:**
  - New setting `KB_RERANK_BREADCRUMBS` (`off` | `on`; default `off` until the A/B result).
    Thread it through `EngineSettings` the way `rerank_char_budget` is threaded.
  - When on, the rerank text becomes
    `"{document_title} › {section path or section}" + (page ? " · p.{page}" : "") + "\n" +
    content`.
  - The char budget applies to the content part only.
  - The response content is unchanged; only what the reranker sees changes.
  - Use only fields the chunk already has (`document_title`, `metadata.section`,
    `metadata.heading_path` if present, `metadata.page`).
- **Tests:**
  - with the flag off, the rerank input is byte-identical to today;
  - with it on, the input has the expected prefix, and the char budget still caps the content.
- **A/B (reviewer, after merge):** MIRACL-id-200 and ViDoRe HR, bge-reranker-v2-m3 on the LAN
  GPU, flag on vs off, nDCG@10 with a paired bootstrap 95% CI.
  - The default becomes `on` only if Δ ≥ +0.01 with a CI above 0 on at least one set, and
    there is no significant loss on the other.
  - The result goes to `docs/eval-ab/rerank-breadcrumbs/README.md`.

### ING-13 · Chunks know where they sit (spans + page index)

- **Why:** progressive disclosure has to stitch a document back together from its chunks
  without duplicating overlap. Today the chunk metadata has no character offsets, and the
  overlap text is mixed into `text`.
- **Change in `rust/crates/kb-core/src/chunk/smart.rs`:**
  - `SmartChunkMetadata` gets `char_start: u32`, `char_end: u32` and
    `overlap_prefix_len: u32`. These are character offsets into the markdown the chunker
    received. `overlap_prefix_len` is how many leading chars of `text` repeat the previous
    chunk (set where `get_overlap` is used, about line 253).
  - Invariant (property test): for every document,
    `concat(chunk.text[overlap_prefix_len..] for chunks in order) == the source text after the
    chunker's own normalisation`. If the chunker rewrites text (table header repetition,
    trimming), define the invariant over what it emits. Document the exceptions in a comment
    and in the test.
  - Write `charStart`, `charEnd` and `overlapPrefixLen` into the stored chunk metadata in
    `kb-engine/src/ingest.rs` (wherever `SmartChunk.metadata` becomes `ChunkToStore.metadata`).
- **Change in Qdrant (`rust/crates/kb-store-qdrant`):**
  - store `charStart` / `overlapPrefixLen` in the payload;
  - add **integer** payload indexes for `page` and `chunkIndex` next to the existing keyword
    indexes in `settings.rs` (about line 212–230). Index creation must be idempotent on an
    existing collection.
- **Old chunks have none of these fields.** Every reader must treat them as missing, never as
  0.
- **Tests:**
  - the property test above (proptest is fine if already a dependency; otherwise use a table
    of 10 handcrafted markdown docs with headings, tables, code and long paragraphs);
  - the payload round-trip in `kb-store-qdrant` (`payload.rs` tests).

### RET-13 · Outline v2 on `GET /v1/documents/:id/structure`

- **Why:** an agent needs a map of the document before reading it. Read the current handler
  (`routes.rs`, `"/v1/documents/:id/structure"`) and the stored outline
  (`Document.metadata.outline`, written by the tree-arm work `RET-6`) first.
- **Additive response fields:**

  ```json
  {
    "outlineVersion": 2,
    "sections": [
      {"id": "s3", "title": "Bab IV Cuti", "level": 1, "path": ["Bab IV Cuti"],
       "pageStart": 12, "pageEnd": 15, "chunkStart": 40, "chunkEnd": 57,
       "estimatedChars": 9200, "marker": "bab"}
    ],
    "estimatedChars": 182000,
    "hasSpans": true
  }
  ```

- **Section sources, in order:**
  1. the stored outline;
  2. markdown headings in chunk metadata (`section` / heading path);
  3. Indonesian legal markers detected by `kb-core` (pure function, no I/O):
     `^BAB [IVXLC]+`, `^Bagian (Kesatu|Kedua|…)`, `^Pasal \d+`, `^Paragraf \d+`, and
     `marker ∈ {bab, bagian, pasal, paragraf, heading, page}`;
  4. if nothing is found, one section per page.
- **Chunk ranges** come from the chunk store, through a scroll by `documentId` ordered by
  `chunkIndex` (add a port method if none exists, e.g.
  `ChunkStore::list_chunk_meta(document, scope)` returning index, page, section and
  `char_*`, with no vectors).
- The scope check is the same as the handler's existing one.
- `estimatedChars = sum(char_end - char_start - overlap)`, falling back to `content.len()`
  when spans are missing.
- **Tests:**
  - an Indonesian regulation-style markdown fixture (`BAB I … Pasal 1 … Pasal 2 … BAB II`)
    gives the expected tree;
  - a PDF without headings gives per-page sections;
  - another tenant's document gives 404;
  - a KB-limited key outside the KB gives 404 (see `SEC-1`).

### RET-14 · `GET /v1/documents/:id/content` — read on, without re-searching

- **Contract** (scope `kb:read`; same document visibility as `/structure`):

  ```
  GET /v1/documents/:id/content?section=s3            # or
       ?pageStart=12&pageEnd=13                       # or
       ?chunkStart=40&chunkEnd=57                     # or
       ?cursor=<opaque>
       &maxChars=8000                                 # default 8000, clamp 500..50000
  200 {
    "documentId": "...", "text": "...stitched, overlap removed...",
    "range": {"chunkStart": 40, "chunkEnd": 47, "pageStart": 12, "pageEnd": 13},
    "truncated": true, "nextCursor": "opaque-or-null",
    "refs": [{"kind": "table", "tableId": "...", "page": 12},
             {"kind": "figure", "assetKey": "...", "page": 13}],
    "degrades": []                                  # e.g. "content: spans missing; overlap not removed"
  }
  409 {"error": "document changed since cursor was issued"}
  ```

- **The cursor** is base64url of `{documentId, nextChunkIndex, contentHash}`.
  - `contentHash` is the document's content hash at issue time (use `Document.sha256` from
    `ING-10`, or the latest ingest job id if `sha256` is null).
  - On read, a hash mismatch is a 409.
  - A cursor whose `documentId` is not the path id is a 400.
  - A cursor never reveals another document: it is validated against the path id and the
    scope.
- **Stitching:**
  - order the chunks by `chunkIndex` and append `text[overlapPrefixLen..]`;
  - cut at a chunk boundary once `maxChars` is reached, always emitting at least one chunk;
  - chunks without spans are appended whole, with a degrade.
- **`refs`:** tables (from `/v1/tables` rows for the document whose pages fall in the range)
  and figures (figure chunks or assets on those pages). Ids and pages only, no payloads.
- **Performance:** one indexed Qdrant scroll (`documentId` filter + `chunkIndex` range, using
  the `ING-13` integer index). Target p95 < 50 ms on a 943-page document, measured on the
  local stack (`rust/scripts/bench-ingest.sh` corpus).
- **Tests:**
  - **byte-exact:** ingest a fixture (stub embedder). The concatenation of all cursor pages
    equals the `ING-13` reconstruction, with no duplicated overlap;
  - `maxChars` respected; at least one chunk;
  - re-upload or reindex between cursor pages → 409;
  - another tenant → 404; KB-limited key → 404; deleted document → 404;
  - a cursor for document A used on the path of document B → 400.
- **Also:** add the route to OpenAPI, `docs/api.md` (with an "agent reading loop" example:
  search → structure → content) and the leak matrix (`kb-eval` `leak_http.rs`: a `content`
  row).

### RET-15 · Search inside a part of a document

- **Additive `/v1/search` body fields:**
  - `pageRange: {start, end}` and `section: "s3"`. Valid only together with a single-element
    `documentIds`; otherwise 400 with a clear message.
  - Every arm gets the filter: dense, BM25, page-image, figure, neighbour and tree. Implement
    it as a scope or extra filter handed to the index (Qdrant integer range on `page`, from
    `ING-13`), **not** as a post-filter.
  - `section` resolves to a chunk range through the outline v2 (`RET-13`).
- **Tests:**
  - a keyword that exists on pages 3 and 30 with `pageRange 1–5` → only page 3;
  - a page-image hit outside the range never appears;
  - recall inside the range equals searching a copy of the document cut to those pages, on a
    fixture.

### ING-14 · Stateless parity

`POST /v1/process` (`rust/crates/kb-engine/src/processor.rs`, `kb-server/src/stateless.rs`)
additively emits:
- the outline v2 `sections` from `RET-13`, built by the same `kb-core` function;
- per-chunk `charStart`, `charEnd` and `overlapPrefixLen`;
- per-page `pageSha256`.

Update `docs/modes/stateless/guidelines.md` with:
- the Qdrant payload recipe (integer indexes on `page` and `chunkIndex`);
- a client-side stitching snippet.

Tests in `processor_tests.rs`: the spans reconstruct the text exactly as in `ING-13`.

### EVAL-7 · Progressive-disclosure coverage gate

- **What:** a deterministic scripted reader (no LLM) in `rust/crates/kb-eval`
  (`kb-eval disclosure`).
  - Per question it calls `/v1/search` (top 5), then `/v1/documents/:id/structure`, then
    `/content` on the hit's section or page window, under a char budget of
    **1.5 × the gold pages' chars**.
  - **Coverage** is the fraction of gold pages whose text was fully returned.
  - **Gold** is external human labels: ViDoRe v3 HR page qrels (already used in
    `docs/eval-ab/vidore3-hr-en`). It is page-level gold, not ours.
  - Output: coverage, mean chars read / gold chars, and p95 per call.
- **Targets:** coverage ≥ 0.90 at ≤ 1.5× gold chars; `/content` p95 < 50 ms.
- If the target is missed, report the number; the reviewer decides.
- Results go to `docs/eval-ab/disclosure-v1/README.md`.

### Eval debt

- **`TBL-1` re-score:** the `feat/tbl-fix` fixes are merged. Re-run `tables-v1` (386
  questions) through the HTTP API on a local stack and record execution and top-5.
  - The target is execution ≥ 0.95 for v0.9.0.
  - If it is missed: up to two fix rounds on the largest failure class. Then ship with the
    measured number written down.
- **`DEP-7` (ThinkStation):** the four measurements in its row. Nemotron already serves on
  `127.0.0.1:8210` there.
- **`EVAL-8` (owner):** a human-labelled in-domain Indonesian set.
  - 30–50 enterprise documents, including versioned pairs and `Bab`/`Pasal` layout, and
    100–150 questions with gold pages.
  - **Blocked on the owner's documents.** It is not a release blocker. Until it exists, the
    release notes say that in-domain quality is unmeasured.

---

## v0.10.0 — P1

### RET-16 · Versions and point-in-time search

- **Additive, nullable Prisma migration on `Document`:**
  - `docKey text` (indexed with `tenantId`), `version text`, `supersedesDocumentId text`,
    `effectiveFrom timestamptz`, `effectiveTo timestamptz`;
  - `isLatest boolean NOT NULL DEFAULT true`.
- **Upload and PATCH accept these fields.** Uploading with an existing `docKey` makes the new
  document latest and sets the previous latest to `isLatest=false`.
  - If the previous latest has no `effectiveTo`, it is set to the new `effectiveFrom` (or now).
  - Do this in one transaction.
- **Search:**
  - **default** searches only `isLatest = true` or `docKey IS NULL`;
  - `includeHistory: true` searches all;
  - `asOf: "2024-06-01"` searches documents with
    `effectiveFrom ≤ asOf < coalesce(effectiveTo, ∞)`, and `isLatest` is ignored.
  - Applied in `build_scope` (Postgres), so every arm inherits it.
- **Response:**
  - each chunk and source carries `docKey`, `version`, `effectiveFrom` and `effectiveTo`;
  - `multiVersionWarning: [{docKey, versions:[…]}]` when the returned set holds more than one
    version of one `docKey` (possible only with `includeHistory`).
- **Tests:**
  - v1 and v2 of "Kebijakan Cuti" (12 vs 14 days). Default → only v2;
    `asOf` before v2's `effectiveFrom` → only v1; `includeHistory` → both, plus the warning;
  - the leak test: `asOf` never widens the scope past the tenant or KB.
- **Targets:** 0 stale-version leaks; 100% `asOf` accuracy on the fixture.

### RET-17 · `maxPerDocument`

- **Additive search field** `maxPerDocument: u32` (1..50), applied after rerank and before
  the final cut, in `kb-engine`.
  - A setting `KB_MAX_PER_DOCUMENT` provides the default; unset means off.
  - The response stats record `perDocumentCapped: n`.
- **Tests:** 10 chunks from A and 2 from B, `maxPerDocument=2`, `maxChunks=4` → A, A, B, B in
  rerank order.
- **Measure:** unique-docs@10 on MIRACL-id-200 with the cap off vs at 3, and the nDCG@10 delta
  (must be ≥ −0.005).

### RET-18 · Confidence the caller can act on

- **Additive response `confidence`:**

  ```json
  {"topScore": 0.82, "scoreKind": "rerank|dense|fused",
   "margin": 0.11, "band": "high|medium|low|none", "calibrated": true}
  ```

- **Bands:**
  - thresholds per scoring model, from a calibration table in `kb-core` keyed by the reranker
    or embedder model id;
  - fitted with `kb-eval calibrate` on MIRACL-id-200 positives vs the off-topic set already used
    for `RET-2` (`docs/eval-ab/retrieval-gaps`);
  - `calibrated: false` when the active model has no table entry. Then `band` comes from
    `minScore`-style fallbacks and is said to be uncalibrated.
- **Target:** on the held-out half, `band=none` on ≥ 90% of off-topic queries and on ≤ 5% of
  answerable ones.

### RET-19 · Passage breadcrumbs (conditional)

Only if `RET-12` won its A/B. The same breadcrumb is prepended to the text that is **embedded
and BM25-indexed** (not to the displayed content). It needs a re-index, so ask the owner with
a cost estimate first (rule 7).

### DEC-6 · Freeze the graph and global arms

`KB_GRAPH_ARM` and `KB_GLOBAL_ARM` stay off and are deprecated:
- a startup warning when either is set;
- a note in `docs/api.md`, `CHANGELOG.md` and the README.

Reopen only if `EVAL-8` multi-hop R@10 < 0.85 with dense + rerank + neighbour, and a
proposition-at-ingest variant gives ≥ +5 pp R@5 at ≤ +15 ms. Evidence:
`docs/eval-baseline.md` (HotpotQA −8.5 pp, +400 ms p95), `EVAL-3`.

---

## v1.0.0

### SEC-2 · Predicate scope with a fail-closed verify

- **Why:** `DocumentScope` (`rust/crates/kb-ports/src/scope.rs`) materialises every visible
  `documentId` and sends it to Qdrant as a `match any` on every search. At 100k documents that
  is megabytes per query. Postgres stays the authority.
- **Design:**
  - `DocumentScope` becomes an enum: `Ids(Vec<DocumentId>)` (today's path, used when the
    count is ≤ `KB_SCOPE_ID_LIMIT`, default 2000) or
    `Predicate { tenant, kb_ids, filter, epoch }`.
  - In predicate mode, Qdrant filters on payload `tenantId` + `knowledgeBaseIds` (+
    `language` / `category`). This is an **optimisation only**:
    1. Every hit is **verified against Postgres** (`find_alive_meta_by_ids` with the full
       filter) before it can be returned.
    2. Unverified hits are dropped and counted in `stats.scopeVerifyDropped`.
    3. The arm over-fetches (`k × 1.5`) to keep recall.
  - Payload drift is repaired by an **outbox**:
    - a Postgres table `ScopeOutbox(documentId, op, createdAt)`, written in the same
      transaction as any KB move, delete or ACL change;
    - a worker applies it with Qdrant `set_payload` / delete.
- **Tests:**
  - the leak matrix with **deliberately stale Qdrant payloads**: move a document out of a KB
    in Postgres only, then search through every arm, `/content`, `/structure`, `/tables` and
    `/mcp` → 0 leaks, and `scopeVerifyDropped > 0`;
  - the outbox drains and the next search drops nothing.
- **Bench:** a synthetic tenant with 100k documents (random 2048-d vectors are fine for scope
  cost) gives search p95 ≤ 1.2× a 1k-document tenant. Run on the ThinkStation (RAM).

### API-4 · Thin `/mcp`

- **What:** Streamable HTTP MCP on the same port, behind `KB_MCP=on` (default off), with the
  same Bearer and `X-API-Key` auth and scopes.
- **Tools:** `kb_search`, `kb_list_documents`, `kb_outline` (→ structure), `kb_read`
  (→ content) and `kb_query_table`. Each is a thin call into the existing handlers.
  - The tool and parameter descriptions are written for an LLM, with a short "reading loop"
    hint.
  - The server keeps no session state.
  - < 600 LoC, no new dependency unless a maintained Rust MCP crate fits. If one is used,
    check its licence first: Apache or MIT only.
- **Tests:**
  - `tools/list` and `tools/call` round-trip;
  - another tenant's id → error;
  - a `/mcp` row in the leak matrix.
- **Not built:** stateless `/mcp`.

### API-5 · Stateless parity tests

Add tests that pin the stateless mode to the full mode:
- `/v1/encode` (query and passage) equals what full mode indexes and queries with, bit for
  bit, on a stub and on a prefixed model config;
- `/v1/rerank` equals full mode's rerank order for the same candidates.

### SEC-3 · `aclPrincipals`

- **What:**
  - documents get an optional `aclPrincipals text[]`, set by upload or PATCH (additive
    migration, GIN index);
  - search, content, structure, tables and `/mcp` accept `principals: ["user:42",
    "group:hr"]`, **only if the key has scope `kb:assert-principals`**. Otherwise 403.
- **Rules:**
  - a document with a non-empty ACL is visible only when the sets intersect;
  - **default-deny:** an ACL'd document is invisible to a request without principals;
  - documents without an ACL behave as today.
  - The ACL is enforced in the `SEC-2` Postgres verify, and mirrored to the Qdrant payload
    through the outbox.
- **Tests:** a principal leak matrix (every arm and route) = 0, including stale payloads.

### RET-20 · Quantization (opt-in)

- **What:** `KB_QDRANT_QUANTIZATION=off|scalar|binary`, applied at collection creation, with
  rescoring on and oversampling at 2.0. Re-index note in the guide.
- **Bench (ThinkStation):** 1M chunks.
  - p95 < 20 ms;
  - recall@20 ≥ 0.97 against float32 on 500 queries;
  - RAM before and after.
- **Not the default.**

### REL-7 · v1.0 freeze

- `docs/api.md` marks `/v1/*` stable.
- `docs/rust-cutover.md` gets a "0.8 → 1.0" section listing every behaviour change.
- The README is updated, and `CHANGELOG.md` has a `1.0.0` entry.
- Gates:
  - the live smoke `rust/scripts/smoke.sh`;
  - `rust/scripts/ci-eval-gate.sh`;
  - the full live leak matrix, 0 leaks.

## Validation the reviewer runs (never the runner's word)

- **Per PR:**
  - the four gates on a clean target dir;
  - the new tests are read for loosened assertions;
  - one failing-then-passing check of the bug tests.
- **Per release:**
  - a local stack up;
  - `smoke.sh`;
  - the live leak matrix;
  - the `EVAL-7` disclosure gate (v0.9.0+);
  - the A/B numbers in `docs/eval-ab/*`;
  - then a release PR, a tag, and a green release workflow.
