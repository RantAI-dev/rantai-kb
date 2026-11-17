# Changelog

## Unreleased

### Changed

- **The README is a front page; the detail moved into docs.** It now carries the hardware
  (minimum and recommended) and the model inventory for every tier — params, dimensions,
  precision, runtime and weights on disk — and links out. The API reference is
  `docs/api.md`, the retrieval pipeline `docs/retrieval.md`, the security model
  `docs/security.md` and the measured numbers `docs/performance.md`. The claim that a
  knowledge-base-limited key could still read by-ID has been removed: `SEC-1` shipped in
  v0.2.0.

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
