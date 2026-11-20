# How wide should the reranked head be? (`KB_RERANK_MAX_K`, issue #31)

**Question.** Before #31 the engine kept only the top `KB_RERANK_FINAL_K` (5) candidates the
reranker had scored: those five were reordered and carried a `rerankScore`, and the rest of the
pool came back in the fused order **without a score**. #31 makes the width a setting
(`KB_RERANK_MAX_K`) and adds `stats.rerankedCount`. This page answers: **what should the default
be?**

**Answer: 10, for every tier.** The reranker's ordering beats the fused order to about ten and
no further; past that, widening the head costs nothing and gains nothing.

Everything below is measured on the bake-off's own artefacts — the same candidate pool and the
same cached reranker scores the 2026-11-17 bake-off used — so the only thing that changes
between the arms is the rule that puts the pool in order.

- Script: [`rust/crates/kb-eval/scripts/rerank_head_ab.py`](../../../rust/crates/kb-eval/scripts/rerank_head_ab.py)
- Raw output: [`head-ab-raw.txt`](head-ab-raw.txt) (140 lines, reproduced byte for byte below)
- The fused-list measurement: [`rust/crates/kb-eval/scripts/fused_rank_histogram.py`](../../../rust/crates/kb-eval/scripts/fused_rank_histogram.py),
  raw output [`fused-rank-raw.txt`](fused-rank-raw.txt)

## Method

- **Queries:** the 200 Indonesian MIRACL-id queries of
  `rust/crates/kb-eval/datasets/miracl-id-200`, with their qrels.
- **Pools:** two candidate pools from the bake-off (`/home/shiro/kfv-cache/abl-current/candidates.jsonl`,
  20 candidates per query, and `/home/shiro/kfv-cache/abl/candidates.jsonl`, 30 per query), each
  with the rerank scores the bake-off cached (`bakeoff-current/rerank-cache.jsonl`,
  `bakeoff/rerank-cache.jsonl`).
- **The pool is verified against the cache before anything is scored.** The cache key is
  `sha256(model, qid, query, documents…)` — the same function `rerank_bakeoff.py` uses — so a
  pool that does not hash to the stored key is a different pool and the stored `order` would
  point at the wrong documents. The script prints `pool vs cache keys verified: 200/200` and
  **stops** if it does not hold.
- **Arms:** `head K + fused tail` (today's rule: the top K by rerank score, then the rest of the
  pool in the order it arrived), `all by score` (the whole pool ordered by the rerank score),
  and `fused only` (the pool with no reranker at all, the reference).
- **Statistics:** paired bootstrap over the queries, 10,000 resamples, seed 42, 95% CI on the
  mean difference. nDCG@10 / MRR@10 / Hit@1 / Recall@10 use the same arithmetic as
  `rerank_bakeoff.py` (document level, a document judged by its best chunk).
- **Cost:** none. No GPU, no reranker, no paid API — the scores were already on disk.

## Result: the head is worth widening to 10, and no further

### 20-candidate pool, `voyageai/rerank-3`

| arm | nDCG@10 | MRR@10 | Hit@1 | Recall@10 |
|---|---:|---:|---:|---:|
| head 5 + fused tail (today) | 0.839 | 0.850 | 0.775 | 0.959 |
| **head 10 + fused tail** | **0.842** | **0.851** | 0.775 | **0.966** |
| head 20 + fused tail | 0.841 | 0.851 | 0.775 | 0.963 |
| all 20 by score | 0.841 | 0.851 | 0.775 | 0.963 |
| fused only (no rerank) | 0.801 | 0.797 | 0.685 | 0.955 |

Paired deltas against today's rule:

| arm | nDCG@10 Δ [95% CI] | Recall@10 Δ [95% CI] |
|---|---|---|
| head 10 + fused tail | +0.0028 [−0.0022, +0.0084] | +0.0070 [−0.0039, +0.0210] |
| head 20 + fused tail | +0.0014 [−0.0041, +0.0073] | +0.0037 [−0.0081, +0.0182] |
| all 20 by score | +0.0014 [−0.0041, +0.0073] | +0.0037 [−0.0081, +0.0182] |

### 20-candidate pool, `nvidia/nemotron-3.5-rerank-8b:free`

| arm | nDCG@10 | MRR@10 | Hit@1 | Recall@10 |
|---|---:|---:|---:|---:|
| head 5 + fused tail (today) | 0.837 | 0.847 | 0.770 | 0.956 |
| **head 10 + fused tail** | **0.847** | 0.848 | 0.770 | **0.980** |
| head 20 + fused tail | 0.846 | 0.848 | 0.770 | 0.979 |
| all 20 by score | 0.846 | 0.848 | 0.770 | 0.979 |
| fused only (no rerank) | 0.801 | 0.797 | 0.685 | 0.955 |

| arm | nDCG@10 Δ [95% CI] | Recall@10 Δ [95% CI] |
|---|---|---|
| head 10 + fused tail | **+0.0101 [+0.0030, +0.0182]** | **+0.0246 [+0.0078, +0.0449]** |
| head 20 + fused tail | +0.0094 [+0.0021, +0.0177] | +0.0230 [+0.0057, +0.0435] |
| all 20 by score | +0.0094 [+0.0021, +0.0177] | +0.0230 [+0.0057, +0.0435] |

### 30-candidate pools (the same two models, a deeper pool)

| model | arm | nDCG@10 | MRR@10 | Hit@1 | Recall@10 |
|---|---|---:|---:|---:|---:|
| voyageai/rerank-3 | head 5 + fused tail | 0.841 | 0.850 | 0.775 | 0.964 |
| voyageai/rerank-3 | **head 10 + fused tail** | 0.841 | 0.850 | 0.775 | **0.967** |
| voyageai/rerank-3 | head 20 + fused tail | 0.841 | 0.850 | 0.775 | 0.964 |
| voyageai/rerank-3 | all 30 by score | 0.841 | 0.850 | 0.775 | 0.964 |
| voyageai/rerank-3 | fused only (no rerank) | 0.818 | 0.831 | 0.740 | 0.959 |
| nvidia/nemotron-3.5-rerank-8b:free | head 5 + fused tail | 0.839 | 0.848 | 0.770 | 0.962 |
| nvidia/nemotron-3.5-rerank-8b:free | **head 10 + fused tail** | **0.844** | 0.848 | 0.770 | 0.972 |
| nvidia/nemotron-3.5-rerank-8b:free | head 20 + fused tail | 0.844 | 0.848 | 0.770 | 0.973 |
| nvidia/nemotron-3.5-rerank-8b:free | all 30 by score | 0.844 | 0.848 | 0.770 | 0.973 |
| nvidia/nemotron-3.5-rerank-8b:free | fused only (no rerank) | 0.818 | 0.831 | 0.740 | 0.959 |

The deltas at depth 30 are inside the noise for voyageai (nDCG +0.0005 [−0.0061, +0.0073] for
head 10, −0.0000 [−0.0073, +0.0074] for head 20) and small-but-positive for nemotron (nDCG
+0.0044 [−0.0026, +0.0116] for head 10, +0.0047 [−0.0023, +0.0118] for head 20). **Nothing
gains past 10.**

### What the numbers say

1. **The reranker is worth having** — the reranked head beats the fused order by +0.0385 nDCG@10
   (voyageai, depth 20; +0.0357 on nemotron) against today's head-5 rule. This is
   the reason the head exists at all, and it agrees with the live measurement in
   [`docs/eval-ab/README.md`](../README.md) (+0.037 nDCG@10, bge-reranker-v2-m3 on the RX 590).
2. **Widening the head to 10 is a small but real gain.** On nemotron it is significant
   (nDCG +0.0101 [+0.0030, +0.0182], Recall@10 +0.0246 [+0.0078, +0.0449]); on voyageai it is
   a tie on nDCG with a small Recall@10 gain (+0.0070 [−0.0039, +0.0210]).
3. **Past 10 nothing gains.** head 20 and "all by score" are the same numbers as head 10 on
   every pool, within the noise. The reranker's ordering stops beating the fused order at
   about ten — which is what the fused-list histogram below predicts.
4. **The head of 10 covers the default answer.** The default answer is 8 chunks
   (`KB_DEFAULT_MAX_CHUNKS`), so a head of 10 means **every returned chunk carries a
   `rerankScore`** by default. Under the old rule (head 5) only 5 of the 8 did, and a caller
   that decided grounding on the score had to treat the other 3 as irrelevant.

## Why 10 and not 20: where the relevant chunk actually sits

The head only matters for the candidates the reranker can move up. If the relevant chunk is
already in the fused top 5, the head was never the bottleneck.

[`fused_rank_histogram.py`](../../../rust/crates/kb-eval/scripts/fused_rank_histogram.py) measures
exactly that on the fused lists (raw output:
[`fused-rank-raw.txt`](fused-rank-raw.txt)):

```
== fused-old   (k60 fusion, pool 20)
   recall@5   0.960  (192/200)
   recall@10  0.990  (198/200)
   recall@20  1.000  (200/200)
   rank    1-5:  192 (0.960)
   rank   6-10:    6 (0.030)
   rank  11-20:    2 (0.010)

== fused-new   (k2 + parent fusion, pool 50)
   recall@5   0.950  (190/200)
   recall@10  0.990  (198/200)
   recall@20  1.000  (200/200)
   recall@30  1.000  (200/200)
   rank    1-5:  190 (0.950)
   rank   6-10:    8 (0.040)
   rank  11-20:    2 (0.010)
```

- **4 % of the queries (8 of 200) have their best relevant chunk between ranks 6 and 10.** The
  old head of 5 never scored those, so they were returned unscored and could not be grounded
  on. A head of 10 covers them.
- **2 of 200 (1 %) sit between ranks 11 and 20.** A head of 20 would catch them, but the
  measurement above shows that widening to 20 does **not** improve nDCG@10 or Recall@10 —
  the reranker does not move them up. Catching them is not the same as ranking them better.
- **Nothing sits below rank 20** in either list.

So the fused list is essentially decided by rank 10, and the reranker is decisive over roughly
the same window. Both point at 10.

## What it costs

**Nothing per query, and no extra spend.**

The head width does not change what is *sent* to the reranker — that is
`KB_RERANK_CANDIDATE_LIMIT` — it only changes how many of the scores the reranker already
produced are kept and used for the order. A cross-encoder (TEI, vLLM, Cohere) scores every
document it is sent; `final_k` only decided how many of those scores the response kept.
Widening the head therefore costs **zero extra compute and zero extra spend**.

Latency is driven by the send width, not by the head width. From the live numbers in
[`docs/eval-ab/README.md`](../README.md) (MIRACL-id, bge-reranker-v2-m3 on the RX 590):

| sent to the reranker | s/query |
|---|---:|
| 10 documents | 0.8 ([README.md:25](../README.md)) |
| 20 documents | 1.4 ([README.md:26](../README.md)) |

The head width does not appear in that table, and it cannot: the same 10 or 20 documents are
sent either way, and the same scores come back. `KB_RERANK_MAX_K=10` on a tier that sends 20
costs exactly what `KB_RERANK_FINAL_K=5` cost on that tier before.

**One-time cache invalidation.** The on-disk rerank cache keys on the ask width, so the first
run after this change re-runs the reranker for the widened ask and re-populates the cache. The
provider spend is the same (the same documents were sent either way), it is paid once.

## Per tier

Every tier ships the default `KB_DEFAULT_MAX_CHUNKS=8`, so a head of 10 covers the whole
answer everywhere. The tiers differ in how many candidates they *send*
(`KB_RERANK_CANDIDATE_LIMIT`), not in how wide the head should be:

| tier | `KB_RERANK_CANDIDATE_LIMIT` (sent) | reranker | recommended `KB_RERANK_MAX_K` |
|---|---:|---|---:|
| Lite | – | off | – (no reranker runs) |
| Desk | 10 | local cross-encoder | **10** |
| Studio | 20 | local cross-encoder | **10** |
| Station | 30 | local cross-encoder | **10** |
| Cloud | 20 | voyage reranker over OpenRouter (Cohere shape) | **10** |

The head can never be wider than what was sent (`head = min(max(final_k, max_k), sent)`), so
Desk's send width of 10 caps its head at 10 anyway.

**When to raise it:** if an operator raises `KB_DEFAULT_MAX_CHUNKS` above 10, the head should
follow, so the whole answer still carries a `rerankScore` — set `KB_RERANK_MAX_K` to at least
`KB_DEFAULT_MAX_CHUNKS`. The measurement above does not show a quality gain past 10; the reason
to go past it is coverage (every returned chunk scored), not ranking.

**`KB_RERANK_MAX_K=0` means no cap**: the head is the whole pool that was sent.

## The LLM reranker is a different animal

`KB_RERANK_PROVIDER=llm` asks a chat model to name the top passages and hands back **ranks**,
not relevance scores (`final_k − rank` for each pick). Two consequences:

- A wide ask would ask the model for a longer list (more output tokens, more spend) and would
  put a rank where a relevance score belongs.
- The engine therefore keeps the LLM reranker's head at `KB_RERANK_FINAL_K`. This is the
  `Reranker::scores_every_candidate()` capability in `kb-ports`: the cross-encoders report
  `true` (they score every document they are sent), the LLM reranker reports `false`.

The LLM reranker's quality under a wide head **could not be measured here** — it needs a paid
chat model and a labelled reranking set. The capability gate is what keeps that unmeasured
path unchanged.

## What was not measured, and what would confirm it

- **No live end-to-end run.** There is no GPU and no reranker model on this box, and a paid
  rerank API needs an approved budget. The A/B above is offline on cached scores: it answers
  *which order is better given the same scores*, which is the question #31 asks, but it does
  not run the server. **A live confirmation** would be: ingest the MIRACL-id corpus, run
  `kb-server eval` with `KB_RERANK_MAX_K=5` and with `KB_RERANK_MAX_K=10` on the same
  ingest, and compare nDCG@10 and Recall@10 — the same comparison
  [`docs/eval-ab/README.md`](../README.md) makes between "no rerank" and "bge top 20".
- **The reranker models here are the two the bake-off used** (`voyageai/rerank-3` and
  `nvidia/nemotron-3.5-rerank-8b:free`). A local Qwen3-reranker-0.6b (the tier default,
  `docs/eval-ab/rerank-2026-11-17/README.md`) was not in this comparison; its scores would
  have to be cached the same way to be added.
- **The LLM reranker path** is unmeasured under a wide head (above).
- **The 30-candidate pools at depth 30** were built before the 20-candidate pools, from a
  different fusion (k2 + parent). They are a second opinion, not a controlled comparison with
  the depth-20 pools; that is why every conclusion above is stated per pool.

## Reproducing

```bash
# The fused-list recall@k (the TREC run files come from the bake-off)
python3 rust/crates/kb-eval/scripts/fused_rank_histogram.py \
  --qrels rust/crates/kb-eval/datasets/miracl-id-200/qrels/test.tsv \
  --run "fused-old=/home/shiro/kfv-cache/abl-current/new_emb_text_k60_re-sort_pool_20.trec" \
  --run "fused-new=/home/shiro/kfv-cache/abl/new_k2_p_emb_text_p_pool_50_live_tie-break.trec"

# The head A/B (pool and cache come from the bake-off)
python3 rust/crates/kb-eval/scripts/rerank_head_ab.py \
  --candidates /home/shiro/kfv-cache/abl-current/candidates.jsonl \
  --cache      /home/shiro/kfv-cache/bakeoff-current/rerank-cache.jsonl \
  --queries    rust/crates/kb-eval/datasets/miracl-id-200/queries.jsonl \
  --model      "voyageai/rerank-3" \
  --qrels      rust/crates/kb-eval/datasets/miracl-id-200/qrels/test.tsv \
  --heads 5,10,20 --k 10
```
