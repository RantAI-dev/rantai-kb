# Eval baseline — step 0 of `docs/plan-best-multimodal-kb.md`

The first numbers this repo has ever measured, and the exact commands that
produce them. Everything here was run live against a real Postgres 16,
SurrealDB v2, rustfs S3 and `kb-server`, by `kb-eval`. Reproduce with the
commands in each section; nothing in this file is a claim about code.

**These numbers measure plumbing, not quality.** The smoke stack runs
`stub_embeddings` (`rust/crates/kb-server/examples/stub_embeddings.rs`): a
deterministic hash of the input text at dimension 8, with no model, no network
and no cost. A hashed 8-dimensional vector is a lexical proxy, so the ranking
below is roughly "token overlap", not "semantic relevance". Read the first
table as *the harness and the pipeline work end to end over HTTP, and here is
what plumbing is worth* — not as a quality claim about retrieval.

## 1. Retrieval quality — Rust, `fixtures/tiny`

```sh
rust/scripts/stack-up.sh
source /tmp/kb-stack/stack.env
KEY=$("$KB_SERVER_BIN" create-api-key baseline-tenant baseline \
        --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
kb-server eval --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset rust/crates/kb-eval/fixtures/tiny --out /tmp/kb-eval-final \
  --label live-tiny
```

2026-10-16, harness commit `85e436e`, 6 documents (5 markdown + one two-page
PDF), 12 queries, 2 of them page-level judgements only the PDF can answer:

| level | n | nDCG@10 | Recall@10 | MRR@10 | Hit@1 | P@10 |
|---|---|---|---|---|---|---|
| document | 12 | 0.512 [0.348–0.694] | 0.833 [0.583–1.000] | 0.406 [0.246–0.604] | 0.167 [0.000–0.417] | 0.083 [0.058–0.100] |
| page | 2 | 0.500 [0.000–1.000] | 0.500 [0.000–1.000] | 0.500 [0.000–1.000] | 0.500 [0.000–1.000] | 0.050 [0.000–0.100] |

Latency over the 12 answered queries: mean 275 ms, p50 268 ms, p95 354 ms,
p99 373 ms. `n_failed: 0`, `n_degraded: 0`, `n_ingest_failed: 0`.

nDCG uses **linear** gain, the definition `trec_eval`, `pytrec_eval`, BEIR, MTEB
and the ViDoRe leaderboards use, so this number is comparable with a public one
and Burges' `2^rel − 1` is behind `--gain exponential`. Twelve queries give a
confidence interval almost as wide as the estimate: this is a floor to beat, not
a score to quote.

**Bun runtime: not measured.** The plan asks for a baseline for both runtimes;
Bun is frozen and this round was Rust-only, so the Bun row is missing rather
than guessed. To fill it, run the same command against the Bun service and
commit the `report.md` next to this file.

## 2. Cross-scope leak matrix — Rust

```sh
A=$("$KB_SERVER_BIN" create-api-key leak-a a --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
B=$("$KB_SERVER_BIN" create-api-key leak-b b --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -1)
kb-server eval leak --base-url "$KB_STACK_BASE_URL" --api-key "$A" --peer-key "$B" \
  --out /tmp/kb-leak-final
```

2026-10-16, harness commit `85e436e`: 8 seeded documents (2 tenants × 2
knowledge bases × 2 categories), 12 scopes, **240 probes, 0 cross-scope hits**.

| arm | tenant | knowledge_base | category | document_ids | arm |
|---|---|---|---|---|---|
| vector | PASS | PASS | PASS | PASS | **PASS** |
| bm25 | PASS | PASS | PASS | UNTESTED | **PARTIAL** |
| neighbour | PASS | PASS | PASS | PASS | **PASS** |
| figure | UNTESTED | UNTESTED | UNTESTED | UNTESTED | **UNTESTED** |
| entity | PASS | PASS | PASS | PASS | **PASS** |

Each cell is `probes / leaks / positive-control hits`. The positive control is
the same canary queried from a scope that *does* contain the target; 25
cross-tenant entity probes were answered with a 4xx, which is the guard firing.

Two cells are untested, and both are findings rather than passes:

* **`bm25 × document_ids`** — `POST /v1/search` with `documentIds` takes the
  documentIds branch (`routes.rs`, "Branch 1"), which answers from the vector arm
  alone. BM25 never runs inside that scope, so the instrument reports UNTESTED
  instead of claiming containment it never checked. TypeScript has the same
  branch, so this is parity, not a Rust regression.
* **figure, everywhere** — figure crops only come from a layout extractor
  (`kb-extract`: the MinerU sidecar, `mineru-api` or Mistral). The stack runs
  none, so there is no figure to retrieve and the arm has no positive control.
  Closing this needs a stack with a layout extractor, not a different corpus.

## 3. What to run next

| Target (plan) | Today | Gap |
|---|---|---|
| right page ranked first, ≥ 87% over ≥ 200 labelled questions | no labelled set exists | build `eval/id-internal/`, then measure |
| ViDoRe v3 pipeline leaderboard, top 3 | not run; needs real embeddings | drop a ViDoRe v3 subset into `kb-eval` and run it with a real model profile |
| cross-tenant hits over the red-team matrix: 0 | **0 over 240 probes**, 1 arm partial, 1 arm untested | close the figure arm with a layout extractor |
| `/v1/search` p95 warm, local profile < 300 ms | 354 ms with an 8-dim stub, 6 documents | re-measure with real embeddings on a real corpus |