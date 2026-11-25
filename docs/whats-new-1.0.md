# What's new in rantai-kb 1.0 (since 0.8)

This is a briefing for engineers integrating with rantai-kb or running it. It covers everything
added between v0.8.0 and v1.0.0 (released 2026-11-25), plus the figure work in v1.0.1 and v1.0.2. The full reference is
[`docs/api.md`](api.md), the upgrade checklist is [`docs/rust-cutover.md` §13](rust-cutover.md),
and the line-by-line history is [`CHANGELOG.md`](../CHANGELOG.md).

**In one sentence:** search now finds the right place more reliably, and agents can move from a
search hit to reading the exact section or pages behind it. Documents can have versions and
per-user access lists, scoping holds up for large tenants, and the `/v1/*` API is now stable.

---

## 1. API stability promise

From 1.0, the `/v1/*` HTTP API and the Postgres tables that calling apps rely on (documents,
jobs, knowledge bases, keys) are **stable**:

- every change from here on is additive;
- anything that is not additive is announced in `CHANGELOG.md` and in the cutover guide.

Every 0.9 and 1.0 change listed below is additive. No existing field was removed or renamed, with
one exception, described in §2: `documentIds` searches now return the standard format.

---

## 2. Better search results

| Capability | How to use it | Notes |
|---|---|---|
| **Separate citations for same-titled documents** | Automatic | Before, a 2023 and a 2024 "Kebijakan Cuti" could merge into one source. Now each keeps its own `documentId` and page. |
| **Search inside chosen documents with the full pipeline** | `documentIds: [...]` | Used to be a dense-only shortcut. It now runs BM25, the reranker, and the page/tree/neighbour/figure arms. The response now carries `sources` and `context` in the standard format. It can only narrow what the key may see, never widen it. |
| **Search inside part of a document** | `documentIds: ["<one id>"]` plus either `pageRange: {start, end}` (inclusive, 0-based) or `section: "s3"` | The range is applied inside every index query, so no arm returns or wastes top-k on text outside the range. Section ids come from `/structure`. |
| **Diversity cap** | `maxPerDocument: N` (1–50), or server default `KB_MAX_PER_DOCUMENT` | Stops one long document from taking over the answer. `stats.perDocumentCapped` reports how many hits were dropped. |
| **Confidence object** | Automatic on every standard search | `{topScore, scoreKind, margin, band, calibrated}`. `band` is `high`/`medium`/`low`/`none`/`unknown`. Use it to decide "answer or say I don't know". `calibrated` is `false` until an operator installs a fit (`KB_CONFIDENCE_CALIBRATION`, produced by `kb-server eval calibrate-confidence`). Until then, treat the band as a hint. |

```json
POST /v1/search
{"query": "berapa hari cuti tahunan", "documentIds": ["doc-42"], "section": "s7", "maxPerDocument": 3}
```

---

## 3. Progressive disclosure: from a search hit to the full text

This is the main new capability for agents. A search returns chunks, which are fragments. An
agent can now navigate the document a hit came from and read it in order, **without searching
again**.

These endpoints are **always on**; no flag is needed.

### `GET /v1/documents/:id/structure`: the outline (v2)

- Returns `sections[]`, each with an id (`s1`, `s2`…), a title, a level and a path, page and
  chunk ranges, and a detected marker.
- Indonesian legal structure is recognised: `BAB` / `Bagian` / `Pasal` / `Paragraf`.
- Also returns `estimatedChars` (the document size) and `hasSpans`, which tells whether
  byte-exact reading is possible.

### `GET /v1/documents/:id/content`: read a section or pages

- Use exactly one selector: `?section=s3`, `?pageStart=12&pageEnd=13`,
  `?chunkStart=40&chunkEnd=57`, or `?cursor=…`.
- `maxChars` defaults to 8000 (500–50000). When `truncated: true`, send `nextCursor` to continue.
- The text is **byte-exact**: the overlap between chunks is removed, and joining all slices gives
  the same bytes as one unbounded read.
- The cursor is bound to the document's content hash. If the document changed in between, the
  next read answers `409` instead of mixing two versions.
- `refs` lists the tables and figures on the returned pages. Use the table ids with
  `/v1/tables/:id/query`.
- Measured p95: **5.5 ms**.

### Typical agent loop

```text
Turn 1  POST /v1/search {"query": "..."}                  -> hits carry documentId, section, page
        (the agent keeps documentId in its own conversation state)
Turn 2  POST /v1/search {"query": "...", "documentIds": ["<id>"]}   -> focused follow-up
        GET  /v1/documents/<id>/structure                           -> where things are
        GET  /v1/documents/<id>/content?section=s3                  -> read it in full
        GET  /v1/documents/<id>/content?cursor=<nextCursor>         -> until truncated=false
```

The server is **stateless**: it does not remember a conversation. The calling app or agent keeps
the `documentId` between turns and decides when to dig deeper.

Every one of these routes applies the same visibility check as search (tenant, knowledge base,
access list). An agent cannot read a document the user is not allowed to see, even if it knows
the id.

### Stateless mode (`/v1/process`)

For apps that run their own vector store, `/v1/process` now also returns:

- the same outline v2 (`sections[]`, `outlineVersion: 2`);
- per-chunk `charStart` / `charEnd` / `overlapPrefixLen`;
- `pages[].pageSha256`, so an app can re-process only the pages that changed.

`/v1/encode` and `/v1/rerank` are tested to give exactly the same results as full mode.

---

## 4. MCP endpoint for AI agents (opt-in)

- Set `KB_MCP=on` to enable `POST /mcp` on the same port, with the same auth and `kb:read` scope.
  This is the **only** new feature that is off by default; with the flag off, `/mcp` answers 404.
- Five tools: `kb_search`, `kb_list_documents`, `kb_outline`, `kb_read` and `kb_query_table`.
  Each calls the same handler as its `/v1` route, so scope, versions and `degrades` behave
  identically.
- Transport: plain JSON-RPC 2.0 over HTTP, with no sessions and no SSE. An API error comes back as
  a tool result with `isError: true`.

This lets Claude, Cursor or any other MCP client use the knowledge base directly, with no glue
code.

---

## 5. Document versions

- **Upload a new version.** Upload or PATCH with `docKey` (a stable id for "the same document"),
  `version`, `effectiveFrom` and `effectiveTo`. In one transaction, the previous version is
  retired (`isLatest=false`) and the new one links to it with `supersedesDocumentId`.
- **Search defaults to current versions only.** Two options change that:
  - `asOf: "2024-06-01"` searches the versions that were in force on that date;
  - `includeHistory: true` searches all versions.
- **Mixed versions are flagged.** An answer that mixes versions of one `docKey` carries
  `multiVersionWarning`.
- **Reading by id still works for old versions.** `/content` and `/structure` read an older
  version when you ask for it by id.
- **Nothing changes for unversioned documents.**

---

## 6. Per-document access lists (ACL)

- **Setting an ACL.** A document can carry `aclPrincipals`, for example
  `["user:42", "group:hr"]`. Set it with the `aclPrincipals` field on upload or with
  `PATCH /v1/documents/:id`.
- **Asserting principals.** Requests send `X-KB-Principals: user:42,group:hr`, or `principals`
  in the search body. The key needs the **privileged `kb:assert-principals` scope**; without it,
  the request gets a `403`.
- **Visibility rule:**
  - a document with an ACL is visible only when the request's principals overlap the list;
  - with no principals asserted, it is hidden (**default-deny**);
  - a document with an empty ACL behaves as before.
- **Where the rule applies:** in the Postgres visibility query, which every retrieval arm and
  every document and table route uses. This includes `GET /v1/tables`, which used to list every
  table in the tenant and now lists only those the caller can see.

The calling app is the identity authority. rantai-kb trusts the principals that an app key with
`kb:assert-principals` sends, so only give that scope to backends that authenticate their users.
See [`docs/security.md`](security.md).

---

## 7. Exact answers from tables

- Spreadsheet and CSV extraction now handles:
  - a preamble before the header (World Bank and similar exports);
  - two- and three-level headers, flattened to `Parent / Child`;
  - BPS column-number rows and source-note rows;
  - year headers, which stay text.
- Long numeric identifiers such as NIP and NIK stay text: summing them is refused, and counting
  distinct values works.
- Whole numbers come back as JSON integers.
- Exact-execution accuracy rose from **0.521 to 0.995**.
- **Stored tables need `kb-server reindex` to pick this up.**

---

## 8. Scale and tenant isolation

- **Predicate scope** (`KB_SCOPE_ID_LIMIT`, default `0`, which means off). For tenants with many
  documents, search filters Qdrant by tenant and knowledge-base payload instead of sending every
  document id. Every candidate is then **re-verified against Postgres**, and the verify fails
  closed:
  - a failed verify fails the search;
  - dropped candidates are counted in `stats.scopeVerifyDropped`.
- **Scope outbox.** Document create and delete, knowledge-base membership changes and
  knowledge-base deletes are queued in the same Postgres transaction. A background worker then
  syncs the Qdrant payload. `/health` reports any lag as `scopeOutbox`.
- **`kb-server scope backfill`** gives old chunks their knowledge-base payload without
  re-embedding. Run it before you turn on `KB_SCOPE_ID_LIMIT`.
- **Faster scope SQL.** At 100k documents, the visibility query went from 194 ms to 0.7 ms and
  the ACL check from 26 ms to 0.12 ms, thanks to a new index migration.
- **Leak testing.** `kb-server eval leak` covers every retrieval arm, plus `/content`,
  `/structure`, tables and `/mcp`. With `--stale-payload`, it also simulates documents that moved
  knowledge base, were soft-deleted, or changed ACL. Result: **0 leaks** in 778 probes.
- **Optional Qdrant quantization:** `KB_QDRANT_QUANTIZATION=scalar|binary`, applied only when a
  collection is created. At 1M vectors, scalar p95 is 4.9 ms against 10.8 ms for float32.
  Recall on real embeddings has not been measured yet, so leave it off unless you need it.

---

## 9. Upgrading from 0.8

Follow [`docs/rust-cutover.md` §13](rust-cutover.md). In short:

1. **PostgreSQL 14 or later** is required.
2. Run **`kb-server migrate`** to apply four additive migrations: document versions, the scope
   outbox, ACL principals, and the ACL tenant index.
3. Run **`kb-server reindex`** if you want any of the following. Reindexing rebuilds from the S3
   originals and uses local models, so it costs nothing in API fees.
   - byte-exact `/content` on documents ingested before 0.9 (without it they are still readable,
     with a `degrades` note);
   - the fixed table extraction;
   - the `lite` (e5-small) passage-prefix fix.
4. Optionally, run `kb-server scope backfill` and then set `KB_SCOPE_ID_LIMIT` (for example
   `2000`) for large tenants.
5. Optionally, set `KB_MCP=on`.

**Behaviour changes to check in your app:**

- `documentIds` searches now return `sources` and `context` in the standard format.
- Searches return only the latest version of each versioned document, unless you send `asOf` or
  `includeHistory`.
- Documents with an ACL are hidden from any request that asserts no principals.

---

## 10. Figures from PDF pages (1.0.1, 1.0.2)

Before 1.0.1, a PDF's figures were its embedded image objects. That missed vector diagrams
(function graphs, number lines, concept maps), which have no image object. It also missed the
labels and arrows drawn over a picture, and the local Xiaomi-OCR recipe returned no crops at all.

With `KB_LAYOUT_FIGURES=on`, every PDF page is rendered:
- the layout detector (`KB_OCR_LAYOUT_URL`, PP-DocLayoutV3) marks the `image` and `chart`
  regions, which are cropped from the render;
- a nearby `figure_title` becomes the caption;
- a picture repeated on three or more pages, such as an icon or a header, is dropped.

Each figure is described by the local VLM and indexed. It comes back in `sources` as
`chunkType: "figure"` with an `assetKey`, so an agent can show the picture. In a spot check (not a labelled
set), "gambar hukum Newton" returned the diagram itself. The ingest report is in `metadata.layoutFigures`.

**Run the detector on the GPU** next to the vLLM OCR server (1.0.2, `LAYOUT_DEVICE=cuda:0`).
It sees every page and is about 8× faster there than on the CPU. Measured on one NVIDIA GB10:
- the setup was Xiaomi-OCR-0 on vLLM, the detector, the Qwen3-VL-2B describer, the embedder
  and the reranker;
- three textbooks, 808 pages, all OCR'd, finished in 33 minutes;
- there was no CUDA error and no hang;
- the run gave 838 figures, 754 of them described and indexed, and dropped 409 repeated icons.

Run a progress watchdog on vLLM: an earlier run in that position hung
([`rust/deploy/README.md`](../rust/deploy/README.md#the-layout-sidecar-xiaomi-ocr-0)).

For textbooks, set `KB_IMAGE_MAX_PER_DOCUMENT=0`. The default keeps only the first 100 figures
of a document in page order, so later chapters would lose theirs.

---

## 11. Flags at a glance

| Env var | Default | Purpose |
|---|---|---|
| `KB_MCP` | `off` | Enable `POST /mcp` |
| `KB_SCOPE_ID_LIMIT` | `0` (off) | Turn on predicate scope above N visible docs |
| `KB_SCOPE_OUTBOX_INTERVAL_MS` | `2000` | Outbox worker poll interval |
| `KB_MAX_PER_DOCUMENT` | unset (off) | Server-wide `maxPerDocument` default |
| `KB_CONFIDENCE_CALIBRATION` | unset | Path to a fitted `calibration.json` |
| `KB_QDRANT_QUANTIZATION` | `off` | `scalar` / `binary`, new collections only |
| `KB_RERANK_BREADCRUMBS` | `off` | Measured with no gain, so leave it off |
| `KB_LAYOUT_FIGURES` | `off` | Crop figures from rendered PDF pages (needs `KB_OCR_LAYOUT_URL`) |
| `KB_IMAGE_MAX_PER_DOCUMENT` | `100` | Figures kept per document, first in page order; `0` = all |
| `KB_GRAPH_ARM`, `KB_GLOBAL_ARM` | off | **Deprecated**; may be removed after 1.0 |

---

## 12. Known gaps (tracked, not yet done)

- **Predicate scope latency growth.** It still grows 8.1× from 1k to 100k documents; the target
  is 1.2×. The fix is a Qdrant tenant payload index (SEC-2e).
- **Disclosure reader.** Our scripted test reader fully reads the answer page in 52% of questions.
  In most misses the right page *was* among the search hits; the reader just didn't open it. A
  page-first reading strategy is next (EVAL-10). A real LLM agent may do better or worse.
- **Quantization recall** on real embeddings is unmeasured (RET-20).
- **Confidence calibration** ships without a built-in table, so `calibrated` stays `false` until
  you fit one.
