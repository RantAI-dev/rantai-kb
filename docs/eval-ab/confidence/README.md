# Confidence calibration (`RET-18`) — not run yet

**Results: not run yet.** The band code and the fitting tool are built; the live
fit needs a GPU box with the target reranker and the benchmark corpus loaded.
Nothing in this directory is a measured number. Do not install a hand-written
`calibration.json`: the binary ships **no** built-in entries because no
three-point fit is committed (the only committed measurement is the RET-2 floor
`0.42` for `voyageai/rerank-2.5` in
[`../retrieval-gaps/README.md`](../retrieval-gaps/README.md), which is one
off-topic rejection threshold, not a `high`/`medium`/`low` fit).

## What to fit

A caller decides "answer or say I don't know" from `confidence.band`. Raw scores
are not comparable across models, so a table is fitted per scoring model id:

- the **reranker** model id (`KB_RERANK_MODEL`) when the answer carries a
  `rerank` score;
- the **embedding** model id (`KB_EMBEDDING_MODEL`) for `dense`/`fused` answers.

Fit each model separately. Keep the server configuration identical for both
query sets and for both halves of the split.

## Query sets (already in the repo)

`RET-2` used two labelled sets; both are reused here:

- **Answerable:** `rust/crates/kb-eval/datasets/miracl-id-200/queries.jsonl`
  (200 Indonesian questions with `qrels/test.tsv`). The dataset directory is
  generated, not committed; generate it with
  `python3 rust/crates/kb-eval/scripts/import_miracl.py --help` if it is missing.
- **Off-topic:** `rust/crates/kb-eval/sets/off-topic/miracl-id.tsv` (55
  author-written unanswerable questions; see that directory's `README.md`).
  `vidore-en.tsv` is the page-level alternative; use it only with a page corpus.

Both tools read one `{"_id": "q001", "text": "..."}` per line. Convert the TSV
set (which is `id<TAB>text`) without editing the queries:

```sh
python3 - <<'PY' > /tmp/opencode/offtopic-miracl.jsonl
import json
for line in open('rust/crates/kb-eval/sets/off-topic/miracl-id.tsv', encoding='utf-8'):
    qid, sep, text = line.rstrip('\n').partition('\t')
    if sep:
        print(json.dumps({'_id': qid, 'text': text}, ensure_ascii=False))
PY
```

## Run

With the server up (same build on both halves), from the repo root:

```sh
kb-server eval calibrate-confidence \
  --base-url http://localhost:8080 \
  --api-key "$KB_EVAL_API_KEY" \
  --answerable rust/crates/kb-eval/datasets/miracl-id-200/queries.jsonl \
  --offtopic /tmp/opencode/offtopic-miracl.jsonl \
  --out docs/eval-ab/confidence/run-1 \
  --holdout 0.5 --seed 42 \
  --model-id voyageai/rerank-2.5
```

`--model-id` is optional: without it the tool resolves `KB_RERANK_MODEL` for
rerank answers and `KB_EMBEDDING_MODEL` otherwise from its own environment.
The tool refuses a run whose answers mix score kinds or model ids, and refuses
to fit on a degraded search (a failed search is not an empty result).

## Fitting rule

On the fit half only:

- `low` = the smallest observed off-topic top that leaves ≥ 90% of off-topic
  tops strictly below it (the 90th-percentile off-topic top). Picking the
  smallest qualifying score keeps the other target achievable: a larger floor
  would reject more off-topic traffic but also more answerable questions.
- `high` = the smallest answerable top whose precision (answerable tops ≥ t
  among all tops ≥ t) is ≥ 90%.
- `medium` = `(low + high) / 2`.

## Outputs

- `calibration.json` — one `{modelId, scoreKind, high, medium, low}` entry, in
  the exact format `KB_CONFIDENCE_CALIBRATION` accepts.
- `report.md` — fitted thresholds and the hold-out table.
- `scores.jsonl` — per-query top score, score kind, model, band and split.

Acceptance targets on the **hold-out** half: `band=none` on ≥ 90% of off-topic
queries and on ≤ 5% of answerable ones. Both rates are printed by the command
and written to `report.md` with `met`/`MISSED`. A missed target means the table
is not shippable: widen the query sets or change the score, do not move the
threshold by hand.

## Install an accepted table

```sh
export KB_CONFIDENCE_CALIBRATION="$PWD/docs/eval-ab/confidence/run-1/calibration.json"
```

The file is read and validated at startup (ordered, finite thresholds and a
non-empty model id); a bad file stops the process. After installing it, the
`confidence` object reports `calibrated: true` for a matching model and score
kind, and the caller can answer on `high`/`medium`.
