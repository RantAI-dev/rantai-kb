# Retrieval gaps: live results (2026-11-04)

The measurements behind backlog items `RET-2`, `RET-3`, `RET-4`, `RET-5`, `ING-3`,
`EVAL-2` and `EVAL-3`. Everything here comes from `feat/retrieval-gaps` built in
release mode and run on the cloud tier. That setup is the same as
`rk-cloud-vs-kf/run.sh`:

- voyage-multimodal-3.5 text and page embeddings;
- voyageai/rerank-2.5 reranking, 20 candidates;
- page rerank on;
- the `kb-smoke` stack (Postgres, Qdrant, RustFS).

The scripts that ran it are in the planner's working directory, not committed;
the commands are below. OpenRouter spend for the whole session was **$0.48**
(account usage $0.417 → $0.898).

## Defaults off: nothing regressed

| Set | Metric | v0.2.0 cloud (`rk-cloud-vs-kf`) | this build, all new options off |
| --- | --- | --- | --- |
| MIRACL-id-200 | document nDCG@10 | 0.845 | 0.845 (`miracl-id-200/rk-rg`) |
| ViDoRe v3 HR | page nDCG@10 | 0.710 | 0.711 (`vidore3-hr-en/rk-rg`) |

ViDoRe CS was not run. The run was stopped before CS to stay inside the spend
limit set for it, after the HR floor dumps cost more than estimated.

## `RET-2` relevance floor

`floor-calibration.txt` holds the full sweep. It was made with
`rust/crates/kb-eval/scripts/floor_calibrate.py` over raw responses saved by
`dump_responses.py`, with `minScore: 0`: the floor is on, nothing is dropped,
and every candidate is rerank-scored. It uses the rerank score.

| Set | Off-topic set | `minScore` | Off-topic empty | In-domain nDCG@10, no floor → floor |
| --- | --- | --- | --- | --- |
| MIRACL-id-200 (document) | `off-topic/miracl-id.tsv`, 55 | 0.42 | 90.9% | 0.8380 → 0.8381 |
| ViDoRe HR (page) | `off-topic/vidore-en.tsv`, 54 of 55 | 0.42 | 98.1% | 0.6398 → 0.6399 |

- **Absolute nDCG differs from the runner's.** The calibrator scores the top 10
  chunks it saved, so its absolute nDCG differs from the eval runner's. Only the
  change between rows is the measurement.
- **HR excludes one off-topic query.** HR off-topic query `q047` came back with
  `rerank_unavailable` and was excluded, not counted as empty.
- **The off-topic sets are author-written.** They lean on personal questions
  ("my nephew's game"), which are easier to reject than real off-topic traffic.
- **0.42 is a recommendation, not a default.** It is for this reranker only. It
  is not shipped as a default: `KB_MIN_SCORE` stays unset.

**Two bugs this run found, both fixed:**

- **The floor cut every answer to 5 hits.** The reranker returned only
  `rerank_final_k` (5) results, so every later hit had no score, and any floor cut
  the answer to 5 hits. With a floor set, every candidate is now scored, and the
  order stays the floorless one. Fixed in `9ca85e31`.
- **The calibrator was off by one page.** It compared 0-based API pages with
  1-based qrels pages.

## `RET-4` recency and authority boosts

| Set | Run | MRR@10 | nDCG@10 |
| --- | --- | --- | --- |
| `sets/dated-versions` (55 queries, 165 documents) | off → `recency effectiveDate, halfLife 365, weight 1` | 0.3455 → 0.9909 (+0.6455 [+0.6212, +0.6636]) | 0.5095 → 0.9933 |
| MIRACL-id-200 | off → `recency createdAt, halfLife 365, weight 0.3` | 0.8610 → 0.8610 | 0.8450 → 0.8451 (+0.0001 [+0.0000, +0.0002]) |

The dated set is synthetic. Its three editions of each document are nearly the
same text, so without the boost the order among them is close to a coin toss,
and the gain is largely there by construction. The MIRACL row is the no-harm
check.

## `RET-5` adaptive arm choice

The run was ViDoRe HR, 318 queries, query-only, with `adaptiveArms` off and on
(`vidore3-hr-en/adaptive-*`). The embedding and rerank caches were warm, so the
timings measure the engine's own work, not provider latency.

| | p50 | p95 | page nDCG@10 |
| --- | --- | --- | --- |
| off | 48.4 ms | 62.5 ms | 0.711 |
| on | 47.1 ms (−2.6%) | 61.2 ms | 0.710 |

Median latency by class, off → on:

| Class | Queries | Median latency |
| --- | --- | --- |
| exact | 24 | 47.1 → 24.6 ms |
| factual | 176 | 49.0 → 49.4 ms |
| topical | 83 | 48.4 → 49.7 ms |
| relational | 34 | 47.1 → 45.7 ms |
| aggregative | 1 | 30.7 → 17.0 ms |

**The −20% p50 target is missed.** The class plans are conservative: they skip
nothing for factual or topical questions. MIRACL-id-200 has no query in a class
that skips an arm (`kb-server eval classify`).

## `EVAL-2` tree index

The baseline is a live search on this build. The tree arm is
`scripts/tree_arm.py` over `sets/structural/outlines.json`, fused at weight 0.5.
"Gated" means only the queries that `kb-server eval classify --per-query` labels
`structural` (the production classifier).

| Set | Run | nDCG@10 | recall@10 |
| --- | --- | --- | --- |
| structural, HR part (41) | ungated | 0.5967 → 0.7594 (+0.1626 [+0.1113, +0.2212]) | +0.1695 |
| structural, HR part (41) | gated | 0.5967 → 0.6830 (+0.0863 [+0.0375, +0.1466]) | +0.0878 |
| regular ViDoRe HR (318) | ungated | 0.7109 → 0.5176 (−0.1933 [−0.2195, −0.1669]) | −0.0747 |
| regular ViDoRe HR (318) | gated | unchanged (no query labelled structural) | unchanged |

- **The question set is template-made.** It comes from the same outlines the arm
  reads, so this partly tests the tree against its own source.
- **CS was not run live.**
- **Building the index is free.** It makes no model calls: outline parsing takes
  3.5 s for 2,470 pages.

## `EVAL-3` graph community summaries

The baselines are the existing `rk-cloud-vs-kf/<ds>/rk-cloud/run.page.trec`.
There are 159 community summaries (communities with at least 2 pages), written by
deepseek/deepseek-v4-flash with reasoning off. They are in
`eval3/<ds>.summaries.json`, and the full output is in `eval3/results-llm.txt`.

| Corpus | Subset | Run | Δ recall@10 | Δ nDCG@10 |
| --- | --- | --- | --- | --- |
| HR | aggregative (66) | oracle gate | −0.0666 [−0.1139, −0.0210] | −0.1702 [−0.2252, −0.1158] |
| HR | aggregative (66) | lexical gate | −0.0446 [−0.0765, −0.0151] | −0.1194 [−0.1655, −0.0750] |
| CS | aggregative (53) | oracle gate | −0.1033 [−0.1648, −0.0490] | −0.2184 [−0.2757, −0.1635] |
| CS | aggregative (53) | lexical gate | −0.0907 [−0.1475, −0.0417] | −0.1616 [−0.2152, −0.1119] |

Summaries fused into search lower recall even on the broad questions they target.
The prototype selects communities by BM25 over the summary text, and its weight
was not tuned. A first summary run lost 95 of 159 summaries to reasoning tokens
eating the 320-token budget. The script now turns reasoning off and never caches
an empty answer.

## Live API checks (cloud stack)

| Check | Result |
| --- | --- |
| `metadata: {"effectiveDate": "2023-02-02"}` | only `annual-leave-v2023` |
| `metadata: {"authority": [0.6, 0.9]}` | only documents with authority 0.6 or 0.9 |
| `metadata: {"bad key": 1}` | 400 with the key rule |
| `externalRefs: []` | no results |
| off-topic query, `minScore: 0.42` | empty, with a note naming 14 dropped candidates, `floorScoreKind: rerank` |
| `GET /v1/documents/:id/structure` on a 111-page PDF | 111 pages with `hasTextLayer`, 619 chunks with pages, 0 figures (not extracted in this config), no `imageAssetKey` (page JPEGs are kept only with the vision rerank) |
| structure with another tenant's key | 404 |
| structure and `GET` with a key limited to another knowledge base | 404 and 404 |
| search with that limited key and a metadata filter | no results |
