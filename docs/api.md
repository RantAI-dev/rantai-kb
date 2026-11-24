# The HTTP API

All routes are under `/v1` and authenticate with `Authorization: Bearer <key>` (or
`X-Api-Key`). Health and formats are unauthenticated. Every key belongs to exactly one
tenant; **there is no route that reads across tenants.**

The contract is machine-readable: `GET /openapi.json` serves an OpenAPI 3.1 document and
`GET /docs` renders it with a "try it" button. Both are unauthenticated, like `/health`,
because the contract has to be readable before the caller has one.

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/documents` | Upload a file (multipart field `file`). Returns `202` with a `jobId`. |
| `GET` | `/v1/documents` | List documents. Filters: `knowledgeBaseIds`, `externalRef`, `limit`. |
| `GET` `PATCH` `DELETE` | `/v1/documents/:id` | Read, edit or delete one document. |
| `GET` | `/v1/documents/:id/raw` | Download the stored original. |
| `GET` | `/v1/documents/:id/assets` | Fetch a document figure or page image. |
| `GET` | `/v1/documents/:id/structure` | Pages, figures and the chunk → page map. |
| `GET` | `/v1/documents/:id/content` | Bounded, byte-exact text for a section, page or chunk range, with a cursor. |
| `GET` | `/v1/documents/:id/intelligence` | Extracted entities and relations. |
| `POST` | `/v1/search` | Retrieve chunks for a query. The reason this service exists. |
| `GET` | `/v1/tables` | A document's stored tables (`?documentId=…`), or the tenant's. |
| `GET` | `/v1/tables/:id` | One table with its manifest: columns, roles, units, subtotal rows. |
| `POST` | `/v1/tables/:id/query` | Run a typed table query and get rows plus a receipt. |
| `GET` | `/v1/jobs/:id` | Ingest progress: step, percentage, ETA, error. |
| `POST` | `/v1/jobs/:id/retry` | Retry a failed ingest job. |
| `GET` `POST` | `/v1/knowledge-bases` | List or create knowledge bases. |
| `PATCH` `DELETE` | `/v1/knowledge-bases/:id` | Edit or delete a knowledge base. |
| `GET` `POST` | `/v1/categories` | List or create categories. |
| `PATCH` `DELETE` | `/v1/categories/:id` | Edit or delete a category. |
| `GET` | `/v1/events` | Server-sent ingest progress for the tenant. |
| `GET` | `/v1/formats` | Accepted extensions and the size limit. No auth. |
| `GET` | `/health` · `/v1/health` | Liveness, dependency checks, build version, tier and egress hosts. No auth. |

**Ingest options** (multipart fields): `title`, `categories`, `subcategory`,
`knowledgeBaseIds`, `externalRef`, `figures` — `auto` (default), `force` or `skip` — and
`tablePrivacy` — `open` (default) or `aggregate-only`.

**Search options** (JSON): `query` (required), `maxChunks`, `maxPerDocument`, `knowledgeBaseIds`,
`documentIds`, `category`, `hybrid`, `format`, `minScore`, `pageRange`, `section`.

`maxPerDocument` (additive, RET-17) caps how many chunks one document may contribute to an
answer. It is an integer in `1..=50`; `0` or a value above `50` is a `400`. The cap is applied
after rerank and before the final `maxChunks` cut: hits that exceed it are dropped (not moved
to the end), so the survivors keep their rerank order. When the cap is on, the response's
`stats.perDocumentCapped` reports how many hits it dropped; when it is off, that field is
absent. Figures are attached after the cap and are not counted, and neighbour chunks are
counted against the same cap so a capped document cannot regain chunks as context. The
server default is `KB_MAX_PER_DOCUMENT` (unset or `0` = off).

```json
POST /v1/search
{"query": "compare vendor proposals", "maxChunks": 8, "maxPerDocument": 2}
```

With ten chunks from vendor A and two from vendor B at the top, a cap of `2` and
`maxChunks: 4` returns `A, A, B, B` — rather than four chunks from A — and reports
`"perDocumentCapped": 8` in `stats`.

`documentIds` restricts a search to the named documents using the full pipeline — BM25,
reranker, page/tree/neighbour and figure arms all run — inside the caller's visible scope. It
only narrows that scope: a document from another tenant or outside the key's knowledge bases
is never searched. Such an answer carries `sources` and `context` in the standard format, just
like any other search.

`pageRange: {start, end}` (inclusive, 0-based pages) and `section: "s3"` narrow the search to
one part of a document. Both are additive and require **exactly one** `documentIds` entry;
they cannot be combined. `pageRange.start > pageRange.end` and an unknown section id are
`400`s. A section id comes from `GET /v1/documents/:id/structure` (`sections[].id`) and resolves
to that section's chunk range (and its page range for the page arm). The restriction is applied
inside the index query — an integer range on `page` / `chunkIndex`, backed by the ING-13
payload indexes — so dense, BM25, page-image, figure, neighbour and tree arms cannot return a
unit outside the part, and top-k is never spent on one.

```json
POST /v1/search
{"query": "berlaku 12 hari", "documentIds": ["doc-42"], "pageRange": {"start": 12, "end": 15}}
```

`chunks[].content`, `documentTitle`, `page` and `similarity` carry the results; `degrades`
is non-empty when an arm failed, because a failed arm is reported rather than silently
dropped.

### Confidence (additive, `RET-18`)

Every standard and `documentIds` search answers with a top-level `confidence` object;
the `hybrid: true` stub does not, because it runs no search:

```json
{"topScore": 0.82, "scoreKind": "rerank", "margin": 0.11,
 "band": "high", "calibrated": true}
```

- `topScore` — the highest comparable score of the returned hits; `null` when
  nothing was scored. `scoreKind` is `rerank` when a reranker supplied it,
  `dense` for the dense cosine, and `fused` for the rank-only fused score when
  neither arm produced a comparable value.
- `margin` — `topScore` minus the second-highest score of the same kind; `null`
  when fewer than two hits were scored. A single hit has no margin to report.
- `band` — `high`, `medium`, `low`, `none` or `unknown`. With a fitted
  calibration for the active model, `high`/`medium`/`low` are the fitted
  thresholds and any lower score is `none`. Without one, `minScore` (request or
  server default) gives `low` at or above it and `none` below; with no floor at
  all the band is `unknown`. An answer with no scored hits is always `none`.
- `calibrated` — `true` only when the active scoring model has a fitted entry.
  It is `false` by default: no calibration is shipped in the binary because no
  three-point fit is committed yet.

**How to use it:** answer when `band` is `high` or `medium`; say "I don't know"
when it is `none` or `unknown`. When `calibrated` is `false`, treat the band as a
hint only — the thresholds are a relevance floor, not a fitted probability.
A failed search still reports itself in `degrades`; never treat that as
`band: none`, which means "nothing was relevant", not "the search failed".

An operator installs a fit with `KB_CONFIDENCE_CALIBRATION=/path/calibration.json`
(an array of `{modelId, scoreKind, high, medium, low}`). The file is validated at
startup; a malformed one is a startup error. Fit it with
`kb-server eval calibrate-confidence` — see
[`docs/eval-ab/confidence/README.md`](eval-ab/confidence/README.md).

## Tables

An xlsx, CSV, DOCX or OCR/markdown table is stored as a typed grid (Parquet in the blob
store, one record per version in `KbTable`) and indexed as two retrieval units,
`table_summary` (title, caption, sheet or page, every header with its kind, role and unit,
sample values) and `table_row` (`Header: value; …`, for tables of at most 60 rows). A
search hit from either carries `tableId` and `tableTitle`, and the answer lists
`tables: [{tableId, title, documentId}]` for its documents, so a caller can go straight to
a number instead of adding up text:

```json
POST /v1/tables/doc123_t0/query
{"select": [{"column": "Region"}, {"agg": "sum", "column": "Revenue", "as": "total"}],
 "filter": [{"column": "Tahun", "op": "=", "value": 2023}],
 "group_by": ["Region"], "order_by": [{"key": "total", "dir": "desc"}], "limit": 10}
```

The query is **typed, never SQL**: columns are checked against the manifest (a SUM over a
phone-number column is refused), identifiers are quoted from the manifest's whitelist and
values are bound parameters. Subtotal rows are excluded unless `includeDerived` is set.
The answer is `{columns, rows, rows_matched, excluded_derived_rows, suppressed_groups,
truncated, warnings}` — the receipt says what was computed and from which table. A refused
query answers `400` with the `QueryError` JSON (`unknown_column` with `available`,
`not_summable`, `not_grouped`, `restricted`, `bad_value`, `empty`). `tablePrivacy:
"aggregate-only"` on upload restricts the document's tables to whole-group aggregates and
suppresses groups smaller than five rows (with complementary suppression), and forbids
plain rows and MIN/MAX. Tables are tenant-scoped and document-authorised like every other
by-ID read; a hard delete removes them and their Parquet.

## Document structure

`GET /v1/documents/:id/structure` returns the document's `pages` (`number`, 0-based like
every other `page` in the API), `figures` (`key`, `page`, `bbox`, `caption`, `assetKey`)
and the `chunks` → page map, plus `documentId`, `externalRef` and `title`. It never
re-parses the file: pages come from the page-image index, figures from the figure chunks a
search can return and the document's figure records, and chunks from the text index.
`hasTextLayer` and `ocrText` are recorded at ingest from the 0.3 release on; `ocrText` is
the page's stored text (at most 8,000 characters) and omits the visual description the
caption stage appends. `imageAssetKey` is present only when ingest recorded that the page
JPEG was written. A document ingested before these fields were recorded answers `null` for
them, never a guess. The read is capped at 20,000 chunks and 2,000 pages and sets
`truncated` when either cap is reached. Like every by-ID route it enforces the tenant and
the key's knowledge-base limit.

### Outline v2 (additive)

The same response also carries a table of contents, built by `kb-core` from the document's
chunks. These fields are additive: no field above changed.

- `outlineVersion: 2`.
- `sections`: an ordered list of `{id, title, level, path, pageStart, pageEnd, chunkStart,
  chunkEnd, estimatedChars, marker}`. `id` is stable within the document (`s1`, `s2`, …)
  and is what the content route's `?section=` parameter uses. Chunk ranges are inclusive;
  page ranges are `null` when the chunks carry no page. `marker` is one of `bab`, `bagian`,
  `pasal`, `paragraf`, `heading`, `page`.
- `estimatedChars`, `hasSpans`: document-level estimates. `hasSpans` is `true` only when
  every chunk carries a complete character span; old chunks answer `false`.

Section sources are tried in order: the stored PDF outline, headings in chunk metadata,
Indonesian legal markers (`BAB`, `Bagian`, `Pasal`, `Paragraf`) at the start of lines, and
finally one section per page. An agent can read `sections` first, then request exactly one
section's chunk range from `/content`.

```json
{
  "outlineVersion": 2,
  "sections": [
    {"id": "s1", "title": "BAB I KETENTUAN UMUM", "level": 1,
     "path": ["BAB I KETENTUAN UMUM"], "pageStart": 0, "pageEnd": 0,
     "chunkStart": 0, "chunkEnd": 0, "estimatedChars": 100, "marker": "bab"},
    {"id": "s3", "title": "Pasal 2", "level": 4,
     "path": ["BAB I KETENTUAN UMUM", "Pasal 2"], "pageStart": 0, "pageEnd": 0,
     "chunkStart": 1, "chunkEnd": 1, "estimatedChars": 100, "marker": "pasal"}
  ],
  "estimatedChars": 450,
  "hasSpans": true
}
```

## Reading on without re-searching

`GET /v1/documents/:id/content` returns the text of one document in order, stitched
from its stored chunks with overlap removed, under a character budget. It is for
an agent that already found a chunk and wants the surrounding section or pages;
it never searches again and never re-parses the file. Exactly one selector is
allowed:

- `?section=s3` — a section id from `/structure`'s outline v2;
- `?pageStart=12&pageEnd=13` — inclusive zero-based pages;
- `?chunkStart=40&chunkEnd=57` — inclusive chunk indices;
- `?cursor=<opaque>` — continue a previous read.

No selector reads the whole document from chunk 0. `?maxChars=` defaults to 8000
and is clamped to 500..50000. The response is
`{documentId, text, range:{chunkStart, chunkEnd, pageStart, pageEnd}, truncated,
nextCursor, refs, degrades}`. `refs` names tables on the returned pages
(`kind:"table", tableId, page`) and figures on the returned chunks
(`kind:"figure", assetKey, page`), ids and pages only.

When `truncated` is true, send `nextCursor` with the same `maxChars` to get the
next slice. Concatenating all slices is byte-identical to one unbounded read:
page boundaries are not printed and overlap is never duplicated. The cursor is
bound to the document id and to the document's content hash (`sha256`, or
`updatedAt` when there is no hash): using it on another document answers `400`,
and a changed document answers `409 {"error":"document changed since cursor was issued"}`.
Old chunks without spans are emitted whole and the answer carries a `degrades`
entry rather than silently losing text.

### Agent reading loop

```text
POST /v1/search  ->  a hit with documentId, section and page
GET  /v1/documents/:id/structure  ->  sections (s1, s2, s3, …) with chunk ranges
GET  /v1/documents/:id/content?section=s3&maxChars=8000
GET  /v1/documents/:id/content?cursor=<nextCursor>&maxChars=8000  ->  repeat until truncated=false
```

## Keys

`rantai-kb key <tenant>` (or `kb-dev.sh key`) mints an unrestricted key. For anything
narrower, `kb-server create-api-key` takes `--scopes kb:read,kb:write` and
`--kb id,id`. The tenant string is the scope boundary: a key sees its own tenant's
documents and nothing else.

## Stateless processor mode

When you own your own database and Qdrant server and want `rantai-kb` strictly as a
document-processing microservice (zero Postgres, Qdrant, or S3 connections), start it
with `kb-server serve --stateless` or `KB_MODE=stateless` (full integration & Qdrant
setup guide: [`docs/modes/stateless/guidelines.md`](modes/stateless/guidelines.md);
full-service guide: [`docs/modes/full/guidelines.md`](modes/full/guidelines.md)):

```bash
KB_MODE=stateless \
KB_STATELESS_API_KEY=your-shared-secret \
kb-server serve
```

For private VPC / sidecar deployments where another service terminates auth, explicitly
opt out with `KB_STATELESS_ALLOW_UNAUTHENTICATED=true` (boot fails loudly if neither
variable is set). Dense embeddings are optional: omit embedding env vars to run pure
extraction + chunking + tables + figures + Qdrant-ready BM25 sparse vectors with zero
external model dependencies, or set `KB_EMBEDDING_BASE_URL` / `KB_EMBEDDING_API_KEY` /
`KB_TIER` to include dense embeddings.

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/process` | Synchronously process a multipart `file` upload and return text, chunks, tables, figures, BM25 sparse vectors, optional dense embeddings, optional entities/relations, and stage timings. Guarded by `KB_SEATS` and `KB_STATELESS_MAX_INFLIGHT` (sheds bursts with `503` + `Retry-After` before buffering the body). |
| `POST` | `/v1/encode` | Encode `{texts, mode?: "query"\|"passage", dense?, sparse?}` with the exact same FNV-1a BM25 sparse tokenizer and dense embedder/prefixes used by `/v1/process`. `mode: "query"` uses a fast lane and never queues behind bulk ingest seats. |
| `POST` | `/v1/rerank` | Rerank `{query, documents, topK?, returnDocuments?}` through the configured local (TEI / vLLM / llama.cpp) or cloud (Cohere / Voyage / OpenRouter) reranker on the search fast lane. |
| `GET` | `/v1/formats` | Accepted extensions and `maxFileBytes`. No auth. |
| `GET` | `/health` · `/v1/health` | Liveness, `mode: "stateless"`, `capabilities`, `limits`, `backends`, `version`, `tier`, and `egress`. No auth. |
| `GET` | `/openapi.json` · `/docs` | Stateless OpenAPI 3.1 contract and Swagger UI (also printable via `kb-server openapi --stateless`). No auth. |

**Process options** (multipart fields on `POST /v1/process`): `title`, `category`,
`subcategory`, `figures` (`auto`, `all`, `none`, `off`), `documentType`, `embed`
(defaults to `true` when an embedder is configured), `sparse` (default `true`), `tables`
(default `true`), `entities` (default `false`), `includeFigureBytes` (default `false`),
`chunkSize`, and `chunkOverlap`.

**Rerank options** (JSON body on `POST /v1/rerank`): `query` (required), `documents`
(non-empty string array), `topK` (optional positive integer, defaults to all candidates),
and `returnDocuments` (boolean, default `false`). Configured via `KB_RERANK_*` or
`KB_TIER=cloud|desk|studio|station` (disable explicitly with `KB_STATELESS_RERANK=false`).
`KB_RERANK_BREADCRUMBS=off|on` (`false|true` also accepted, default `off`) prepends the
document title, section and page to the text the search reranker sees; it does not change the
response shape and needs no re-index (`docs/core/specs/v1-roadmap.md` RET-12).

`KB_MAX_PER_DOCUMENT` (default `0` = off, clamped to `50`) is the server-wide default for the
search field `maxPerDocument`: it caps the chunks one document may contribute, after rerank and
before the `maxChunks` cut (`docs/core/specs/v1-roadmap.md` RET-17). A request field overrides
it for that search.

**Deprecated (DEC-6):** `KB_GRAPH_ARM` and `KB_GLOBAL_ARM` are frozen and deprecated; enabling
either logs a startup warning naming the evidence (HotpotQA −8.5 pp R@5 and +400 ms p95,
`docs/eval-baseline.md`, `EVAL-3`). They may be removed after v1.0. The arms still work today.

