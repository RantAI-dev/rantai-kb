# dated-versions — synthetic labelled set for recency boosts (`RET-4`)

**This set is synthetic and author-generated.** It is not a benchmark anyone
else can reproduce from a public source; it exists to answer one question:
_when the same document exists in several dated editions, does a search put the
current edition first?_

Every family below is a short English policy, product or specification
document written in three editions (2023, 2024, 2025). The editions are
near-identical in wording and differ in one fact, e.g.

- `remote-work-allowance-v2023.md` — “Monthly remote work allowance: 50 EUR”
- `remote-work-allowance-v2024.md` — “Monthly remote work allowance: 75 EUR”
- `remote-work-allowance-v2025.md` — “Monthly remote work allowance: 100 EUR”

A retriever that only reads text has no reason to prefer one edition; the
three share a title, a topic and almost every sentence. The only signal that
says which one is current is the edition's date, which is why this set is the
instrument for a recency boost.

## Corpus

- **55 families × 3 dated editions = 165 documents**, plus 165 metadata
  sidecars.
- English, plain Markdown, about 60 words each.
- The 2023/2024/2025 editions of one family differ only in the fact value and
  the effective date.
- `metadata/<document>.json` is the upload metadata object:
  `{"effectiveDate": "2025-01-01", "authority": 0.9}`. `effectiveDate` is an
  ISO-8601 **date**. `authority` is present on every family at one of three
  values (0.9, 0.6 or 0.3) and is **constant across the three editions**, so an
  authority boost cannot accidentally decide which edition is current.

## Queries and qrels

- `queries.jsonl` (`{"_id", "text"}`; the format `kb-server eval` reads) and
  `queries.tsv` (`qid<TAB>query`; the same 55 questions for a human).
- `qrels/test.tsv` is three-column BEIR/TREC qrels
  (`qid<TAB>document<TAB>relevance`), the shape every other `kb-eval` set uses,
  with **only the 2025 edition marked `1`**. The older editions are unjudged
  and therefore score zero. That makes MRR@10 exactly “is the current edition
  the first result?” and recall@10 “is the current edition somewhere in the top
  ten?”.

The two files the runner reads are `queries.jsonl` and this three-column
`qrels/test.tsv` (optional `#` comments, no header row beyond the commented
one). A four-column TREC qrels (`qid 0 document rel`) needs a header row for
`kb-eval` to detect the columns, and `score_deltas.py` reads three columns
only, so this set stays in the repo's three-column format.

## Regenerating

```sh
python3 rust/crates/kb-eval/sets/dated-versions/generate.py
```

Python 3 stdlib only, no randomness, no network: the same bytes every run. The
generator rewrites `corpus/`, `metadata/`, `queries.jsonl`, `queries.tsv` and
`qrels/test.tsv`, and leaves this README alone.

## Running the measurement

The metadata sidecars are uploaded as the optional `metadata` form field of
`POST /v1/documents` (additive; a server older than `RET-4` rejects it), and the
boost is sent by the eval client's additive `--boost` flag. Both live in this
build; a stock older `kb-server` cannot run this set correctly.

Assume a running stack and a `kb:read,kb:write` key (`KB_EVAL_API_KEY`):

```sh
export KB_EVAL_API_KEY=…            # key for the tenant to measure
export BASE_URL=http://localhost:3000
DATASET=rust/crates/kb-eval/sets/dated-versions

# 1. Ingest + the OFF run. One document per corpus file, metadata sidecar
#    included; queries run with no boost in the body.
rust/target/release/kb-server eval \
  --base-url "$BASE_URL" --api-key "$KB_EVAL_API_KEY" \
  --dataset "$DATASET" --out eval-out/dated --label off --max-chunks 10
cp eval-out/dated/run.trec eval-out/dated/run.off.trec
cp eval-out/dated/metrics.json eval-out/dated/metrics.off.json

# 2. The ON run: same tenant and ingest map, no re-upload, boost on every query.
rust/target/release/kb-server eval \
  --base-url "$BASE_URL" --api-key "$KB_EVAL_API_KEY" \
  --dataset "$DATASET" --out eval-out/dated --label on --max-chunks 10 \
  --skip-ingest \
  --boost '{"recency":{"field":"metadata.effectiveDate","halfLifeDays":365,"weight":1}}'

# 3. Paired MRR@10 (and recall/nDCG) delta with a 95% bootstrap CI. The script
#    already computes mrr@10; no change was needed for this set.
python3 rust/crates/kb-eval/scripts/score_deltas.py \
  --qrels "$DATASET/qrels/test.tsv" \
  --baseline eval-out/dated/run.off.trec \
  --treatment eval-out/dated/run.trec
```

`score_deltas.py` prints one row per metric, including `mrr@10`: the off mean,
the on mean, the paired delta and its CI. The target in `docs/core/BACKLOG.md`
is **MRR@10 +0.05** on this set.

### Variants worth measuring

- **Gentler recency**: `"weight": 0.5` instead of `1`. With weight 1 the
  recency factor can drive every older edition to near zero; the boost is
  multiplicative, so the choice is a trade-off against the base relevance.
- **Authority as well**: add
  `"authority":{"field":"metadata.authority","weight":0.5}`. The qrels still
  target the newest edition, so this measures whether authority disturbs the
  recency signal.
- **Regression check**: the same `--boost` on `miracl-id-200` must not drop
  nDCG@10 by more than 0.005 (`RET-4`'s second target).

## What it cannot tell you

- It is not a natural reading of real policy documents: 165 documents is small,
  the wording is templated, and family names leak the topic.
- It measures **ranking only**. A search that returns the right edition second
  still fails the user's question; MRR@10 is the metric that reflects “right
  page first”, not answer correctness.
- The dates are fixed (2023–2025). A recency boost with a very short half-life
  and a measurement run years later would see almost no decay difference; the
  generator is deterministic so the ratio between editions stays one year, but
  the absolute age moves with the wall clock.
