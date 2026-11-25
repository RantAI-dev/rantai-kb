# tables-v1 — can a table be found, and then answered exactly?

The eval set for `TBL-1` (`docs/core/specs/tbl-1.md`, work package C). It grades three things
through the live HTTP API: **table retrieval** (does `/v1/search` surface the right table in the
top 5?), **execution** (does the gold `TableQuery` through `POST /v1/tables/{id}/query` return
the right number?) and, optionally, **end to end** (does an agent answer better from a table
than from flattened text?).

Built by `rust/crates/kb-eval/scripts/import_tables.py`; scored by
`rust/crates/kb-eval/scripts/score_tables.py`. Unit tests live in
`rust/crates/kb-eval/scripts/test_score_tables.py`.

| | |
|---|---|
| Files | 30: 25 xlsx/csv, 5 DOCX (generated from five spreadsheets, marked `generated`) |
| Questions | 386: 193 Indonesian, 193 English; about half the intents in both languages |
| Templates | cell lookup, filtered sum, count, top-k, group max, subtotal trap (26), code trap (15) |
| Sources | Satu Data Kabupaten Klaten (`opendata.klaten.go.id`, CC BY 4.0) and the World Bank Open Data indicator CSVs (CC BY 4.0) |
| Built with | seed 20261114, openpyxl / python-docx / stdlib, no code shared with the extractor |

## Why these sources

The Klaten portal is a Satu Data Indonesia CKAN instance that republishes BPS Klaten tables: real
spreadsheets with the properties that break a naive pipeline.

- **Subtotal rows.** `JUMLAH` at the bottom of a per-kecamatan table; the same number a flattened
  text dump invites a model to add twice. 26 questions, phrased as a plain total with no hint.
- **Code columns.** A six-digit `Kode Code` (kode wilayah, 331001…) and 18-digit `NIP` values.
  Asking for their *distinct count* catches a pipeline that sums them; asking for a total catches
  one that refuses to exclude them.
- **Multi-level headers.** `JUMLAH KELAHIRAN (JIWA)` over `LAKI-LAKI` / `PEREMPUAN` /
  `JUMLAH` / `PERSENTASE`, flattened to `A / B` so a column can be named exactly.
- **Year columns.** `Data | Satuan | 2018 … 2024`, and the World Bank shape
  `Country Name | Country Code | Indicator Name | Indicator Code | 1960 … 2024`.
- **A profile table** with names, degrees and NIPs but no quantity at all: a table that must
  still be retrievable and must never be summed.

The World Bank CSVs add English, a country-code column and long year series, and are 16 MB of
real data rather than a fixture. One file has no questions: `klaten-umkm.xlsx` (its first
column repeats section labels, so it has no row key). It stays in the corpus as a distractor;
29 of 30 files carry questions.

The DOCX files are **generated** from five of the spreadsheets with python-docx and marked
`generated: true` with their parent file in `sources.jsonl`. That is the spec's sanctioned path
when no public DOCX with the right tables can be found; their gold answers still come from the
grid, read back through python-docx, so DOCX extraction is graded, not assumed.

PDFs (`needs-ocr`) are **not** in this set: no OCR server was available to build a source that
would be marked `needs-ocr` honestly. `import_tables.py` already records the field; adding a PDF
source later needs no harness change.

## How the gold answers are made

Nothing imports the Rust extractor, the manifest builder or DuckDB. For each source:

1. openpyxl (`data_only=True`, plus a formula pass) or csv / python-docx reads the cells; merged
   cells are spread across their span.
2. The header is the row with the most labelled columns that has numbers below it; a merged
   title row is skipped. Multi-level headers flatten with ` / `, duplicates get ` (2)`.
3. The data region stops at a footnote (`Sumber:`, `Catatan:`) or three blank rows; a stacked
   second block whose first text column turns into bare years is dropped with a note.
4. A row is `derived` when its label is exactly `Jumlah`/`Total`/`Subtotal`/`Rata-rata` (or a
   `SUM` formula in the file). A trailing row that equals the sum of the rows above it is marked
   too, and that grid is then kept out of aggregate templates — the manifest cannot know such a
   row, so grading it would grade a guess.
5. Each template computes its answer in plain Python over those rows.

Then `--self-check` re-reads every source and runs each gold `TableQuery` through a separate
reference executor (`import_tables.execute`) and compares. Generation and validation are two
paths to the same number, so a gold answer that disagrees with its own grid fails the build.
The current set passes 386/386.

## Running it

```sh
# build (downloads ~30 public files into ~/kbdata/tables-v1/sources)
uv run rust/crates/kb-eval/scripts/import_tables.py --out ~/kbdata/tables-v1

# validate the gold answers against the grids
uv run rust/crates/kb-eval/scripts/import_tables.py --out ~/kbdata/tables-v1 --self-check

# unit tests for the comparator and the TableQuery JSON
cd rust/crates/kb-eval/scripts && python3 -m unittest test_score_tables -v

# score against a live server (ingest is resumable)
python3 rust/crates/kb-eval/scripts/score_tables.py \
    --dataset ~/kbdata/tables-v1 --base-url http://127.0.0.1:8080 --key "$KB_API_KEY"

# end to end, two arms, local model only (loopback unless --allow-paid)
python3 rust/crates/kb-eval/scripts/score_tables.py ... \
    --agent --llm-base-url http://127.0.0.1:8080/v1 --llm-model qwen2.5-7b-instruct
```

`score_tables.py --help` documents every flag. Outputs: `report.md` (tables, targets, failures),
`results.json` (the same numbers) and `per_question.jsonl` (both arms' answers, per question).

## What the harness scores

| Score | Question | Failure that must be visible |
|---|---|---|
| 1. Table retrieval | Is the gold `tableId` among the `tableId`s of the top 5 `/v1/search` hits? | A file with no `tableId` is an **extraction failure**, counted, never dropped |
| 2. Execution | Does the gold `TableQuery` through `/v1/tables/{id}/query` match the gold answer? | A 400 (`QueryError`), a 5xx, an empty result and a mismatch each count separately |
| 3. End to end | (a) answer from the search hits' text; (b) table summary → model writes a `TableQuery` → harness calls the API → model states the answer | An unparseable query, a rejected query or a model error is counted |

Every score is reported overall and per template / language / trap with a 95% bootstrap interval
(2000 resamples). Score 3 also reports the paired delta (b) − (a), which is the claim the spec
makes: tables beat flattened text by at least +0.30 absolute.

Grade 2 tolerates a **naming difference**: if the API rejects a gold column as unknown, the
harness retries once after folding case/whitespace/separators onto the manifest's names, and
counts `column_name_drift`. A drift that still answers correctly says extraction and DuckDB are
fine and the naming differs; a drift that answers wrongly is a real failure. Both are reported.

## Targets (`docs/core/specs/tbl-1.md`)

| What | Target |
|---|---|
| Table retrieval (top 5) | ≥ 0.85 |
| Execution | ≥ 0.98; subtotal traps ≥ 0.98 |
| End to end | table arm ≥ 0.80, (b) − (a) ≥ +0.30 |
| Query latency | p95 < 300 ms on 10k rows, warm |

The live numbers are produced by the reviewer after work packages A (extraction) and B (store and
API) merge; this directory holds the set and the instrument, not a result. No live run with a
model was made here, and none is claimed. **The 2026-11-25 live re-score is recorded in commit
`92b56aa8`** (`git show 92b56aa8:docs/eval-ab/tables-v1/README.md`): table retrieval 1.000,
execution 0.521, 180 `query_rejected: unknown_column`, code traps 0.133.

## TBL-2 — close the execution gap (2026-11-25)

**Instrument (offline, no server).** `kb-extract`'s `table_column_drift` example runs the same
extractor and manifest as `kb-server` over `~/kbdata/tables-v1/sources`, picks the grid the live
scorer would map to each question (sheet, index, then header overlap), and compares every column
name in the gold `TableQuery` with the manifest's names.

```sh
cargo run --manifest-path rust/Cargo.toml -p kb-extract --example table_column_drift -- \
  ~/kbdata/tables-v1
```

Before (commit `73b95406`, the instrument with the scorer's grid selection):

```text
files=29 extraction_failures=0 questions=386 gold_columns=715 queries_with_unknown_column=180
=== counts by cause (gold column occurrences) ===
csv_preamble_read_as_header: 229
data_row_read_as_header: 35
title_row_read_as_header: 68
TOTAL_MISMATCH_COLUMNS: 332
```

After (fixes in this branch):

```text
files=29 extraction_failures=0 questions=386 gold_columns=715 queries_with_unknown_column=0

=== counts by cause (gold column occurrences) ===
TOTAL_MISMATCH_COLUMNS: 0
```

The 180 offline rejections match the 180 live `query_rejected: unknown_column` failures exactly.
A second offline check runs all 386 gold queries through the dataset's own reference executor
(`import_tables.execute`) over the extracted grids: **370/386 = 0.940** after the naming
fixes; the remaining differences are row-boundary cases (footnotes, year blocks and integer
labels), not unknown columns. All 15 code traps now produce their gold distinct counts offline
(previously `Kode Code` was off by one on `q-0234`/`q-0235` because a `(1) (2) (3)` numbering
row was kept as data, and `NIP` was a `Quantity`/`Number` column).

**Live re-score (GPU box, reviewer).** Re-extract first — tables are stored, so the fix needs
`kb-server reindex`; then run the same command as the 2026-11-25 run:

```sh
python3 rust/crates/kb-eval/scripts/score_tables.py \
  --dataset ~/kbdata/tables-v1 \
  --base-url http://127.0.0.1:28080 --key "$KEY" \
  --out ~/v09m/results/m5-tables-tbl2
```

Report the failure counts (`query_rejected`, `execution_mismatch`, `column_name_drift`) and the
`template:code_trap` / `template:subtotal_trap` rows from `report.md`; `execution_remapped.all`
should now be 0. No live number is claimed here.
