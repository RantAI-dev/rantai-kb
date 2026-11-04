# aggregative — the EVAL-3 instrument for "global mode"

**Status: instruments built, cost estimated, zero-cost variant measured. The
question itself is not answered yet.** Whether LLM community summaries help
broad questions needs one paid ingest pass; this directory makes that pass a
single command and makes its result scorable. No paid call has been made.

EVAL-3 asks: do graph community summaries ("global mode", as in GraphRAG /
moco-kf) help broad questions — "what are the main themes…", "list all the …" —
without hurting the extractive ones? The answer has to come from human labels,
not from a model grading itself, so the set below is cut from ViDoRe v3's
human page qrels and human `query_types`, before any run is scored.

## 1. The aggregative set

Definition, fixed in advance:

```text
aggregative := query_types contains "enumerative" or "multi-hop"
               AND the query has >= 2 positive gold pages
```

The `>= 2` condition is what makes answer-support recall non-trivial: with one
gold page, recall@10 is hit@10 and a summary cannot help by returning more of
the answer. The non-aggregative subset is every evaluated query in the dataset
that is not in the aggregative set; it is the guardrail.

Source: `docs/eval-ab`-independent ViDoRe v3 datasets, built by
`scripts/import_vidore.py` (CC-BY-4.0; pinned revisions in each dataset's
`PROVENANCE.txt`). Page ids are `<doc>#p<N>` with **N 1-based**, the same
convention as the qrels and the TREC runs.

| corpus | queries | enumerative | multi-hop | candidates before the ≥2-page filter | **aggregative** | non-aggregative | gold pages in the aggregative set |
|---|---|---|---|---|---|---|---|
| vidore3-hr-en | 318 | 61 | 30 | 79 | **66** | 252 | 395 |
| vidore3-cs-en | 215 | 42 | 23 | 60 | **53** | 162 | 310 |
| **total** | 533 | 103 | 53 | 139 | **119** | 414 | 705 |

119 aggregative queries is the ≥ 30 the backlog asked for, with four times the
minimum. Files per corpus: `qrels.aggregative.tsv`, `qrels.nonaggregative.tsv`,
`queries.aggregative.jsonl`, `queries.nonaggregative.jsonl`. `counts.json` holds
the same numbers machine-readably.

### The gate is a stand-in, and it is measured, not trusted

`rust/crates/kb-core/src/query_class.rs` (read 2026-11-04) classifies a query as
Natural or Exact (identifier / quoted phrase) for the relevance floor. It has
**no aggregative class** — that is RET-5, still `Idea`. There is therefore
nothing in the service to mirror for "broad question", and
`community_lib.is_aggregative_query` is a new lexical rule set (enumerative and
multi-hop cue phrases), not a port. `aggregative_subsets.py` reports its
agreement with the human labels:

| | precision | recall | accuracy | tp | fp | fn | tn |
|---|---|---|---|---|---|---|---|
| both corpora | 0.323 | 0.782 | 0.585 | 93 | 195 | 26 | 219 |
| vidore3-hr-en | 0.298 | 0.773 | — | 51 | 120 | 15 | 132 |
| vidore3-cs-en | 0.359 | 0.792 | — | 42 | 75 | 11 | 87 |

It finds most broad questions and is wrong about a third of the ones it fires
on. That is why the fuser has both `--gate aggregative` (what a RET-5
classifier of this quality would do) and `--gate list` (oracle gate, which
isolates the arm from the gate). Do not use the gate as a production answer;
the numbers say what a better classifier would be worth.

## 2. Communities built from the real corpora

`community_build.py` extracts page text with `pdftotext -f N -l N` (cached under
`~/.cache/kb-cache/pages/<dataset>/`), extracts entities with a Python port of
`kb_core::intelligence::heuristic` (the same zero-spend extractor
`KB_ENTITY_EXTRACTOR=heuristic` selects; the Rust `STOPWORDS` list is parsed
from `heuristic.rs` at run time so the two cannot drift), builds a weighted
page–entity co-occurrence graph, and clusters it with a deterministic Louvain
(node order sorted, ties to the lowest community id).

One preprocessing step matters and is reported: **hub stop-entities** —
entities on more than 5% of pages (`member states`, `eurostat`, `access`…) are
removed from the graph. Without that, "Member States" appears on 469 of 1087 HR
pages and joins every topic into one community (the first build produced exactly
1 community at modularity 0). They are listed under `hub_entities` in the JSON.

| corpus | pages | pages with entities | hub entities dropped | entities | co-occurrence edges | communities | with ≥2 pages | with ≥10 pages | 5 largest communities (pages) |
|---|---|---|---|---|---|---|---|---|---|
| vidore3-hr-en | 1110 | 1079 | 91 | 8,124 | 306,390 | 225 | 81 | 21 | 181, 93, 55, 49, 38 |
| vidore3-cs-en | 1360 | 1345 | 25 | 8,436 | 203,729 | 292 | 78 | 19 | 246, 148, 98, 79, 60 |

The median community is **1 page**: a surface entity extractor produces many
entities that appear on exactly one page. That is expected for this extractor
and is why the cost estimate below quotes both "every community" and "≥2
pages". A real LLM entity extractor would produce fewer, larger communities
(and cost more at extraction time — see §3).

`communities.json` (≈0.78 MB each) is committed because it is under 2 MB and is
the exact input to the cost estimate and the no-LLM measurement.

**Plumbing checks run, with output:**

- `community_build.py --selftest` → heuristic sample extracts 8 entities
  (`scott derrickson`, `los angeles`, …); two 5-node cliques → exactly 2
  communities; a second run gives identical labels.
- `global_fuse.py --weight 0` → reproduces the baseline ordering **and row
  count** on both corpora (verified query by query).
- `fused.oracle` on the non-aggregative subset → Δ = 0.0000 for every metric
  (fused queries are untouched, as designed).

## 3. Cost estimate: one summary per community, per 1,000 pages

The paid step is `community_summarise.py`: one chat completion per community.
The estimate below uses the **real `communities.json` from both corpora**
(2,470 pages), the real prompt texts built from cached page excerpts (12 pages ×
1,200 chars per community, `--max-tokens 320`).

- **Measured:** prompt characters (hence token counts at chars/4), number of
  communities, completion budget.
- **Looked up:** prices, from `GET https://openrouter.ai/api/v1/models` on
  **2026-11-04** (USD per 1M tokens):
  `deepseek/deepseek-v4-flash` **0.091 / 0.182**,
  `qwen/qwen3.7-flash` **0.030 / 0.130**,
  `google/gemini-3.8-flash` **0.825 / 4.125**.
- **Not measured:** the provider invoice. Token counts are chars/4 estimates,
  not tokenizer output; the invoice is the only measured cost.

Recommended configuration — summaries for communities with **≥ 2 pages**
(81 HR + 78 CS = 159 summaries for 2,470 pages):

| model | HR (1,110 pp) | CS (1,360 pp) | both | **USD / 1,000 pages** |
|---|---|---|---|---|
| deepseek/deepseek-v4-flash | $0.0199 | $0.0173 | $0.0372 | **$0.0151** |
| qwen/qwen3.7-flash | $0.0084 | $0.0074 | $0.0158 | **$0.0064** |
| google/gemini-3.8-flash | $0.2446 | $0.2185 | $0.4631 | **$0.1875** |

Measured token budget behind that table: **124,286 prompt tokens and 20,599
completion tokens per 1,000 pages** (306,986 + 50,880 over 2,470 pages).

Every community, including one-page ones (225 HR + 292 CS = 517 summaries):

| model | HR | CS | both | **USD / 1,000 pages** |
|---|---|---|---|---|
| deepseek/deepseek-v4-flash | $0.0348 | $0.0392 | $0.0740 | **$0.0300** |
| qwen/qwen3.7-flash | $0.0165 | $0.0195 | $0.0360 | **$0.0146** |
| google/gemini-3.8-flash | $0.4934 | $0.5863 | $1.0797 | **$0.4371** |

Token budget: 195,017 prompt + 66,980 completion tokens per 1,000 pages.

What the estimate excludes, stated so it is not mistaken for a full ingest cost:

- **Entity extraction at ingest.** The heuristic extractor is $0 (and is what
  built these communities). `community_build.py --extractor llm` is implemented
  and budget-gated but **untested** (no key here); a model per page would cost
  far more than the summaries — that is the larger number to get before
  choosing LLM extraction.
- **Query-time embedding of summaries.** 159 summaries for 2,470 pages is
  negligible next to any embedding bill; the prototype ranks summaries with
  BM25 instead, because that runs with no model and no key.
- **Retries and provider minimums.** Cache hits are free: every response is
  keyed by `sha256(model + prompt)` and a re-run reuses it, so an interrupted
  run never pays twice.

For scale: at **$0.0151 per 1,000 pages**, summarising a 100,000-page tenant
once costs about **$1.51** on deepseek-v4-flash and about **$18.75** on
gemini-3.8-flash. The decision in DEC-5 is therefore not mainly about money —
it is about whether the summaries improve retrieval.

## 4. Measured (zero-cost) result: the `--no-llm` variant

No `OPENROUTER_API_KEY` was set in this environment, so **no LLM summary was
generated**. What was measured is the zero-cost lower baseline:
`community_summarise.py --no-llm` writes each community's concatenated entity
names as its "summary", the arm ranks communities with BM25 and pages by
query–entity overlap, and `global_fuse.py` RRF-fuses the result with the
existing cloud baseline (`docs/eval-ab/rk-cloud-vs-kf/<dataset>/rk-cloud/run.page.trec`;
both page runs exist, so this needed **no new live search**).

Full exact output, all arms and both subsets: `results-no-llm.md`. Summary,
paired bootstrap over queries, 10,000 resamples, seed 42 (statistics imported
from `scripts/score_deltas.py`; recall@20 added in `score_aggregative.py`):

**Aggregative subset.** "on" is the treatment's absolute mean; Δ and CI are
treatment − baseline.

| corpus | n | arm | recall@10 on | Δrecall@10 [95% CI] | Δrecall@20 [95% CI] | ΔnDCG@10 [95% CI] |
|---|---|---|---|---|---|---|
| HR | 66 | global arm alone | 0.1756 | −0.4924 [−0.5795, −0.4050] | −0.5443 [−0.6335, −0.4540] | −0.5013 [−0.5820, −0.4197] |
| HR | 66 | fused, no gate | 0.5812 | −0.0868 [−0.1359, −0.0397] | +0.0013 [−0.0252, +0.0286] | −0.1479 [−0.2062, −0.0911] |
| HR | 66 | fused, gate | 0.5968 | −0.0712 [−0.1135, −0.0341] | −0.0132 [−0.0345, +0.0045] | −0.1120 [−0.1613, −0.0629] |
| HR | 66 | fused, oracle gate | 0.5812 | −0.0868 [−0.1359, −0.0397] | +0.0013 [−0.0252, +0.0286] | −0.1479 [−0.2062, −0.0911] |
| CS | 53 | global arm alone | 0.1904 | −0.5608 [−0.6448, −0.4771] | −0.6200 [−0.7055, −0.5318] | −0.5427 [−0.6235, −0.4625] |
| CS | 53 | fused, no gate | 0.6974 | −0.0538 [−0.0982, −0.0125] | −0.0370 [−0.0724, −0.0043] | −0.1351 [−0.1897, −0.0843] |
| CS | 53 | fused, gate | 0.7006 | −0.0506 [−0.0884, −0.0160] | −0.0212 [−0.0490, +0.0046] | −0.0994 [−0.1446, −0.0579] |
| CS | 53 | fused, oracle gate | 0.6974 | −0.0538 [−0.0982, −0.0125] | −0.0370 [−0.0724, −0.0043] | −0.1351 [−0.1897, −0.0843] |

Baseline absolute means (HR / CS): recall@10 **0.6680 / 0.7512**,
recall@20 **0.7769 / 0.8507**, nDCG@10 **0.6676 / 0.7588**.

**Non-aggregative subset (the guardrail).**

| corpus | n | arm | Δrecall@10 [95% CI] | Δrecall@20 [95% CI] | ΔnDCG@10 [95% CI] |
|---|---|---|---|---|---|
| HR | 252 | fused, no gate | −0.0821 [−0.1103, −0.0558] | −0.0361 [−0.0538, −0.0206] | −0.1737 [−0.2045, −0.1438] |
| HR | 252 | fused, gate | −0.0577 [−0.0803, −0.0379] | −0.0249 [−0.0393, −0.0126] | −0.0932 [−0.1179, −0.0695] |
| HR | 252 | fused, oracle gate | +0.0000 [0, 0] | +0.0000 [0, 0] | +0.0000 [0, 0] |
| CS | 162 | fused, no gate | −0.1032 [−0.1317, −0.0761] | −0.0460 [−0.0667, −0.0276] | −0.2000 [−0.2354, −0.1657] |
| CS | 162 | fused, gate | −0.0452 [−0.0655, −0.0272] | −0.0279 [−0.0449, −0.0129] | −0.0895 [−0.1186, −0.0633] |
| CS | 162 | fused, oracle gate | +0.0000 [0, 0] | +0.0000 [0, 0] | +0.0000 [0, 0] |

What this does and does not say:

- **It does not say global mode fails.** Entity names are not summaries. The
  arm built on them is a lower bound that loses to the baseline almost
  everywhere, and even an oracle gate does not rescue it on recall@10.
- **It does say the instrument is sensitive and the partition is usable.** Best
  3 communities cover **91% / 95%** of gold pages (oracle community selection),
  while BM25 over entity names selects communities covering only **29% / 33%**.
  The loss is in *selecting* communities, which is exactly the step a real
  summary is supposed to fix.
- **RRF at weight 0.5 with 40–100 global pages hurts ranking** even when
  recall@20 is flat: a page present in both arms collects both RRF terms and can
  jump above a gold baseline page. The weight and `--top-communities` were not
  tuned, deliberately — tuning them on these 119 queries and then reporting the
  same 119 would be self-grading.
- The gate itself leaks: 195 of the 414 non-aggregative queries (both corpora)
  are labelled aggregative by the lexical rule, so "gated" still loses nDCG@10
  on the guardrail. Its measured precision/recall is the number to improve in RET-5.

## 5. Runbook

Paths assume the repo root and the datasets in the main checkout (they are not
committed: 114 MB of PDFs). `$DS` is `vidore3-hr-en` or `vidore3-cs-en`.

```sh
REPO=$PWD
SCRIPTS=rust/crates/kb-eval/scripts
DATA=/home/shiro/rantai-kb/rust/crates/kb-eval/datasets
OUT=/tmp/opencode/eval3-runs
```

**0. Baselines.** No new live run is needed for this comparison:
`docs/eval-ab/rk-cloud-vs-kf/vidore3-hr-en/rk-cloud/run.page.trec` (21,822
rows) and `.../vidore3-cs-en/rk-cloud/run.page.trec` (15,377 rows) are the
rk-cloud page runs from 2026-10-31. If a fresh baseline is wanted anyway:

```sh
KB_EVAL_API_KEY=… kb-server eval --base-url http://localhost:3000 \
  --dataset "$DATA/$DS" --out "$OUT/$DS/rk-fresh" --label rk-fresh
# -> "$OUT/$DS/rk-fresh/run.page.trec"
```

**1. Rebuild the subsets (deterministic, no model).**

```sh
nice -n 19 python3 $SCRIPTS/aggregative_subsets.py \
  --dataset "$DATA/vidore3-hr-en" --dataset "$DATA/vidore3-cs-en" \
  --out rust/crates/kb-eval/sets/aggregative
```

**2. Build communities (zero cost, page text cached).**

```sh
nice -n 19 python3 $SCRIPTS/community_build.py \
  --dataset "$DATA/$DS" \
  --out rust/crates/kb-eval/sets/aggregative/$DS/communities.json
```

**3. Summarise — dry run first, always.**

```sh
nice -n 19 python3 $SCRIPTS/community_summarise.py \
  --communities rust/crates/kb-eval/sets/aggregative/$DS/communities.json \
  --dataset "$DATA/$DS" --model deepseek/deepseek-v4-flash \
  --min-pages 2 --max-usd 0 --dry-run --out "$OUT/$DS/summaries.json"
```

The dry run prints the per-community table and the total. Then, **with the
planner's approval**, a paid run (hard ceiling in USD; every response cached):

```sh
export OPENROUTER_API_KEY=…        # never echo it
nice -n 19 python3 $SCRIPTS/community_summarise.py \
  --communities rust/crates/kb-eval/sets/aggregative/$DS/communities.json \
  --dataset "$DATA/$DS" --model deepseek/deepseek-v4-flash \
  --min-pages 2 --max-usd 0.05 --i-have-approval \
  --out "$OUT/$DS/summaries.json"
```

A model id ending in `:free` costs nothing and needs `--i-have-approval` only
above 20 communities; a paid model always needs it. `--no-llm` writes the
zero-cost entity-name baseline instead.

**4. Arm and fuse.**

```sh
nice -n 19 python3 $SCRIPTS/global_arm.py \
  --communities rust/crates/kb-eval/sets/aggregative/$DS/communities.json \
  --summaries "$OUT/$DS/summaries.json" --queries "$DATA/$DS/queries.jsonl" \
  --out "$OUT/$DS/global.page.trec" --stats "$OUT/$DS/global.stats.json"

# what a RET-5 classifier of the measured quality would do
nice -n 19 python3 $SCRIPTS/global_fuse.py \
  --baseline docs/eval-ab/rk-cloud-vs-kf/$DS/rk-cloud/run.page.trec \
  --global "$OUT/$DS/global.page.trec" --queries "$DATA/$DS/queries.jsonl" \
  --gate aggregative --weight 0.5 --out "$OUT/$DS/fused.gated.page.trec"

# oracle gate: separates the arm from the gate
nice -n 19 python3 $SCRIPTS/global_fuse.py \
  --baseline docs/eval-ab/rk-cloud-vs-kf/$DS/rk-cloud/run.page.trec \
  --global "$OUT/$DS/global.page.trec" --queries "$DATA/$DS/queries.jsonl" \
  --gate list --query-list rust/crates/kb-eval/sets/aggregative/$DS/queries.aggregative.jsonl \
  --weight 0.5 --out "$OUT/$DS/fused.oracle.page.trec"
```

**5. Score both subsets.**

```sh
for subset in aggregative nonaggregative; do
  nice -n 19 python3 $SCRIPTS/score_aggregative.py \
    --qrels rust/crates/kb-eval/sets/aggregative/$DS/qrels.$subset.tsv \
    --baseline docs/eval-ab/rk-cloud-vs-kf/$DS/rk-cloud/run.page.trec \
    --treatment "$OUT/$DS/fused.gated.page.trec" --label "$DS $subset gated"
done
```

Read **recall@10 and recall@20** on the aggregative subset, then the
non-aggregative subset, then the CIs. A result that lifts the first while its
CI on the second excludes zero in the negative direction is not a result DEC-5
can use, whatever the point estimate says.

## 6. Files and limits

| file | what it is |
|---|---|
| `counts.json` | exact set counts + gate agreement |
| `<dataset>/qrels.*.tsv`, `queries.*.jsonl` | the frozen subsets |
| `<dataset>/communities.json` | the graph partition the cost and the no-LLM run used |
| `results-no-llm.md` | exact printed output of every measured arm |
| `scripts/community_lib.py` | heuristic extractor port, Louvain, BM25, TREC I/O, gate |
| `scripts/community_build.py` | page text → entities → communities |
| `scripts/community_summarise.py` | cached, budget-capped summaries; `--no-llm` |
| `scripts/global_arm.py` | community summaries → page run |
| `scripts/global_fuse.py` | RRF fusion, gate none/aggregative/list |
| `scripts/aggregative_subsets.py` | builds this set from ViDoRe labels |
| `scripts/score_aggregative.py` | recall@10/@20/nDCG@10 with CIs via `score_deltas.py` |

Limits, honestly:

- **No LLM summary was generated**: `OPENROUTER_API_KEY` was unset. The cost
  table is an estimate from real prompts, not an invoice.
- **The LLM entity-extraction path is untested** (same reason). Its cost is not
  in the table and would dominate it.
- **The arm uses BM25, not embeddings.** Production should embed summaries once
  and use the same model as the chunk index. The prototype deliberately avoids
  a model so the zero-cost measurement is possible.
- **The gate is not in `query_class.rs`.** It is a lexical stand-in with
  precision 0.32; RET-5 owns a real classifier.
- **The fusion configuration (k=60, weight 0.5, top 3 communities) was not
  tuned.** One configuration was measured to keep the set honest; the planner
  may pre-register a small sweep before the paid run.
- **Surface entities produce many one-page communities**; a real LLM extractor
  or a graph-aware merge step is likely to change community sizes and the cost
  per page.
- Two corpora, 2,470 pages, 119 aggregative queries. Results speak for these,
  not for every tenant.
