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
`tablePrivacy` — `open` (default) or `aggregate-only`. Versioned documents add `docKey`,
`version`, `effectiveFrom` and `effectiveTo` (see "Versioned documents" below); the two date
fields accept `YYYY-MM-DD` or a full ISO timestamp and a bad date is a `400`.

**Search options** (JSON): `query` (required), `maxChunks`, `maxPerDocument`, `knowledgeBaseIds`,
`documentIds`, `category`, `hybrid`, `format`, `minScore`, `pageRange`, `section`, `includeHistory`
(default `false`) and `asOf` (`YYYY-MM-DD` or a full ISO timestamp; a bad date is a `400`).

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

`stats.scopeVerifyDropped` is additive (SEC-2a): when the tenant has more visible documents
than `KB_SCOPE_ID_LIMIT`, the index filters on the tenant / knowledge-base / category payload
instead of sending every document id, and Postgres re-verifies every candidate before fusion.
The field counts candidates dropped because their document was no longer visible (stale
payload, deleted document, a move between knowledge bases); it is absent when the id-list path
ran. A failed verify fails the search rather than returning unverified hits.

`chunks[].content`, `documentTitle`, `page` and `similarity` carry the results; `degrades`
is non-empty when an arm failed, because a failed arm is reported rather than silently
dropped. Each chunk and source may also carry `docKey`, `version`, `effectiveFrom` and
`effectiveTo`; these are absent for documents uploaded before versioning.

## Versioned documents (RET-16)

Upload several versions of one policy under a stable `docKey`:

```sh
curl -F file=@cuti-2024.txt -F 'title=Kebijakan Cuti' \
     -F docKey=cuti -F version=2024 -F effectiveFrom=2024-01-01 \
     -H 'Authorization: Bearer KEY' /v1/documents
curl -F file=@cuti-2025.txt -F 'title=Kebijakan Cuti' \
     -F docKey=cuti -F version=2025 -F effectiveFrom=2025-01-01 \
     -H 'Authorization: Bearer KEY' /v1/documents
```

The second upload makes `2025` the current version in one transaction: it sets the old
row's `isLatest=false`, closes its `effectiveTo` at `2025-01-01` (or at the upload time
when the new version has no `effectiveFrom`) and records the old id in the new row's
`supersedesDocumentId`. Deleting the latest version does **not** promote the previous
one — upload that version again under the same `docKey` to restore it.

`PATCH /v1/documents/:id` accepts `docKey`, `version`, `effectiveFrom` and `effectiveTo`;
absent fields are left alone. `GET /v1/documents/:id` and `GET /v1/documents` report
`docKey`, `version`, `supersedesDocumentId`, `effectiveFrom`, `effectiveTo` and `isLatest`.

**Search modes** (all narrow the tenant/KB scope; none can widen it):

| Body | Documents searched |
|---|---|
| default | only current versions (`isLatest=true`) and every unversioned document (`docKey` null) |
| `includeHistory: true` | all versions |
| `asOf: "2024-06-01"` | versions with `effectiveFrom <= asOf < effectiveTo`; null bounds are unbounded, undated documents stay visible, `isLatest` ignored |

`asOf` takes precedence if both fields are sent. When an answer contains chunks from more
than one document with the same `docKey`, the response adds
`multiVersionWarning: [{docKey, documentIds: [...], versions: [...]}]` so a caller that
asked for history is warned that it is mixing versions. A default search can never mix.

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

## Access lists (SEC-3)

A document may carry an **access list** of principals. rantai-kb does not know the
calling app's users or groups; the app asserts the principals the caller holds on
each request, and rantai-kb shows an ACL'd document only when the two sets overlap.

- **Set it** with the multipart field `aclPrincipals` on upload, or the JSON field
  `aclPrincipals` on `PATCH /v1/documents/:id`. Both accept an array such as
  `["user:42","group:hr"]` (the upload form also accepts `user:42,group:hr`).
  Each entry is non-empty, at most 200 characters and matches
  `^[A-Za-z0-9_.:@-]+$`; at most 100 entries. A violation is a `400`. Setting an
  ACL needs the `kb:write` scope, like uploads. `GET /v1/documents/:id` and
  `GET /v1/documents` return `aclPrincipals` additively; `[]` means no ACL.
- **Assert them** with the header `X-KB-Principals: user:42,group:hr` on search,
  the document list, `GET /v1/documents/:id` and its `/raw`, `/structure`,
  `/content`, `/assets` and `/intelligence` sub-resources, `/v1/tables` and the
  thin `/mcp` tools (`KB_MCP=on`). `POST /v1/search` also accepts a body field
  `principals: ["user:42","group:hr"]`;
  the header and body sets are combined. Supplying either **requires the
  `kb:assert-principals` scope** — a leaked ordinary `kb:read`/`kb:write` key
  cannot claim `group:admin`. Without it the request is a `403`.
- **Default-deny:** a document with a non-empty ACL is invisible to a request
  that asserts no principals, in *every* retrieval arm and on every by-id route
  (it answers `404`). An un-ACL'd document behaves exactly as before. Because
  the Qdrant payload has no ACL, a tenant with any ACL'd document stays on the
  Postgres-backed id-list scope; the SEC-2a verify applies the same predicate.

```sh
# Store HR files for the hr group only (kb:write key):
curl -F file=@salaries.pdf -F 'aclPrincipals=["group:hr"]' \
     -H 'Authorization: Bearer WRITE_KEY' /v1/documents

# An app with the privileged key asserts what this caller holds:
curl -H 'Authorization: Bearer KEY_WITH_ASSERT_SCOPE' \
     -H 'X-KB-Principals: user:42,group:hr' \
     -H 'Content-Type: application/json' \
     -d '{"query":"salary bands"}' /v1/search
```

Create the privileged key with
`kb-server create-api-key <tenant> --scopes kb:read,kb:assert-principals`.

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

## MCP

When `KB_MCP=on` (default **off**), the same port also serves a thin
Streamable-HTTP MCP endpoint at `POST /mcp`. It takes JSON-RPC 2.0 requests and
answers `application/json` — there are no sessions and no SSE stream, so a GET
on the endpoint answers `405`. With the flag off, `/mcp` is an unknown path and
answers `404` like any other unmatched route.

Authentication is identical to `/v1/*`: `Authorization: Bearer <key>` or
`X-Api-Key`. No key is `401`; the tools need `kb:read`, and table queries use
the same table-route authorisation. A tool failure (404, 400, 409) is a
`tools/call` result with `isError: true` and the API's own error message — not
a JSON-RPC protocol error and never a response containing another tenant's
content.

Supported methods: `initialize`, `notifications/initialized`, `ping`,
`tools/list` and `tools/call`. The server echoes the client's
`protocolVersion` when it is one of `2025-06-18`, `2025-03-26` or
`2024-11-05`; otherwise it answers `2025-06-18`.

A generic MCP client (Streamable HTTP) config:

```json
{
  "mcpServers": {
    "rantai-kb": {
      "type": "http",
      "url": "https://kb.example.com/mcp",
      "headers": {"Authorization": "Bearer <your-api-key>"}
    }
  }
}
```

For Claude Code, the equivalent is:

```bash
claude mcp add --transport http rantai-kb https://kb.example.com/mcp \
  --header "Authorization: Bearer <your-api-key>"
```

The tools, with the arguments they take:

| Tool | Arguments | Use it for |
|---|---|---|
| `kb_search` | `query`, `maxChunks?`, `documentIds?`, `pageRange?`, `section?` | The search response: chunks with `documentId`, `page`, `section`, `content`, `sources` and `context`. Start here. |
| `kb_list_documents` | `knowledgeBaseId?`, `limit?`, `cursor?` | Discover `id`, `title`, `pages`, `status`, `knowledgeBaseIds` and `externalRef` when you have no search hit. |
| `kb_outline` | `documentId` | The `/structure` sections (ids, titles, page/chunk ranges). |
| `kb_read` | `documentId`, plus one of `section`, `pageStart`+`pageEnd` or `cursor`, and `maxChars?` | The `/content` text of one document without re-searching; follow `nextCursor` while `truncated`. |
| `kb_query_table` | `tableId`, `query` | The `/v1/tables/:id/query` rows and receipt for exact numbers in a stored table. |

The intended **reading loop** is `kb_search` → `kb_outline` → `kb_read`
(repeating with `nextCursor`), with `kb_query_table` when the answer lives in a
table. A successful result is
`{content: [{type: "text", text: "<the JSON>"}], structuredContent: <the JSON>}`.

Each tool calls the same shared handler function as its `/v1` route, so tenant
scope, knowledge-base limits, versions and visible degradations are identical.

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

