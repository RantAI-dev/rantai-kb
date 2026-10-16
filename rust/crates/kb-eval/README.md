# kb-eval — the retrieval-quality instrument

Nobody can say how good rantai-kb's search is until something measures it. This
crate is that something: it ingests a corpus through the public HTTP API, runs
every query, and writes standard IR metrics with bootstrap confidence intervals.
Every later change — a reranker, a different vector store, page images — is kept
or dropped on the number this produces.

It is a **black-box client**. It speaks HTTP to a running `kb-server` and links
no store, engine or inference crate, so the same harness measures any backend
behind the same API. `kb-server/src/boundary.rs` classifies it as an
adapter-side crate, which also means no inner crate may ever depend on it.

## Dataset format

A dataset is a directory in the standard BEIR/TREC layout, so a public
benchmark drops in unchanged:

```text
<dataset>/
  corpus/            the files to ingest; the file stem is the document key
  queries.jsonl      {"_id": "q1", "text": "..."} per line
  qrels/test.tsv     query-id <TAB> corpus-id <TAB> score
  answers.jsonl      optional {"_id": "...", "answer": "..."} — parsed, not yet scored
```

* **`corpus-id`** is `<doc-key>` (document-level) or `<doc-key>#p<N>`
  (page-level). `<N>` is **1-based**, as in a human-facing page citation.
* **`score`** is a graded relevance integer, 0–3. Anything `> 0` counts as
  relevant; grades above one change nDCG only.
* The header row is optional, tabs or spaces both work, and `#` comments and
  blank lines are ignored.
* `fixtures/tiny/` is a runnable example: six short documents (five `.md` plus a
  real two-page PDF), twelve queries, and qrels including two page-level
  judgements that only the PDF can answer.

## Running it

```sh
# against a running server: uploads the corpus, then queries it
KB_EVAL_API_KEY=… kb-server eval \
  --base-url http://localhost:3000 \
  --dataset rust/crates/kb-eval/fixtures/tiny \
  --out ./eval-out/tiny \
  --label baseline

# re-query without re-uploading (reuses <out>/ingest-map.json)
KB_EVAL_API_KEY=… kb-server eval --base-url … --dataset … --out ./eval-out/tiny --skip-ingest

# score a run file offline, with no server, to cross-check against trec_eval
kb-server eval score --qrels <dataset>/qrels/test.tsv --run ./eval-out/tiny/run.trec
```

| flag | meaning |
|---|---|
| `--base-url` | the kb-server to measure (required) |
| `--api-key` | API key; defaults to `$KB_EVAL_API_KEY` (required) |
| `--dataset` | dataset directory (required) |
| `--k` | cutoff for the `@k` metrics (default 10) |
| `--max-chunks` | chunks requested per query (default 50) |
| `--out` | output directory (default `eval-out`) |
| `--skip-ingest` | reuse `<out>/ingest-map.json` instead of uploading |
| `--label` | run label written into every output (default `run`) |
| `--timeout` / `--poll-ms` | per-job ingest timeout and job poll interval |

## What it does

1. **Ingest** — `POST /v1/documents` per corpus file, then poll
   `GET /v1/jobs/:id` until `success` or `failed`, with a timeout. Each
   `doc-key → documentId` pair is appended to `<out>/ingest-map.json` as it
   lands, so an interrupted run resumes instead of re-uploading. A file that
   fails is counted (`n_ingest_failed`) and the run continues.
2. **Search** — `POST /v1/search` per query, timing it client-side. The
   response's chunks are mapped back to dataset keys: document keys from the
   ingest map, page keys from the response's **0-based** `page`, converted to
   the **1-based** pages the qrels use. Chunks are collapsed to unique keys by
   first occurrence, because a document's rank is the rank of its best chunk.
3. **Score** — at document level and page level, with a fixed-seed bootstrap
   confidence interval per metric.

## Outputs, in `--out`

| file | what it is |
|---|---|
| `metrics.json` | every metric with its 95% CI, the counters, latency, label, timestamp, git commit and dataset name |
| `report.md` | the same numbers as a table, for a human |
| `run.trec` | document-level TREC run (`qid Q0 docno rank score tag`) |
| `run.page.trec` | page-level TREC run |
| `ingest-map.json` | `doc-key → documentId`, reused by `--skip-ingest` |

The TREC files are the interoperability surface: `trec_eval` and `ranx` re-score
them, which is how this harness is checked against a tool that did not write it.
Because a document's rank here is the rank of its first chunk, the score column
encodes the rank (`1/rank`) rather than a similarity, so a re-scoring tool
reproduces the same ordering.

## How the numbers are computed

* **nDCG@k** — exponential gain `2^rel − 1` with the `log2` discount,
  `IDCG` from the sorted grades.
* **Recall@k**, **MRR@k**, **Hit@1**, **P@k** — a grade `> 0` is relevant;
  `P@k` divides by `k`, not by the number of results returned.
* A query with **no positive judgement at a level** is not evaluable there and
  is left out of that level's mean (as BEIR does) instead of being counted as a
  zero. The `n` column in `report.md` is the evaluable count; `n_queries` in
  `metrics.json` is how many queries were asked.
* A query whose search **fails** is counted in `n_failed` and excluded from the
  means. A failed search is a failure, not a query that found nothing: it never
  becomes a zero.
* A query answered with `degrades` (a retrieval arm was down) or with a server
  `note` is still scored, but `n_degraded`, `n_noted` and the labels themselves
  are published next to the metrics so a degraded number is never mistaken for a
  clean one.
* The 95% interval is a percentile bootstrap, 1000 resamples, fixed seed
  (`BOOTSTRAP_SEED`) — the same data always produces the same interval.
* Latency is client-side and covers answered queries only: mean, p50, p95, p99.

## Trying it by hand

With a local server on `:3000` and an API key that has `kb:write` and
`kb:read`:

```sh
export KB_EVAL_API_KEY=<key>
kb-server eval --base-url http://localhost:3000 \
  --dataset rust/crates/kb-eval/fixtures/tiny \
  --out /tmp/kb-eval-tiny --label smoke
cat /tmp/kb-eval-tiny/report.md
```

`kb-server/tests/eval_tests.rs` runs the same harness end to end against the
real router with the in-process fakes behind it, so the loop is proven in CI
without a network or a database.