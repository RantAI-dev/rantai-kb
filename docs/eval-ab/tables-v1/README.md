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
model was made here, and none is claimed. **See the dated re-score section below for the
2026-11-25 live measurement.**

## Live re-score — 2026-11-25 (GB10, `bench/v09-measure` @ `7d5adbdf`)

**Result: table retrieval is solved, exact execution is not.** Table-in-top-5
rose from the previous live run's **0.668** to **1.000** (386/386), but exact
execution is **0.521 [0.469, 0.570]** versus the v0.9.0 target **≥ 0.95**
(previous live run **0.417**). Subtotal-trap execution is **0.692** and code-trap
execution **0.133**. **Target MISSED**; reported as measured. No `--agent` arm
was run (no chat model), so the end-to-end numbers remain unmeasured.

Setup: release kb-server from `~/kb-target-v09m`, run natively on the host
against `stack-up.sh`'s containers on `127.0.0.1:28080`; the already-running
Nemotron at `127.0.0.1:8210` supplied embeddings (omniembed `/v2/embed`, 2048-d)
and the qwen3 reranker at `127.0.0.1:8122` was on with breadcrumbs off. Dataset:
`~/kbdata/tables-v1` (30 files, 386 questions). All 30 files ingested cleanly
(`ready: 30, failed: []`); 29 grids mapped to a `tableId`. No paid call.

Exact command (wrapper `~/v09m/run-m5-tables.sh`, log
`~/v09m/logs/m5-tables.out`):

```sh
python3 rust/crates/kb-eval/scripts/score_tables.py \
  --dataset ~/kbdata/tables-v1 \
  --base-url http://127.0.0.1:28080 --key "$KEY" \
  --out ~/v09m/results/m5-tables
```

### Scores (2000 bootstrap resamples)

| slice | n | table in top 5 | exact execution |
|---|---:|---:|---:|
| overall | 386 | **1.000 [1.000, 1.000]** | **0.521 [0.469, 0.570]** |
| template:cell_lookup | 122 | 1.000 | 0.574 [0.492, 0.664] |
| template:count | 55 | 1.000 | 0.455 [0.327, 0.582] |
| template:sum_filter | 62 | 1.000 | 0.500 [0.371, 0.629] |
| template:top_k | 67 | 1.000 | 0.507 [0.388, 0.627] |
| template:group_max | 39 | 1.000 | 0.538 [0.385, 0.692] |
| template:subtotal_trap | 26 | 1.000 | **0.692 [0.500, 0.846]** |
| template:code_trap | 15 | 1.000 | **0.133 [0.000, 0.333]** |
| lang:en | 193 | 1.000 | 0.518 [0.446, 0.591] |
| lang:id | 193 | 1.000 | 0.523 [0.456, 0.591] |
| trap:code | 15 | 1.000 | 0.133 |
| trap:subtotal | 26 | 1.000 | 0.692 |

Failures counted (never dropped): `query_rejected 180`, `execution_mismatch 5`.
Examples: `q-0075: unknown_column`, `q-0191: gold 6 got 5`, `q-0234: gold 26 got 27`.

Raw excerpt — `report.md` targets table:

```text
| table retrieval, top 5 | 0.85 | 1.000 |
| execution, all | 0.98 | 0.521 |
| execution, subtotal traps | 0.98 | 0.692 |
```

Raw excerpt — `results.json` overall scores:

```text
retrieval.all  n=386 mean=1.0000 [1.0000,1.0000]
execution.all  n=386 mean=0.5207 [0.4689,0.5699]
failure_examples: query_rejected ['q-0075: unknown_column', ...], execution_mismatch ['q-0191: gold 6 got 5', ...]
```

Raw artifacts: `~/v09m/results/m5-tables/{report.md,results.json,per_question.jsonl,ingest.json}`.
The 180 `unknown_column` rejections are the dominant failure class; the column
remapping retry helped 0 questions in this run (`execution_remapped.all = 0`).

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
(`import_tables.execute`) over the extracted grids:

```text
offline execution: 386/386 = 1.000
failures by template: {}
```

All 15 code traps now produce their gold distinct counts offline (previously `Kode Code` was
off by one on `q-0234`/`q-0235` because a `(1) (2) (3)` numbering row was kept as data, and
`NIP` was a `Quantity`/`Number` column). This is an offline check, not a live score; the
reviewer's live re-score below is the acceptance measurement.

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

## Live re-score after TBL-2 — 2026-11-25 (GB10, `bench/tbl2-quant` @ `aa3f8294`)

**Result: both targets met.** Table-in-top-5 stays **1.000 (386/386)** and exact execution
rises from the pre-TBL-2 live run's **0.521** to **0.995 [0.987, 1.000]** against the
v0.9.0 target **≥ 0.95**. Code-trap execution goes **0.133 → 1.000** and subtotal-trap
execution **0.692 → 1.000**. `query_rejected` and `column_name_drift` are both **0**
(the 180 `unknown_column` failures are gone); `execution_mismatch` is **2**. No `--agent`
arm was run (no chat model), so end-to-end numbers remain unmeasured.

Setup: a **fresh** stack `tq` (own ports 28090/25442/26343, own fresh `~/tq/cache` so every
table was re-extracted by the TBL-2 code) with a release `kb-server` built from this branch
into `~/kb-target-tq`, run natively on the host against `stack-up.sh`'s Postgres/Qdrant/RustFS
containers. Embeddings from the already-running `ret8-phase0-nemo35` Nemotron at
`127.0.0.1:8210` (omniembed `/v2/embed`, 2048-d); the qwen3-rerank-0.6b bakeoff unit was
started at `127.0.0.1:8122` for this run, breadcrumbs off, page arm off. No paid call.
All 30 files ingested cleanly (`ready: 30, failed: []`); 29 grids mapped to a `tableId`.

Exact command (wrapper `~/tq/run-m1-tables.sh`, log `~/tq/logs/m1-tables.out`):

```sh
python3 rust/crates/kb-eval/scripts/score_tables.py \
  --dataset ~/kbdata/tables-v1 \
  --base-url http://127.0.0.1:28090 --key "$KEY" \
  --out ~/tq/m1-tables
```

### Scores (2000 bootstrap resamples)

| slice | n | table in top 5 | exact execution |
|---|---:|---:|---:|
| overall | 386 | **1.000 [1.000, 1.000]** | **0.995 [0.987, 1.000]** |
| template:cell_lookup | 122 | 1.000 | 1.000 |
| template:count | 55 | 1.000 | 1.000 |
| template:sum_filter | 62 | 1.000 | 1.000 |
| template:top_k | 67 | 1.000 | 1.000 |
| template:group_max | 39 | 1.000 | 0.949 [0.872, 1.000] |
| template:subtotal_trap | 26 | 1.000 | **1.000 [1.000, 1.000]** |
| template:code_trap | 15 | 1.000 | **1.000 [1.000, 1.000]** |
| lang:en | 193 | 1.000 | 1.000 |
| lang:id | 193 | 1.000 | 0.990 [0.974, 1.000] |
| trap:code | 15 | 1.000 | **1.000** |
| trap:subtotal | 26 | 1.000 | **1.000** |

Failures counted (never dropped): `execution_mismatch 2`; `query_rejected` and
`column_name_drift` are absent from the failure map (0). Raw examples:
`q-0237: gold {'label': 'Prambanan', 'value': 28} got [['Bayat', 28]]`,
`q-0380: gold {'label': 'Bahrain', 'value': 100} got [['Gibraltar', 100]]` — both are
`group_max` label ties at the same value, not query rejections. The column-remapping
retry was never exercised (`execution_remapped.all = 0.000`), which is consistent with
`query_rejected = 0`.

Raw excerpt — `report.md` targets table:

```text
| table retrieval, top 5 | 0.85 | 1.000 |
| execution, all | 0.98 | 0.995 |
| execution, subtotal traps | 0.98 | 1.000 |
```

Raw excerpt — `results.json` overall scores and failures:

```text
retrieval.all  n=386 mean=1.0000 [1.0000,1.0000]
execution.all  n=386 mean=0.9948 [0.9870,1.0000]
failures: {'execution_mismatch': 2}
execution_remapped.all  n=386 mean=0.0000
```

Raw artifacts: `~/tq/m1-tables/{report.md,results.json,per_question.jsonl,ingest.json}`.
A first attempt that accidentally used the repo's default **debug** binary failed all
30 ingests on an embedding wire-shape mismatch; it is preserved at
`~/tq/logs/m1-failed-debug-bin.out` and `~/tq/results/m1-failed-debug-bin/` and is not
part of the numbers above.
