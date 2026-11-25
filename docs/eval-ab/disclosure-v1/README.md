# EVAL-7 — progressive-disclosure coverage gate (measured 2026-11-25, GB10)

**Status: measured, two targets MISSED.** Coverage **0.238 [0.206, 0.269]**
(target ≥ 0.90) and `/content` p95 **60.4 ms** (target < 50 ms) both missed;
chars read / gold chars **1.353 [1.315, 1.387]** passed (≤ 1.5×). These are
live numbers on the ViDoRe HR corpus, reported as measured — not hidden.

## Why this exists

A calling agent needs the one page that answers a question, read through the API, without
re-searching and without blowing its context budget. Retrieval nDCG@10 says a relevant chunk was
ranked highly; it does not say the agent could actually *read the whole gold page* from the
reported hit. This gate measures the latter with an outside, human-labelled set: ViDoRe v3 HR
page qrels. The reader is a fixed script, with no LLM in the loop, so a number here is a property
of the API, not of a model.

## Method

For each question, in order:

1. `POST /v1/search {"query": …, "maxChunks": 5}`.
2. Group hits by `documentId`, keeping rank order. Fetch `GET /v1/documents/:id/structure` once
   per document (cached across the whole run) and take the first section whose page range
   contains the hit's page. With no such section, read the page window `[page-1, page+1]`
   (API pages are 0-based; qrels pages are 1-based).
3. `GET /v1/documents/:id/content` on that section or window, following `nextCursor` until the
   read is complete. A selection whose chunk range was already read is skipped. The reader stops
   when its budget of `1.5 ×` the gold pages' characters is used; `maxChars` is capped at the
   API's 8,000 default.
4. Separately, `GET /v1/documents/:id/content?pageStart=p&pageEnd=p` for every gold page gives
   the budget denominator. These calibration reads do **not** spend the budget and are **not**
   included in the latency samples.
5. Coverage for the question is the fraction of gold pages whose text appears, after collapsing
   whitespace runs to a single space, as an exact substring of the concatenation of everything
   the reader fetched. A gold page whose document is absent from the ingest map (`missing_doc`)
   counts as uncovered and is also counted separately. An empty gold page cannot be verified and
   counts as uncovered.

`results.json` holds mean coverage with a 95% percentile bootstrap CI (2000 resamples, fixed
seed), mean chars-read/gold-chars with its CI, totals, p50/p95 latency for each endpoint, the
question count and the `missing_doc` count. `per_question.jsonl` gives one record per question,
including which gold pages were covered and every visible error. `report.md` repeats the table
and prints the three targets with PASS/MISS. A MISS is reported, never hidden.

**Targets:** coverage ≥ 0.90 at ≤ 1.5× gold chars, and `/content` p95 < 50 ms. All three must pass.

## Running it against a local stack

Build the dataset first, once, from the pinned ViDoRe v3 revision (the PDFs are not committed):

```bash
cd rust/crates/kb-eval/scripts
uv run import_vidore.py --dataset vidore/vidore_v3_hr --language english \
  --out ../../../kb-eval/datasets/vidore3-hr-en
```

Bring up the stack, mint a key, ingest the corpus and only then run the disclosure harness. The
ingest step writes the `<doc-key> → documentId` map that the harness needs to find a gold page's
document; `--skip-ingest` on a second `eval` run reuses the same map without re-uploading.

```bash
cd <repo>
rust/scripts/stack-up.sh
. /tmp/kb-stack/stack.env
KEY="$(./rust/target/release/kb-server create-api-key disclosure disclosure \
  --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -n 1)"
DATASET=rust/crates/kb-eval/datasets/vidore3-hr-en
OUT=docs/eval-ab/disclosure-v1/run

# Ingest the 14 PDFs and produce $OUT/ingest-map.json.
./rust/target/release/kb-server eval \
  --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset "$DATASET" --out "$OUT"

# Measure the scripted reading loop over the same 318 human-labelled questions.
./rust/target/release/kb-server eval disclosure \
  --base-url "$KB_STACK_BASE_URL" --key "$KEY" \
  --dataset "$DATASET" --out "$OUT" --ingest-map "$OUT/ingest-map.json" \
  --top-k 5 --budget-factor 1.5
```

Use `--limit N` for a smoke subset (the bootstrap CI on a subset is not the gate). If the
ingest used a different output directory, point `--ingest-map` at that directory's
`ingest-map.json`, or pass `--out` of that ingest as the harness `--out`. The committed
`rust/crates/kb-eval/sets/aggregative/vidore3-hr-en/` subset also works: the harness auto-discovers
`queries.nonaggregative.jsonl` + `qrels.nonaggregative.tsv` when `queries.jsonl` is absent.

The four release gates are unchanged by this harness; it only reads `/v1/*` with a key and writes
files under `--out`.

## Results (2026-11-25)

Setup: branch `bench/v09-measure` @ `7d5adbdf`; release kb-server from
`~/kb-target-v09m`, run natively on the GB10 against the `stack-up.sh` containers;
Nemotron at `127.0.0.1:8210` embedded the HR corpus once for the RET-12 A/B
(no re-ingest here); qwen3-rerank-0.6B at `127.0.0.1:8122`; the page-image arm
was off and the search config was the shipped default (breadcrumbs off, no cap).
See [`../rerank-breadcrumbs/README.md`](../rerank-breadcrumbs/README.md) for the
stack wiring. The `--ingest-map` came from that single ingest.

Exact command (wrapper `~/v09m/run-m4-disclosure.sh`, log
`~/v09m/logs/m4-disclosure.out`):

```sh
source ~/v09m/stack-env-hr.sh
~/kb-target-v09m/release/kb-server eval disclosure \
  --base-url http://127.0.0.1:28081 --key "$KEY" \
  --dataset "$PWD/rust/crates/kb-eval/datasets/vidore3-hr-en" \
  --out ~/v09m/results/m4-disclosure \
  --ingest-map ~/v09m/results/m1-hr/base/ingest-map.json \
  --top-k 5 --budget-factor 1.5
```

| Measure | Value | Target | Verdict |
|---|---:|---|---|
| coverage (318 questions) | **0.238 [0.206, 0.269]** | ≥ 0.90 | **MISS** |
| chars read / gold chars | 1.353 [1.315, 1.387] | ≤ 1.5× | PASS |
| `/content` p95 | **60.4 ms** | < 50 ms | **MISS** |

Per-endpoint latency (ms, from `results.json`; `/structure` n = 14 cached
documents, `/content` n = 3839 reads, `/v1/search` n = 318):

| endpoint | p50 | p95 | p99 | n |
|---|---:|---:|---:|---:|
| `/v1/search` | 11.7 | 56.0 | 79.8 | 318 |
| `/v1/documents/:id/structure` | 13.5 | 41.7 | 50.6 | 14 |
| `/v1/documents/:id/content` | 15.4 | **60.4** | 118.9 | 3839 |

`missing_doc = 0`; bootstrap 2000 resamples. Raw stdout excerpt:

```text
disclosure: 318 questions, coverage 0.238 [0.206, 0.269], chars read/gold 1.353, content p95 60.4 ms
targets: coverage MISS · chars PASS · content p95 MISS
wrote results.json, per_question.jsonl, report.md
```

Raw artifacts: `~/v09m/results/m4-disclosure/{results.json,report.md,per_question.jsonl}`.
One question's raw line from `report.md`:

```text
| 0 | 0.333 | 2/6 | 0 | 10276 | 13460 | 0.763 |
```

No paid call was made. The MISS is reported, not hidden.
