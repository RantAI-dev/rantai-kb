# RET-17 — `maxPerDocument` on MIRACL-id-200 (measured 2026-11-25, GB10)

**Result. Target met, cap is inert on this set.** cap off → `maxPerDocument=3`
gives nDCG@10 **0.860655 → 0.860655**, Δ **+0.0000 [+0.0000, +0.0000]**
(≥ −0.005). Unique documents in the top 10 (mean over 200 queries) go
**9.4650 → 9.4700**. In every one of the 200 queries each document contributes
at most one chunk to the top 10, so a cap of 3 never binds; this is a no-op
measurement, not evidence that the cap preserves quality on a corpus where it
would bind.

## Method

Same checkout (`bench/v09-measure` @ `7d5adbdf`), release binary in
`~/kb-target-v09m`, native kb-server against `stack-up.sh`'s containers, the
already-running Nemotron at `127.0.0.1:8210` and the qwen3 reranker at
`127.0.0.1:8122` — details in [`../rerank-breadcrumbs/README.md`](../rerank-breadcrumbs/README.md).
The MIRACL corpus was **ingested once** for M1; M2 reused that ingest with
query-only runs and a server restart between arms:

```sh
source ~/v09m/stack-env.sh
bash ~/v09m/run-m2-variants.sh
# rust/scripts/eval-variants.sh base \
#   "cap-off=$RERANK_OFF" "cap3=$RERANK_OFF KB_MAX_PER_DOCUMENT=3"
python3 rust/crates/kb-eval/scripts/score_deltas.py \
  --qrels rust/crates/kb-eval/datasets/miracl-id-200/qrels/test.tsv \
  --baseline ~/v09m/results/m1-miracl/cap-off/run.trec \
  --treatment ~/v09m/results/m1-miracl/cap3/run.trec --resamples 2000 --seed 42
python3 ~/v09m/unique_docs.py ~/v09m/results/m1-miracl/<arm>/run.trec \
                              ~/v09m/results/m1-miracl/<arm>/ingest-map.json
```

`unique_docs.py` maps each top-10 TREC docid to its document UUID through the
ingest map, counts distinct documents per query, and averages. A second local
script counted the maximum chunks from one document in each top 10.

## Results

| | cap off | `maxPerDocument=3` |
|---|---:|---:|
| document nDCG@10 (mean, 200 queries) | 0.860655 | 0.860655 |
| unique docs@10 (mean) | 9.4650 | 9.4700 |
| max chunks from one doc in top 10 | 1 (all 200 queries) | 1 (all 200 queries) |
| failed queries | 0 | 0 |

Paired bootstrap: `ndcg@10 +0.0000 [+0.0000, +0.0000]`, n = 200. Recall@5,
recall@10 and MRR@10 deltas are also exactly 0.

Raw excerpt — `~/v09m/results/m1-miracl/m2-delta-cap.txt`:

```text
Evaluated 200 queries.
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.8085 | 0.8085 | +0.0000 | [+0.0000, +0.0000] | 200 |
| recall@10 | 0.9718 | 0.9718 | +0.0000 | [+0.0000, +0.0000] | 200 |
| mrr@10 | 0.8759 | 0.8759 | +0.0000 | [+0.0000, +0.0000] | 200 |
| ndcg@10 | 0.8607 | 0.8607 | +0.0000 | [+0.0000, +0.0000] | 200 |
```

Raw excerpt — unique docs:

```text
run=/home/rismana/v09m/results/m1-miracl/cap-off/run.trec
queries=200 mean_unique_docs@10=9.4650 min=5 max=10
run=/home/rismana/v09m/results/m1-miracl/cap3/run.trec
queries=200 mean_unique_docs@10=9.4700 min=5 max=10
```

Raw excerpt — eval summaries:

```text
eval cap-off: 200 queries (0 failed, gain linear) — document nDCG@10 0.861
eval cap3: 200 queries (0 failed, gain linear) — document nDCG@10 0.861
```

## Caveats

- Both M2 arms recorded `n_degraded = 200` with `page_arm_unavailable`:
  `eval-variants.sh`'s restart drops the explicit page-arm-off setting (see
  the RET-12 README). The same condition applied to both arms, so the paired
  delta is like-for-like; no page-image hits were present in either arm.
- The "unique documents" number is below 10 for some queries because those
  searches returned fewer than 10 chunks, not because of duplicate documents.
- Logs: `~/v09m/logs/m2-variants.out`; raw results in
  `~/v09m/results/m1-miracl/cap-off/` and `~/v09m/results/m1-miracl/cap3/`.
