# RET-12 — reranker breadcrumbs A/B (measured 2026-11-25, GB10)

**Result. Do not ship `on`.** On MIRACL-id-200 the paired nDCG@10 delta is
**−0.0006 [−0.0104, +0.0102]**; on ViDoRe HR page nDCG@10 it is
**+0.0094 [−0.0062, +0.0250]**. Neither set reaches the decision rule
(Δ ≥ +0.01 with CI > 0 on at least one set, with no significant loss on the
other), and both intervals contain zero. No default was changed.

## Hardware, checkout and stack

- Box: ThinkStation NVIDIA GB10 (arm64, 20 cores, 128 GB unified). `free -g`
  before the build: `available 70`; `df -h /`: `72G` free.
- Branch `bench/v09-measure` cut from `origin/main` = `7d5adbdf`.
- Build (once): `CARGO_TARGET_DIR=$HOME/kb-target-v09m CARGO_BUILD_JOBS=8
  cargo build --release --manifest-path rust/Cargo.toml -p kb-server` →
  `Finished release profile ... in 10m 25s`. Full log: `~/v09m/logs/build.log`.
- Embedder: the **already running** `ret8-phase0-nemo35` Nemotron vLLM at
  `127.0.0.1:8210` (`nvidia/Nemotron-3.5-Embed-1B-BF16`, 2048-d), used via the
  DEP-7 wire shape (`KB_EMBEDDING_FORMAT=omniembed`, `/v2/embed`). It was never
  restarted. Docker containers cannot reach that loopback port, so kb-server
  ran **natively on the host** against `stack-up.sh`'s Postgres/Qdrant/RustFS,
  exactly as `origin/bench/dep-7:rust/scripts/bench-real.sh` does.
- Reranker: `~/llmhost/bakeoff/serve.sh up qwen3-rerank-0.6b` on
  `127.0.0.1:8122` (`serve.sh status`: healthy, 641 MiB). Configured as
  `LOCAL_RERANK_QWEN3` in `tier.rs`:
  `KB_RERANK_PROVIDER=cohere`, `KB_RERANK_MODEL=Qwen/Qwen3-Reranker-0.6B`,
  `KB_RERANK_BASE_URL=http://127.0.0.1:8122/v1/rerank`,
  `KB_RERANK_API_KEY=kb-eval-vlm-2026` (the bakeoff server's key; `local`
  answers 401). `/health` reports this backend `unavailable / HTTP 404`
  because the probe uses GET and llama.cpp answers GET on a POST route with
  404; a real POST `/v1/rerank` returns **200** and every search's server-side
  `rerank` stage is populated, so the reranker did run.
- Page-image arm was explicitly off (`KB_PAGE_EMBEDDING_MODEL=''`). Both text
  and page numbers therefore come from text chunks scored at page level, as in
  `docs/eval-ab/vidore3-hr-en/README.md`.
- The datasets are generated/not committed; symlinks were made from the
  already-present copies on this box:
  `rust/crates/kb-eval/datasets/miracl-id-200 -> ~/ret8/data/miracl-id-200`
  (200 queries, `qrels/test.tsv`), and
  `rust/crates/kb-eval/datasets/vidore3-hr-en -> ~/ret8/data/vidore3-hr-en`
  (318 queries, 14 PDFs, graded page qrels).

## Method

Each set was **ingested once** with `KB_RERANK_BREADCRUMBS=off`, then only the
search-time flag changed. The exact environment is in `~/v09m/stack-env.sh`
(MIRACL, ports 28080/25432/26333) and `~/v09m/stack-env-hr.sh` (HR,
28081/25433/26335); both source-able. Long jobs ran as `systemd-run --user
--unit v09m-* --property=KillMode=process` so the tmux-hosted server survived.

```sh
# base ingest + queries, one stack per set (KB_AB_KEEP=1, KB_AB_MAX_CHUNKS=10)
bash ~/v09m/run-m1-miracl-base.sh     # rust/scripts/eval-ab.sh base=$BIN
bash ~/v09m/run-m1-hr-base.sh
# same ingest, flag on: query-only (--skip-ingest), server restarted with the flag
bash ~/v09m/run-m1-miracl-variants.sh # eval-variants.sh base "bc-on=$RERANK_BC_ON"
bash ~/v09m/run-m1-hr-variants-recheck.sh
# clean re-run of MIRACL 'on' with the page arm explicitly off (no degrade)
ENVFILE=~/v09m/stack-env.sh OUT=~/v09m/results/m1-miracl-clean LABEL=bc-on-clean \
  DATASET=$PWD/rust/crates/kb-eval/datasets/miracl-id-200 OVERRIDES="$RERANK_BC_ON" \
  bash ~/v09m/run-query-arm.sh
# paired bootstrap CI over per-query nDCG (repo tool, 2000 resamples, seed 42)
python3 rust/crates/kb-eval/scripts/score_deltas.py \
  --qrels rust/crates/kb-eval/datasets/miracl-id-200/qrels/test.tsv \
  --baseline ~/v09m/results/m1-miracl/base/run.trec \
  --treatment ~/v09m/results/m1-miracl-clean/run.trec --resamples 2000 --seed 42
python3 rust/crates/kb-eval/scripts/score_deltas.py \
  --qrels rust/crates/kb-eval/datasets/vidore3-hr-en/qrels/test.tsv \
  --baseline ~/v09m/results/m1-hr/base-recheck/run.page.trec \
  --treatment ~/v09m/results/m1-hr/bc-on/run.page.trec --resamples 2000 --seed 42
```

## Results

| Set / metric | off | on | Δ [95% paired bootstrap CI] |
|---|---:|---:|---|
| MIRACL-id-200, document nDCG@10 | 0.860655 | 0.860101 | **−0.0006 [−0.0104, +0.0102]** |
| ViDoRe HR, page nDCG@10 | 0.588607 | 0.598043 | **+0.0094 [−0.0062, +0.0250]** |

The harness's own (unpaired, per-query bootstrap) CIs on the absolute means:
MIRACL off `0.860655 [0.836452, 0.884032]`, on
`0.860101 [0.837457, 0.883227]`; HR off
`0.588607 [0.556146, 0.620369]`, on `0.598043 [0.565533, 0.629630]`.
All four arms had `n_failed = 0`.

Raw excerpt — `metrics.json` means and the eval summary lines:

```text
eval base: 200 queries (0 failed, gain linear) — document nDCG@10 0.861, page nDCG@10 0.000
eval bc-on-clean: 200 queries (0 failed, gain linear) — document nDCG@10 0.860, page nDCG@10 0.000
eval base-recheck: 318 queries (0 failed, gain linear) — document nDCG@10 0.896, page nDCG@10 0.589
eval bc-on: 318 queries (0 failed, gain linear) — document nDCG@10 0.903, page nDCG@10 0.598
```

Raw excerpt — `score_deltas.py` (log `~/v09m/results/m1-miracl/m1-clean-delta.txt`):

```text
Evaluated 200 queries.
| Metric | Off (Baseline) | On (Treatment) | Delta | 95% Bootstrap CI | n |
|---|---|---|---|---|---|
| recall@5 | 0.8085 | 0.7938 | -0.0146 | [-0.0309, +0.0020] | 200 |
| recall@10 | 0.9718 | 0.9714 | -0.0004 | [-0.0051, +0.0037] | 200 |
| mrr@10 | 0.8759 | 0.8804 | +0.0045 | [-0.0115, +0.0220] | 200 |
| ndcg@10 | 0.8607 | 0.8601 | -0.0006 | [-0.0104, +0.0102] | 200 |
```

Raw excerpt — HR page delta (log `~/v09m/results/m1-hr/m1-page-delta.txt`):

```text
Evaluated 318 queries.
| ndcg@10 | 0.5886 | 0.5980 | +0.0094 | [-0.0062, +0.0250] | 318 |
```

## Decision rule

- MIRACL: Δ −0.0006, CI spans 0 → no gain, and no significant loss.
- HR: Δ +0.0094 < +0.01 and CI spans 0 → no qualifying gain.
- **Verdict: do not change the default.** `KB_RERANK_BREADCRUMBS` stays `off`.

## What happened / caveats

- The **first** HR base ingest run finished with 73/318 queries failed
  (`no HTTP response ...:28081/v1/search`): the host-side kb-server, started
  inside a plain `systemd-run` unit, was killed with that unit's cgroup partway
  through. Its numbers are **not used**. The baseline used here is a query-only
  re-run (`base-recheck`, 0 failed) against a server started under a persistent
  tmux session. Full failure lines are in `~/v09m/logs/m1-hr-base.out`.
- `eval-variants.sh` restarts kb-server via `stack-up.sh`, whose `write_page_env`
  strips `KB_PAGE_EMBEDDING_MODEL=''`, so the *variant* arms recorded
  `page_arm_unavailable` degrades (200/200 MIRACL, 318/318 HR) while the
  ingest-time base arms did not. The clean MIRACL re-run
  (`run-query-arm.sh`, 0 degrades) reproduced the same 0.860101; the HR off and
  on arms both carry the identical degrade, so their paired delta is still
  like-for-like. The server-side `rerank` stage was present in every arm.
- No paid API call was made. Logs: `~/v09m/logs/m1-miracl-base.out`,
  `~/v09m/logs/m1-miracl-variants.out`, `~/v09m/logs/m1-miracl-bcon-clean.out`,
  `~/v09m/logs/m1-hr-base.out`, `~/v09m/logs/m1-hr-recheck.out`,
  `~/v09m/logs/m1-hr-variants.out`; raw results in `~/v09m/results/m1-*/`.
