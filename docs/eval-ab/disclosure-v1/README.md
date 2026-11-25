# EVAL-7 — progressive-disclosure coverage gate (measured 2026-11-25, GB10)

**Status: measured, two targets MISSED.** Coverage **0.238 [0.206, 0.269]**
(target ≥ 0.90) and `/content` p95 **60.4 ms** (target < 50 ms) both missed;
chars read / gold chars **1.353 [1.315, 1.387]** passed (≤ 1.5×). These are
live numbers on the ViDoRe HR corpus, reported as measured — not hidden.

## Why this exists

A calling agent needs the one page that answers a question, read through the API, without
re-searching and without blowing its context budget. Retrieval nDCG@10 says a relevant chunk was
ranked highly; it does not say the agent could actually *read the whole gold page* from the
reported hit. This gate measures the latter with an outside, human-labelled set: ViDoRe v3 HR
page qrels. The reader is a fixed script, with no LLM in the loop, so a number here is a property
of the API, not of a model.

## Method

For each question, in order:

1. `POST /v1/search {"query": …, "maxChunks": 5}`.
2. Group hits by `documentId`, keeping rank order. Fetch `GET /v1/documents/:id/structure` once
   per document (cached across the whole run) and take the first section whose page range
   contains the hit's page. With no such section, read the page window `[page-1, page+1]`
   (API pages are 0-based; qrels pages are 1-based).
3. `GET /v1/documents/:id/content` on that section or window, following `nextCursor` until the
   read is complete. A selection whose chunk range was already read is skipped. The reader stops
   when its budget of `1.5 ×` the gold pages' characters is used; `maxChars` is capped at the
   API's 8,000 default.
4. Separately, `GET /v1/documents/:id/content?pageStart=p&pageEnd=p` for every gold page gives
   the budget denominator. These calibration reads do **not** spend the budget and are **not**
   included in the latency samples.
5. Coverage for the question is the fraction of gold pages whose text appears, after collapsing
   whitespace runs to a single space, as an exact substring of the concatenation of everything
   the reader fetched. A gold page whose document is absent from the ingest map (`missing_doc`)
   counts as uncovered and is also counted separately. An empty gold page cannot be verified and
   counts as uncovered.

`results.json` holds mean coverage with a 95% percentile bootstrap CI (2000 resamples, fixed
seed), mean chars-read/gold-chars with its CI, totals, p50/p95 latency for each endpoint, the
question count and the `missing_doc` count. `per_question.jsonl` gives one record per question,
including which gold pages were covered and every visible error. `report.md` repeats the table
and prints the three targets with PASS/MISS. A MISS is reported, never hidden.

**Targets:** coverage ≥ 0.90 at ≤ 1.5× gold chars, and `/content` p95 < 50 ms. All three must pass.

## Running it against a local stack

Build the dataset first, once, from the pinned ViDoRe v3 revision (the PDFs are not committed):

```bash
cd rust/crates/kb-eval/scripts
uv run import_vidore.py --dataset vidore/vidore_v3_hr --language english \
  --out ../../../kb-eval/datasets/vidore3-hr-en
```

Bring up the stack, mint a key, ingest the corpus and only then run the disclosure harness. The
ingest step writes the `<doc-key> → documentId` map that the harness needs to find a gold page's
document; `--skip-ingest` on a second `eval` run reuses the same map without re-uploading.

```bash
cd <repo>
rust/scripts/stack-up.sh
. /tmp/kb-stack/stack.env
KEY="$(./rust/target/release/kb-server create-api-key disclosure disclosure \
  --scopes kb:read,kb:write | grep -oE 'rkb_[A-Za-z0-9_-]+' | head -n 1)"
DATASET=rust/crates/kb-eval/datasets/vidore3-hr-en
OUT=docs/eval-ab/disclosure-v1/run

# Ingest the 14 PDFs and produce $OUT/ingest-map.json.
./rust/target/release/kb-server eval \
  --base-url "$KB_STACK_BASE_URL" --api-key "$KEY" \
  --dataset "$DATASET" --out "$OUT"

# Measure the scripted reading loop over the same 318 human-labelled questions.
./rust/target/release/kb-server eval disclosure \
  --base-url "$KB_STACK_BASE_URL" --key "$KEY" \
  --dataset "$DATASET" --out "$OUT" --ingest-map "$OUT/ingest-map.json" \
  --top-k 5 --budget-factor 1.5
```

Use `--limit N` for a smoke subset (the bootstrap CI on a subset is not the gate). If the
ingest used a different output directory, point `--ingest-map` at that directory's
`ingest-map.json`, or pass `--out` of that ingest as the harness `--out`. The committed
`rust/crates/kb-eval/sets/aggregative/vidore3-hr-en/` subset also works: the harness auto-discovers
`queries.nonaggregative.jsonl` + `qrels.nonaggregative.tsv` when `queries.jsonl` is absent.

The four release gates are unchanged by this harness; it only reads `/v1/*` with a key and writes
files under `--out`.

## Results (2026-11-25)

Setup: branch `bench/v09-measure` @ `7d5adbdf`; release kb-server from
`~/kb-target-v09m`, run natively on the GB10 against the `stack-up.sh` containers;
Nemotron at `127.0.0.1:8210` embedded the HR corpus once for the RET-12 A/B
(no re-ingest here); qwen3-rerank-0.6B at `127.0.0.1:8122`; the page-image arm
was off and the search config was the shipped default (breadcrumbs off, no cap).
See [`../rerank-breadcrumbs/README.md`](../rerank-breadcrumbs/README.md) for the
stack wiring. The `--ingest-map` came from that single ingest.

Exact command (wrapper `~/v09m/run-m4-disclosure.sh`, log
`~/v09m/logs/m4-disclosure.out`):

```sh
source ~/v09m/stack-env-hr.sh
~/kb-target-v09m/release/kb-server eval disclosure \
  --base-url http://127.0.0.1:28081 --key "$KEY" \
  --dataset "$PWD/rust/crates/kb-eval/datasets/vidore3-hr-en" \
  --out ~/v09m/results/m4-disclosure \
  --ingest-map ~/v09m/results/m1-hr/base/ingest-map.json \
  --top-k 5 --budget-factor 1.5
```

| Measure | Value | Target | Verdict |
|---|---:|---|---|
| coverage (318 questions) | **0.238 [0.206, 0.269]** | ≥ 0.90 | **MISS** |
| chars read / gold chars | 1.353 [1.315, 1.387] | ≤ 1.5× | PASS |
| `/content` p95 | **60.4 ms** | < 50 ms | **MISS** |

Per-endpoint latency (ms, from `results.json`; `/structure` n = 14 cached
documents, `/content` n = 3839 reads, `/v1/search` n = 318):

| endpoint | p50 | p95 | p99 | n |
|---|---:|---:|---:|---:|
| `/v1/search` | 11.7 | 56.0 | 79.8 | 318 |
| `/v1/documents/:id/structure` | 13.5 | 41.7 | 50.6 | 14 |
| `/v1/documents/:id/content` | 15.4 | **60.4** | 118.9 | 3839 |

`missing_doc = 0`; bootstrap 2000 resamples. Raw stdout excerpt:

```text
disclosure: 318 questions, coverage 0.238 [0.206, 0.269], chars read/gold 1.353, content p95 60.4 ms
targets: coverage MISS · chars PASS · content p95 MISS
wrote results.json, per_question.jsonl, report.md
```

Raw artifacts: `~/v09m/results/m4-disclosure/{results.json,report.md,per_question.jsonl}`.
One question's raw line from `report.md`:

```text
| 0 | 0.333 | 2/6 | 0 | 10276 | 13460 | 0.763 |
```

No paid call was made. The MISS is reported, not hidden.

## EVAL-9

**Status: code done; live re-run pending. No new result is claimed here.** The 2026-11-25
numbers above are the baseline.

### What changed in the gate

`answer_page_coverage` is now the **primary** metric: the mean, with a bootstrap CI, of the
per-question `answer_page_hit` (1 when at least one gold page's text was fully read, else 0).
The primary target is **≥ 0.80 at ≤ 1.5× gold chars**. Two diagnostics were added:

- `single_gold_coverage` — the old all-pages coverage restricted to questions with exactly one
  gold page, with its own CI.
- `best_gold_rank` — the 1-based rank of the first search hit whose (document, page) is a gold
  page, `null` if no gold page is in the top-k. `results.json` also carries its distribution
  (count per rank, number of questions with/without a gold hit, mean rank).

`per_question.jsonl` now carries `answer_page_hit`, `best_gold_rank`, `hits` (rank, document id,
page and goldness for every hit) and `pages_read` (document, selector, chunk range, page range
and characters kept for every budgeted read). That split is what makes the diagnosis possible:
a low `best_gold_rank` with a high rank value means search ranked the gold page low; a gold hit
at rank 1 with `answer_page_hit = 0` means the reader spent its budget on the wrong page.

`coverage` (all gold pages) is still computed and printed in `report.md` with its PASS/MISS, but
it is **secondary** now; the old 0.90 target is kept only for continuity with EVAL-7.

### Why the 0.90 all-pages gate was mis-specified

The reviewer's analysis of the 2026-11-25 per-question data:

- ViDoRe HR has on average **5.4 gold pages per question** (median 3, max 30), spread over
  documents of roughly **180 pages**.
- At least one gold page was fully read in **52.2%** of questions.
- Only **14%** of the **71 single-gold-page questions** were covered.

Demanding that the reader fetch *every* gold page in one 1.5× budget mostly measures how many
gold pages a question happens to have, not whether the API can expose the page that answers it.
On this corpus the floor imposed by question shape is far below 0.90, so the gate could never
pass even with a perfect rank-1 answer. The answer-page gate asks the question the calling agent
actually asks: did it get one complete answer page, under budget?

### Exact re-run command

Build the new release binary and run the harness against the same stack and ingest map. Run this
on the GPU box after bringing up the HR stack (see the 2026-11-25 setup above); nothing in this
section is a measured result.

```sh
export CARGO_TARGET_DIR=/home/shiro/kb-target-eval9
export CARGO_BUILD_JOBS=2
cargo build --release --manifest-path rust/Cargo.toml -p kb-server

source ~/v09m/stack-env-hr.sh
"$CARGO_TARGET_DIR/release/kb-server" eval disclosure \
  --base-url http://127.0.0.1:28081 --key "$KEY" \
  --dataset "$PWD/rust/crates/kb-eval/datasets/vidore3-hr-en" \
  --out ~/v09m/results/m4-disclosure-eval9 \
  --ingest-map ~/v09m/results/m1-hr/base/ingest-map.json \
  --top-k 5 --budget-factor 1.5
```

The three outputs are `results.json` (both coverages, the rank distribution and the latency
table), `per_question.jsonl` (the new fields above) and `report.md` (both gates with PASS/MISS).
Part A's diagnosis — search rank vs. reader choice — is read off those per-question fields; no
paid call is made by the harness.

## EVAL-9 live run (2026-11-25, GB10)

**Status: measured. Primary target MISSED; `/content` p95 PASS.** Setup: branch
`bench/v1-measure` @ `231a25bf` (`origin/main`), fresh release kb-server in
`~/kb-target-v1m` (10m 54s build), run natively on the host against our own
`stack-up.sh` stack (`KB_STACK_NAME=v1mhr`, kb-server `http://127.0.0.1:28091`,
Postgres 25443, Qdrant 26345/26346, cache `~/v1m/cache-hr`). The **already running**
Nemotron at `127.0.0.1:8210` embedded the HR corpus (`omniembed`, 2048-d) and
qwen3-rerank-0.6B at `127.0.0.1:8122` reranked (`serve.sh up qwen3-rerank-0.6b`).
The page-image arm was pinned off with `KB_PAGE_EMBEDDING_MODEL=''` after
`stack-up.sh` wrote `stack.env` (otherwise the studio tier re-enables it);
breadcrumbs off, no cap, as in the 2026-11-25 baseline. The corpus was re-ingested
with the new binary into the fresh stack (`~/v1m/results/m1-hr/base/ingest-map.json`,
`missing_doc = 0`); the harness then read the same 318 labelled questions.

Wrapper `~/v1m/run-m1-disclosure.sh`, stdout `~/v1m/logs/m1-disclosure.out`.
Raw stdout excerpt:

```text
disclosure: 318 questions, answer-page coverage 0.522 [0.469, 0.575], all-pages coverage 0.238 [0.206, 0.269], chars read/gold 1.353, content p95 5.5 ms
targets: answer-page MISS (primary) · chars PASS · all-pages MISS (secondary) · content p95 PASS
wrote results.json, per_question.jsonl, report.md
```

| Measure | Value | Target | Verdict |
|---|---:|---|---|
| **answer_page_coverage** (318 questions) | **0.522 [0.469, 0.575]** | ≥ 0.80 | **MISS** |
| single_gold_coverage (71 single-gold questions) | 0.141 [0.070, 0.225] | (diagnostic) | — |
| coverage, all gold pages (secondary) | 0.238 [0.206, 0.269] | ≥ 0.90 | MISS |
| chars read / gold chars | 1.353 [1.314, 1.387] | ≤ 1.5× | PASS |
| `/content` p95 | **5.5 ms** | < 50 ms | PASS |

`best_gold_rank` (top-k=5): rank 1: 178, rank 2: 31, rank 3: 31, rank 4: 20,
rank 5: 8; no gold in the top 5: 50; mean rank among top-5 hits 1.69. The
per-question `hits` list is longer than 5 (15 entries): across all 318 questions
40 have no gold hit at any recorded rank and 10 have their first gold hit at
rank 8–15.

Per-endpoint client latency (ms, `results.json`; `/structure` n = 14 cached
documents, `/content` n = 3838 reads, `/v1/search` n = 318):

| endpoint | p50 | p95 | p99 | n |
|---|---:|---:|---:|---:|
| `/v1/search` | 10.9 | 19.4 | 23.6 | 318 |
| `/v1/documents/:id/structure` | 12.9 | 41.1 | 41.2 | 14 |
| `/v1/documents/:id/content` | 3.6 | **5.5** | 6.2 | 3838 |

`/content` p95 fell from **60.4 ms → 5.5 ms** on the same corpus and question
set; the answer-page gate still misses (0.522 < 0.80). The first `eval` query
run warmed the rerank disk cache in `~/v1m/cache-hr` before the disclosure run,
so the `/v1/search` samples are warm-cache measurements.

### Diagnosis of the 152 questions with `answer_page_hit = 0`

Classified from `per_question.jsonl` (`best_gold_rank` there is over all recorded
hits, not just the top 5):

- **(a) no gold page among the search hits (`best_gold_rank` null): 36**
- **(b) a gold page was among the hits but the reader did not read it fully: 116**

Within (b) the first gold hit was rank 1 for 50 questions, rank 2 for 20, rank 3
for 22, rank 4 for 12, rank 5 for 5, and rank 8–15 for 7. So the dominant cause
is the reader spending its 1.5× budget on sections that omit the gold page, not
search failing to surface it. Three verbatim examples of each (full `hits` and
`pages_read` arrays are in `~/v1m/results/m4-disclosure-eval9/diagnosis.txt`):

```text
(a) no gold hit
--- 1: What drives the rise in women's workforce involvement in EU nations such as Germany?
    covered=0/14 best_gold_rank=None chars_read=68106 chars_ratio=1.138
    hits: rank1 doc 7565e68d-… page 6 gold=false, rank2 doc f2343726-… page 67 gold=false,
          rank3/4/5 doc e01449b3-… pages 55/61/13 gold=false (15 hits, all gold=false)
    pages_read: section:s4 doc 7565e68d-… chunks 13-13 pages 6-6 chars 728;
                section:s14 doc f2343726-… chunks 418-477 pages 67-72 (5 reads, 37k chars);
                section:s27/s29/s3 doc e01449b3-… pages 55-57, 61-62, 12-13 (5 reads, 26k chars)
--- 21: Compare adult learning participation rates between tertiary-educated adults and those with lower education in the EU.
    covered=0/2 best_gold_rank=None chars_read=12309 chars_ratio=1.500
    hits: rank1 doc f2343726-… page 55, rank2 doc e01449b3-… page 45, rank3 doc f2343726-… page 57,
          rank4 rank1 again, rank5 doc e01449b3-… page 12 (15 hits, all gold=false)
    pages_read: section:s13 doc f2343726-… chunks 292-304 pages 48-50 chars 7737;
                section:s13 chunks 305-315 pages 50-51 chars 4572
--- 22: How do the trends in EU citizen mobility compare to those of third-country nationals in terms of inflows and net mobility between 2021 and 2022?
    covered=0/2 best_gold_rank=None chars_read=5112 chars_ratio=1.500
    hits: all 10 hits on doc 90abe182-… pages 28/25/25/24/28/… gold=false
    pages_read: section:s26 chunks 106-109 page 28 chars 2793;
                section:s24 chunks 90-92 page 24 chars 2129; section:s24 chunks 93-94 pages 24-25 chars 190

(b) gold hit, reader missed
--- 4: estimated skilled labor needs for EU green transition by 2030
    covered=0/2 best_gold_rank=3 chars_read=10347 chars_ratio=1.500
    hits: rank3 and rank10 doc d124a3f7-… page 2 gold=true (15 hits)
    pages_read: section:s19 chunks 106-117 pages 26-28 chars 7965;
                section:s19 chunks 118-120 page 29 chars 2062;
                section:s1 chunks 0-1 pages 0-1 chars 320  (gold page 2 never read)
--- 6: How might the observed trends in top-end income inequality (S80/S50) relate to broader economic shifts in high-income households during the pandemic recovery?
    covered=0/1 best_gold_rank=1 chars_read=7218 chars_ratio=1.500
    hits: rank1/6/7 doc f2343726-… page 96 gold=true; ranks 2/8/9 doc 90abe182-… page 97 gold=false;
          ranks 3/4/5/10/11/12/13/14 doc e01449b3-… pages 31/39/40 gold=false; rank15 page 41 gold=false
    pages_read: section:s19 doc f2343726-… chunks 584-594 pages 88-89 chars 7186;
                section:s19 chunks 595-605 pages 89-91 chars 32 (gold page 96 never read)
--- 7: difference in labor productivity growth between eu and us in q4 2024 percentage points
    covered=0/1 best_gold_rank=1 chars_read=5352 chars_ratio=1.500
    hits: rank1/6/7 doc c47bf43c-… page 6 gold=true; all other 12 hits doc 9b5e7c69-… pages 31/64/32 gold=false
    pages_read: section:s5 doc c47bf43c-… chunks 18-23 pages 5-6 chars 4922;
                section:s5 chunks 24-25 page 6 chars 430 (page 6 section truncated before gold text)
```

Raw artifacts: `~/v1m/results/m4-disclosure-eval9/{results.json,report.md,per_question.jsonl,diagnosis.txt}`.
No paid call was made. Both missed targets are reported, not hidden.
