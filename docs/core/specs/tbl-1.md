# TBL-1 — Tables you can ask questions of

## Why

A user asks "what was the total revenue in Jawa Timur in 2023?" and the answer is in a
spreadsheet or a table in a report. Today rantai-kb can only *find the text* of that table:

- xlsx is flattened to CSV (`kb-extract/src/xlsx.rs`);
- a markdown table becomes one unbounded chunk (`kb-core/src/chunk/smart.rs:165-190`);
- DOCX cells become loose paragraphs (`docx.rs:11-13`).

Nothing computes. A language model asked to add 40 numbers from flattened text gets it wrong,
and it sums subtotals and phone numbers with the rest.

paragon (`kb-bridge`) answers these questions in production:

- tables are kept as structured grids;
- a manifest knows quantity, code and identifier columns and the formula-proven subtotal rows
  (`tabel_manifes.py`);
- a parameterised `tabel_query` and `tabel_hitung` compute with receipts and privacy rules
  (`tabel_tool.py`).

The owner asked to close that gap (2026-11-14). The design is `docs/architecture-target.md` §10;
the contract is already in the code (`kb_core::tables`, `kb_ports::tables`, commit `e84b0e3e`).

The goal behind the task: **a calling app's agent can find the right table and get an exact,
explained number from it, inside the caller's tenant.** rantai-kb stays retrieval-and-compute;
the agent in the calling app does the talking.

## Work packages (parallel, one contract)

### A — Extraction, manifest, table-aware text (kb-core, kb-extract)

Grids, into `ExtractionResult.tables`:

- **xlsx / xls / ods (calamine).** One grid per table region of a sheet:
  - blank rows and columns split regions;
  - hidden sheets are skipped, with a note;
  - merged cells are spread across their span;
  - multi-level headers are flattened with " / ";
  - cached formula values are used, and rows whose numeric cells are formulas go into
    `formula_rows`;
  - locale-aware numbers: `1.234,5` → 1234.5 when the sheet uses comma decimals;
  - the title is a single merged title row above the header, else the sheet name.
- **CSV / TSV.** Sniff the delimiter; the header is the first row.
- **DOCX.** Tables in document order. Empty cells are **kept** (paragon drops them and shifts
  columns), `gridSpan` and `vMerge` are spread, and the title is the paragraph just above.
- **Markdown pipe tables** in any extractor's content (Mistral, MinerU, OCR router):
  - parse them into grids, titled by the line above;
  - **stitch** a table that continues on the next page when its header matches, or when the
    next page starts with a header-less table of the same width;
  - **drop furniture**: a table whose fingerprint (header plus first-row shape) repeats on
    ≥ 50% of the pages (letterheads, signature blocks).

**Manifest** (`kb_core::tables::build_manifest(&TableGrid) -> TableManifest`, pure):

- **Column kind.**
- **Role**:
  - Quantity: numeric, and not a code;
  - Code: digits with leading zeros, a fixed length ≥ 6 (NIK, phone numbers), or a header
    matching `kode|code|nik|npwp|telepon|phone|hp|no\.?`;
  - Date: a year column of integers 1900–2100 with a period-like header, or ISO dates;
  - Identifier: the first text column with high distinctness;
  - Label: everything else.
- **Unit** from the header, e.g. `(Rp juta)` or `%`; it is never applied to the values.
- **Up to 10 sample values** for non-quantity columns.
- **Derived rows**:
  - `formula_rows` → `formulas_known=true`, confidence High;
  - else label rows matching `^(total|jumlah|sub ?total|grand total|rata-rata|average)\b`
    (case-insensitive, any text column) → confidence Medium;
  - markdown or PDF tables, ragged rows, or stitching → Low.

**Text rendering**, the 4th retrieval unit in §10, replacing CSV/flat text in normal chunks:

- a simple table renders as markdown;
- a table larger than the chunk size is split into row groups, **each repeating the header**;
- a spreadsheet is no longer rendered as CSV.

### B — Store, query, ingest, API (new `kb-store-tables`, kb-engine, kb-server)

**Storage:**

- The grid is Parquet in the blob store at `tables/{tenant}/{document}/{tableId}/v{n}.parquet`.
  There is one column per header (typed by kind) plus a `_row` (0-based) column and a
  `_derived` bool column.
- The record goes in a new Postgres table `kb_table`, via an **additive** migration in
  `prisma/migrations/`. Its id is `{documentId}_t{index}`. Two versions are kept.

**Query** (`TableStore::query`):

1. Validate against the manifest, and refuse with a `QueryError`:
   - unknown columns, listing the available names;
   - SUM/AVG over a non-Quantity column;
   - ungrouped plain columns;
   - privacy (`aggregate_only` forbids plain rows and MIN/MAX).
2. Compile to SQL over `read_parquet(…)`. Identifiers are quoted from the manifest's
   whitelist, and values are **bound parameters**. Derived rows are excluded unless
   `include_derived`.
3. Run it in **DuckDB**: in-memory, no external access beyond the one local Parquet file,
   `QUERY_TIMEOUT_MS` and `MAX_ROWS_RETURNED`.
4. Apply privacy: suppress groups smaller than `min_group`. When exactly one group is
   suppressed, also suppress the next-smallest (complementary suppression).
5. Fill the receipt: `rows_matched`, `excluded_derived_rows`, `suppressed_groups`, and warnings
   (the source, how derived rows were known, the confidence).

**Ingest** (kb-engine), after extraction:

- `build_manifest` for each grid, then `put_tables`.
- Index two units per table in the existing chunk index, with the tenant and document payload
  like every chunk:
  - `chunk_type=table_summary`: title, caption, sheet or page, the headers with kind, role and
    unit, and sample values;
  - `chunk_type=table_row`: `Header: value; …` for each row, for tables ≤ 60 rows
    (glossary/key–value promotion).
- Both carry `tableId` in metadata. A document with no tables is unchanged. The ingest fails
  only if the store fails, never because a table could not be parsed (that gets a note).

**API** (additive, tenant-scoped exactly like documents):

- `GET /v1/tables?documentId=` and `GET /v1/tables/{id}` return the record with its manifest.
- `POST /v1/tables/{id}/query` takes a `TableQuery` and returns a `QueryResult`. A 400 carries
  the `QueryError` JSON; a 404 is returned for another tenant's table.
- `/v1/search` hits with `chunk_type` `table_summary`/`table_row` carry `tableId` and the table
  title, so a caller can query without guessing. Search responses list
  `tables: [{tableId, title, documentId}]` for the documents hit.
- Hard delete removes the document's tables and their Parquet.
- `CHANGELOG.md`, `docs/rust-cutover.md` (additive) and an OpenAPI/README section.
- **The leak matrix** (`kb-server eval leak`) gets a table row: list, get, query and search
  units across tenants.

### C — `tables-v1`: a table-QA eval set and harness (kb-eval)

**Sources: public tables only.**

- BPS/data.go.id/World Bank xlsx and csv files: ≥ 20 files, Indonesian and English, with
  subtotal rows and code columns.
- ≥ 5 DOCX files with tables. If none can be found, generate them from the spreadsheets with
  python-docx.
- ≥ 5 PDFs whose tables come out as markdown from OCR. These are optional when no OCR server
  is up.

**Questions** are generated from each grid with **gold answers computed from the grid
itself**, as paragon's seeded A/B does. Templates:

- a cell lookup;
- a sum or average with a filter;
- a count;
- a top-k;
- a group maximum;
- "total excluding subtotals": the trap where summing the flattened text would include the
  "Jumlah" row.

The set has ≥ 300 questions, in Indonesian and English. Each record holds the question, the
source file, the gold `TableQuery`, the gold answer and a tolerance.

**Three scores**, through the live HTTP API:

1. **Table retrieval.** Is the right `tableId` in the top 5 of `/v1/search` for the question?
   No LLM is involved.
2. **Execution.** Does the gold `TableQuery`, run through `POST /v1/tables/{id}/query`, return
   the gold answer? This tests extraction, the manifest (subtotals excluded, codes refused) and
   DuckDB.
3. **End to end.** An LLM agent gets the question and the `/v1/search` result, and then either:
   - (a) **today's path**: answer from the retrieved text chunks; or
   - (b) **the table path**: pick a table and write a `TableQuery`, call the API, answer.

   Score exact-match within tolerance. Use a free or local model. No paid run without the
   owner's go-ahead (AGENTS.md rule 7).

## Targets

| What | Target |
|---|---|
| Table retrieval (score 1) | right table in top 5 ≥ 0.85 |
| Execution (score 2) | ≥ 0.98 correct; the subtotal-trap questions ≥ 0.98 |
| End to end (score 3) | (b) − (a) ≥ +0.30 absolute, and (b) ≥ 0.80 |
| Retrieval elsewhere | MIRACL-id and hotpot nDCG@10 not lower than ING-6 (CI overlaps or above) |
| Isolation | leak matrix: the table row PASS, 0 leaks |
| Query latency | p95 < 300 ms on a 10k-row table, warm |
