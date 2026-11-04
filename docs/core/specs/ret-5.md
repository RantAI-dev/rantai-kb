# RET-5 — Adaptive arm choice

## Why

Every search today runs every configured arm: dense, BM25, page image, figure,
neighbour, graph and rerank. Most questions need only some of them. A part-number
lookup cannot be answered by a picture of a page, and an entity walk knows nothing
about `chapter 2`. Running those arms anyway costs a query embedding, a store round
trip and sometimes a model call, on every request.

The goal behind the task is not "classify queries"; it is **the same right page, sooner**.
A skipped arm is only acceptable if it cannot change which page comes first. So the
classifier is deliberately conservative: only a class with a clearly useless arm skips
anything, every skip is named in the response, and a plan that leaves nothing re-runs
the full plan once.

## Target

| Metric | Target | Set |
|---|---|---|
| p50 search latency | −20% with adaptive on | MIRACL-id-200 + ViDoRe v3 HR/CS mix |
| nDCG@10 | drop ≤ 0.005 | same mix |

Measurement is pending. The target is not met until these numbers are produced by the
commands below on a live stack; a compile-time test proves only behaviour, not speed.

## Class → arm plan

| Class | Runs | Skipped | Why |
|---|---|---|---|
| `exact` | dense, sparse, figure, neighbour, rerank | page image, graph | an ID/quoted phrase is matched literally by text; a page image or entity walk cannot match it |
| `factual` | every configured arm | — | no clear loser; the plan stays exactly today's pipeline |
| `structural` | dense, sparse, page image, figure, neighbour, rerank | graph | "chapter 2"/"page 12" is a place in a document; the page image and neighbours help, the entity graph does not |
| `relational` | every configured arm | — | "relationship between X and Y" is the graph arm's purpose |
| `aggregative` | dense, sparse, neighbour, graph, rerank | page image, figure | "main themes"/"list every" wants corpus coverage, not one page or one figure |
| `topical` | every configured arm | — | no clear loser; identical to today |

English and Indonesian cues are recognised (MIRACL-id is Indonesian). The classifier
is `kb_core::query_class`; the table is duplicated here and in its module docs on
purpose, so a reader of either can check the other.

## Response

Only when adaptive mode ran (`KB_ADAPTIVE_ARMS=true` or request `adaptiveArms: true`):

```json
"stats": {
  "queryClass": "exact",
  "skippedArms": [{ "arm": "pageImage", "reason": "exact query" }],
  "armPlanFallback": false
}
```

`skippedArms` is empty when the plan skipped nothing. After a fallback it is empty and
`armPlanFallback` is `true`, because the full plan did run. These fields are additive:
absent means "an old server or adaptive off", never "nothing was skipped".

## Measurement commands

Build once (`KB_ADAPTIVE_ARMS` is read at server start; the request flag overrides it):

```bash
CARGO_BUILD_JOBS=2 cargo build --manifest-path rust/Cargo.toml --release -p kb-server
```

Class distribution of a query file, using the pipeline's own rules (no server, no model):

```bash
rust/target/release/kb-server eval classify \
  --queries rust/crates/kb-eval/datasets/miracl-id-200/queries.jsonl
rust/target/release/kb-server eval classify \
  --queries /path/to/miracl-id/queries.tsv
```

A/B one binary on the same dataset, adaptive off then on, each on a fresh stack:

```bash
KB_AB_DATASET=rust/crates/kb-eval/datasets/miracl-id-200 \
KB_AB_ADAPTIVE_ARMS=false \
KB_AB_OUT=/tmp/ret5-miracl \
  rust/scripts/eval-ab.sh off=rust/target/release/kb-server

KB_AB_DATASET=rust/crates/kb-eval/datasets/miracl-id-200 \
KB_AB_ADAPTIVE_ARMS=true \
KB_AB_OUT=/tmp/ret5-miracl \
  rust/scripts/eval-ab.sh on=rust/target/release/kb-server
```

Repeat with `rust/crates/kb-eval/datasets/vidore3-hr-en` (and `vidore3-cs-en`) for the
ViDoRe half of the mix. Read p50 from `metrics.json` (`latency_ms.p50`) or, per query,
from `latency.json`; read nDCG@10 from `document.ndcg.mean` / `page.ndcg.mean`. Compare
with `--max-drop`:

```bash
rust/target/release/kb-server eval compare \
  --baseline /tmp/ret5-miracl/off/metrics.json \
  --current  /tmp/ret5-miracl/on/metrics.json \
  --max-drop 0.005
```

`KB_EVAL_ADAPTIVE_ARMS=true|false` does the same as the per-arm env above if you run
`kb-server eval` by hand. `--adaptive-arms` / `--no-adaptive-arms` force the request
field; omitting both lets the server's `KB_ADAPTIVE_ARMS` decide.

## Acceptance checklist

- [x] Classifier is pure, unit-tested, and handles EN + ID examples per class.
- [x] Off is the pre-RET-5 pipeline, proven by a test comparing on/off outcomes.
- [x] Exact skips page image + graph; the fakes prove the arms were not called.
- [x] Skipped arms are named in `stats.skippedArms`, with a reason.
- [x] An empty planned result falls back once and reports `armPlanFallback`.
- [x] Scope is unchanged for every arm that runs.
- [x] Eval runner passes the flag through and writes per-query `latency.json`.
- [ ] Live measurement: p50 −20% at ≤ 0.005 nDCG@10 loss (see commands above).
