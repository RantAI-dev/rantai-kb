# Confidence calibration (`RET-18`) — measured 2026-11-25 (GB10)

**Result: one of two targets missed.** The fitted table is
`calibration.json` in this directory (produced by the tool, not hand-written).
On the hold-out half, `band=none` was 24/28 = **85.7%** on off-topic (target
≥ 90% → MISSED) and 0/100 = **0.0%** on answerable (target ≤ 5% → met). The
table is therefore **not shippable**; it is committed as evidence, not installed
as a default. The binary still ships no built-in entries (the only earlier
committed measurement is the RET-2 floor `0.42` for `voyageai/rerank-2.5` in
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

## Measured run (2026-11-25)

Setup: branch `bench/v09-measure` @ `7d5adbdf`; release kb-server from
`~/kb-target-v09m`; native host server on `127.0.0.1:28080` against the
`stack-up.sh` containers; embeddings from the already-running Nemotron at
`127.0.0.1:8210` (omniembed `/v2/embed`, 2048-d); reranker
`Qwen/Qwen3-Reranker-0.6B` at `127.0.0.1:8122` with the bakeoff key. The MIRACL
corpus was ingested once for the RET-12 A/B and reused here (no re-index).
Every search answered with `scoreKind: rerank` and no `degrades`, which the tool
requires. See [`../rerank-breadcrumbs/README.md`](../rerank-breadcrumbs/README.md)
for the full stack wiring.

The off-topic TSV was converted to the tool's JSONL with the exact snippet
documented above under "Query sets":

```sh
python3 - <<'PY' > ~/v09m/offtopic-miracl.jsonl
import json
for line in open('rust/crates/kb-eval/sets/off-topic/miracl-id.tsv', encoding='utf-8'):
    qid, sep, text = line.rstrip('\n').partition('\t')
    if sep:
        print(json.dumps({'_id': qid, 'text': text}, ensure_ascii=False))
PY
# 55 lines written
```

Exact command (wrapper `~/v09m/run-m3-confidence.sh`, log
`~/v09m/logs/m3-confidence.out`):

```sh
source ~/v09m/stack-env.sh
~/kb-target-v09m/release/kb-server eval calibrate-confidence \
  --base-url http://127.0.0.1:28080 --api-key "$KEY" \
  --answerable "$PWD/rust/crates/kb-eval/datasets/miracl-id-200/queries.jsonl" \
  --offtopic ~/v09m/offtopic-miracl.jsonl \
  --out ~/v09m/results/m3-confidence \
  --holdout 0.5 --seed 42 --model-id Qwen/Qwen3-Reranker-0.6B
```

**Fitted thresholds** (`calibration.json`, committed here):

| modelId | scoreKind | low | medium | high |
|---|---|---:|---:|---:|
| `Qwen/Qwen3-Reranker-0.6B` | rerank | 0.42842302 | 0.70805687 | 0.9876907 |

Fit half: 100 answerable + 27 off-topic scores.

**Hold-out (50%, seed 42):**

| Set | Queries | band=none | Rate | Target | Result |
|---|---:|---:|---:|---|---|
| off-topic | 28 | 24 | 85.7% | ≥ 90% | **MISSED** |
| answerable | 100 | 0 | 0.0% | ≤ 5% | met |

Raw stdout excerpt:

```text
calibrate-confidence: rerank / Qwen/Qwen3-Reranker-0.6B: low 0.4284, medium 0.7081, high 0.9877
hold-out: off-topic none 85.7% (24 / 28, target ≥ 90%), answerable none 0.0% (0 / 100, target ≤ 5%)
wrote calibration.json, report.md and scores.jsonl to /home/rismana/v09m/results/m3-confidence
```

Raw artifacts: `calibration.json` (committed), `report.md`, `scores.jsonl`
(255 rows: 200 answerable + 55 off-topic; `scoreKind: rerank` throughout) in
`~/v09m/results/m3-confidence/`. The `band` field in `scores.jsonl` is
`unknown` because the server was not started with the calibration installed —
the tool evaluates the hold-out against the fitted table itself.

**First attempt failed and is not used:** with the page-image arm enabled by the
`KB_TIER=studio` preset after a restart, every search carried
`degrades: ["page_arm_unavailable"]` and the tool refused at query `17` with
`17: degraded search; refusing to calibrate on it`. The run above explicitly
appends `KB_PAGE_EMBEDDING_MODEL=''` after `stack-up.sh`'s page-env rewrite so
the page arm stays off, and all 255 searches were clean. No paid call was made.
