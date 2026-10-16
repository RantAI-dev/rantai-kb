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

### MIRACL Indonesian baseline (`miracl-id-200`)

Built by `rust/crates/kb-eval/scripts/import_miracl.py` from the public MIRACL
benchmark:
- **Licence**: Apache-2.0
- **Sources**: Hugging Face `miracl/miracl` (topics and qrels for `id` dev split,
  commit `5be20db9509754dadad47689368639fcec739c00`) and `miracl/miracl-corpus`
  (corpus passages for `id`, commit `d921ec7e349ce0d28daf30b2da9da5ee698bef0d`).
- **Composition**:
  - 200 dev queries sampled with fixed seed 42.
  - 1,933 judged passages for those 200 queries (both relevant passages and
    human-judged non-relevant hard distractors).
  - 1,000 random unjudged passages from the Indonesian corpus as extra distractors
    (reservoir sampled with seed 42).
  - Total corpus: 2,933 markdown files in `corpus/<docid>.md` (docids sanitized by
    replacing `#` with `_`).
  - Qrels: 639 document-level judgements with grade 1 (non-relevant passages stay in the
    corpus as distractors and are omitted from `qrels/test.tsv`).

To build or rebuild the identical dataset:
```sh
uv run rust/crates/kb-eval/scripts/import_miracl.py --seed 42 --num-queries 200 --num-distractors 1000
```

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

# Burges' exponential gain instead of the linear one the leaderboards use
kb-server eval … --gain exponential
```

| flag | meaning |
|---|---|
| `--base-url` | the kb-server to measure (required) |
| `--api-key` | API key; defaults to `$KB_EVAL_API_KEY` (required) |
| `--dataset` | dataset directory (required) |
| `--k` | cutoff for the `@k` metrics (default 10) |
| `--gain` | `linear` (default) or `exponential` nDCG gain |
| `--max-chunks` | chunks requested per query (default 50) |
| `--out` | output directory (default `eval-out`) |
| `--skip-ingest` | reuse `<out>/ingest-map.json` instead of uploading |
| `--label` | run label written into every output (default `run`) |
| `--allow-failures` | exit 0 even with failed queries or corpus files |
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
| `metrics.json` | every metric with its 95% CI, the counters, latency, label, timestamp, provenance and dataset name |
| `report.md` | the same numbers as a table, for a human |
| `run.trec` | document-level TREC run (`qid Q0 docno rank score tag`) |
| `run.page.trec` | page-level TREC run |
| `ingest-map.json` | `doc-key → documentId`, reused by `--skip-ingest` |

The TREC files are the interoperability surface: `trec_eval` and `ranx` re-score
them, which is how this harness is checked against a tool that did not write it.
Because a document's rank here is the rank of its first chunk, the score column
encodes the rank (`1/rank`) rather than a similarity, so a re-scoring tool
reproduces the same ordering.

## Who produced the number

Every run records **two** independent identities, because they are two different
binaries:

| field | where it comes from |
|---|---|
| `harness_commit` | `git rev-parse` run by `build.rs` **at compile time**, baked into the binary. Never read from the working directory: a harness invoked from `/tmp` must still name the code that produced the number. |
| `server_version` | `GET /health` on the server under test (`KB_VERSION`, default `dev`). `null` if it does not answer. |
| `server_commit` | `null` — **no `kb-server` route exposes a build commit**. `/health` reports `version` only, and filling this in with the harness's commit would attribute one binary's identity to another. |

## How the numbers are computed

* **nDCG@k** — **linear** gain (`gain = rel`) with the `log2` discount and an
  `IDCG` from the sorted grades. Linear is what `trec_eval`'s `ndcg_cut`,
  `pytrec_eval`, BEIR, MTEB and the ViDoRe leaderboards compute, so a number
  from here can be put next to a public one. Burges' exponential gain
  (`2^rel − 1`) is behind `--gain exponential` for anyone reproducing the 2005
  paper; the two are not interchangeable — for qrels `a 1`, `b 3` with the run
  `a, b`, linear gives **0.797** and exponential **0.710**. The gain in force is
  written into `metrics.json` and `report.md`.
* **Recall@k**, **MRR@k**, **Hit@1**, **P@k** — a grade `> 0` is relevant;
  `P@k` divides by `k`, not by the number of results returned.
* A query with **no positive judgement at a level** is not evaluable there and
  is left out of that level's mean (as BEIR does) instead of being counted as a
  zero. The `n` column in `report.md` is the evaluable count; `n_queries` in
  `metrics.json` is how many queries were asked.
* A query whose search **fails** is counted in `n_failed` and scored as a **zero**
  at every level where it was evaluable — the same rule as a query that found
  nothing. The earlier rule excluded it, which quietly rewarded a backend for
  timing out on exactly the hard queries: drop them and the mean over the rest
  goes up.
* The **answered-only** mean (today's old behaviour) is reported next to it, in
  `metrics.json` under `document.answered` / `page.answered` and as a clearly
  secondary table in `report.md`. It is the diagnostic that separates "worse"
  from "down"; it is never the number to publish.
* `kb-server eval` **exits non-zero** when `n_failed > 0` or
  `n_ingest_failed > 0`. The outputs are written first either way, and
  `--allow-failures` turns the exit back to 0 for a CI job that wants both the
  numbers and the failure.
* A query answered with `degrades` (a retrieval arm was down) or with a server
  `note` is still scored, but `n_degraded`, `n_noted` and the labels themselves
  are published next to the metrics so a degraded number is never mistaken for a
  clean one.
* The 95% interval is a percentile bootstrap, 1000 resamples, fixed seed
  (`BOOTSTRAP_SEED`) — the same data always produces the same interval.
* Latency is client-side and covers answered queries only: mean, p50, p95, p99.

## The red-team leak matrix (`kb-server eval leak`)

```sh
rust/scripts/stack-up.sh
source /tmp/kb-stack/stack.env
KEY_A=$("$KB_SERVER_BIN" create-api-key leak-a a --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
KEY_B=$("$KB_SERVER_BIN" create-api-key leak-b b --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
kb-server eval leak --base-url "$KB_STACK_BASE_URL" --api-key "$KEY_A" --peer-key "$KEY_B"
```

It is the same black-box client as the quality harness, so the matrix re-runs
unchanged against the Qdrant + Postgres-graph backend.

**Seed.** 2 tenants × 2 knowledge bases × 2 categories = 8 documents, one per
(knowledge base, category) cell, each planted with its own canaries: a nonsense
token for BM25, a distinctive sentence for the vector arm, an invented proper
noun for entities, a figure caption, and a paragraph that is not the head of the
document for the neighbour weave. Canaries are derived from `--canary-seed`, so
a run is reproducible and two documents can never collide.

**Arms.** Each answer is attributed to an arm *from the wire*: `ChunkDto` marks a
BM25 hit by omitting `chunkType`, `page` and `assetKey`, and a figure hit by
carrying an `assetKey`. The entity arm is `GET /v1/documents/:id/intelligence`;
the figure arm also fetches `/v1/documents/:id/assets` when the target document
has a figure.

**Scopes.** `tenant` (no filter), `knowledge_base` (`knowledgeBaseIds`),
`category` (`category`), `document_ids` (`documentIds`). A document-addressed
route is judged against **tenant** visibility rather than the search filters,
because that is how the API authorises it — `knowledgeBaseIds` narrows a
*search*, it is not an access boundary for a document the caller already owns.

**Positive control (the point).** Every cell also runs the same canary from a
scope that *does* contain the target. A cell whose positive control never hits
is `UNTESTED`, never `PASS`: "zero leaks" from a probe that cannot find anything
proves nothing. A leak always wins over a missing control.

| status | meaning |
|---|---|
| `PASS` | no cross-scope hit, and the positive control found its own target |
| `LEAK` | at least one document outside the scope came back; the exit code is non-zero |
| `UNTESTED` | no leak was found, but the evidence is thin: no positive control, no out-of-scope probe, or the arm cannot answer in that scope at all |

Outputs: `leak-report.md` (the matrix, every leak with its request and response,
and a "what this run does not prove" section) and `leak.json`. Exits non-zero on
any `LEAK`, and with `--strict` on any `UNTESTED` too. `--keep` leaves the seeded
corpus behind; by default the documents, knowledge bases and categories it
created are deleted afterwards.

### Two keys, not one

API keys are minted per tenant by `kb-server create-api-key`; there is no
"admin key" that spans tenants. `--api-key` seeds the first tenant and
`--peer-key` a second one. Without the second key the cross-tenant row has
nothing to look for and is reported `UNTESTED` rather than passing — which is
the whole reason the positive control exists.

### What the stack cannot answer

* **Figures need a layout extractor.** `kb-extract` produces figure crops from
  the MinerU sidecar, `mineru-api` or Mistral. A markdown corpus has none, so on
  a plain stack the figure arm is `UNTESTED`. The report says so; it does not
  pass on an empty result.
* **`documentIds` bypasses hybrid retrieval.** `POST /v1/search` with
  `documentIds` takes the documentIds branch, which answers from the vector arm
  alone, so the bm25 and figure cells for that scope type are `UNTESTED`. That
  is a finding about the current implementation, recorded rather than papered
  over.

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
