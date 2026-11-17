# The November bake-off (2026-11-13), in the repo

On 2026-11-13/14 a bake-off ran on the ThinkStation PGX (GB10, 128 GB unified) over
`hotpot-200-test`, `miracl-id-200`, `vidore3-hr-en-pages` and `vidore3-cs-en-pages`: text
embedders, page embedders, rerankers and OCR engines, all behind one registry that states
each model's protocol. The OCR half became [`../ocr-tiers`](../ocr-tiers/); this folder
carries the **embedding and reranking runs**, which until now existed only on that machine and
were missing from [`../../benchmarks.md`](../../benchmarks.md).

The four runs here are the ones worth keeping:

| Folder | Role | Models | Datasets |
|---|---|---|---|
| `embed-20261113T182129Z` | text embedders | 7 (one failed) | hotpot, MIRACL-id |
| `embed-20261113T210207Z` | text embedders, re-run | 3 | hotpot, MIRACL-id |
| `rerank-20261113T191332Z` | text rerankers | 3 | hotpot, MIRACL-id |
| `page-20261113T190504Z` | page embedders | 4 | ViDoRe HR / CS pages — **no data**, see below |

Each folder is the tool's own output: `report.md` (human), `results.json` (everything,
including each model's protocol), `per_query.jsonl` (the per-query scores the CI comes from).
The harness lives on the box at `~/llmhost/bakeoff` (`models.yaml` registry, `serve.sh`,
`bakeoff.py`); it is *not* in this repo, so these runs are not reproducible from a checkout
alone — that is a real gap, listed under "What this is not".

## Method

- **Serving.** llama.cpp for text embedders and rerankers (GGUF Q8_0/Q4_K_M), vLLM 0.33 for
  VL models. The box has 128 GB of **unified** memory, so the runner serves **one role at a
  time**; the page run's four models were all down at once and every row says
  `endpoint DOWN: Connection refused`.
- **Protocol is per model, and client-side.** The server adds none of it:
  `qwen3-embed-*` take `Instruct: Given a web search query, retrieve relevant passages that
  answer the query\nQuery:` (no space after `Query:`); both nemotron embedders take
  `query: ` / `passage: `; `bge-m3` and `granite-embedding-*-r2` take nothing.
- **Scoring.** The runner's own metric code: nDCG@10, Recall@10/50, MRR@10, Hit@1, with a
  paired bootstrap for every delta. Baseline is `qwen3-embed-0.6b` (today's tier preset).

## Text embedders — pooled over both datasets

Two runs disagree about `nemotron-embed-1b-v2`; both are here rather than picking one.

| Model | Run 182129 | Run 210207 | Licence | Dim |
|---|---:|---:|---|---:|
| `nemotron3-embed-1b` | **0.8671** | **0.8664** | NVIDIA Open Model License | 1024 |
| `qwen3-embed-4b` | 0.8658 | — | Apache-2.0 | 2048 |
| `nemotron-embed-1b-v2` | 0.7423 | 0.8516 | NVIDIA Open Model License | 2048 |
| `bge-m3` | 0.8362 | — | MIT | 1024 |
| `qwen3-embed-0.6b` (baseline) | 0.8151 | 0.8134 | Apache-2.0 | 1024 |
| `granite-embed-ml-r2` | 0.7854 | — | Apache-2.0 | 384 |
| `me5-small` | failed: 4,228/4,228 items rejected | — | MIT | 384 |

Per dataset, run 182129:

| Model | hotpot-200-test (nDCG@10) | Δ vs baseline | MIRACL-id-200 | Δ vs baseline |
|---|---:|---:|---:|---:|
| `nemotron3-embed-1b` | 0.9469 | **+0.0857** [+0.0594, +0.1135] | 0.8272 | **+0.0351** [+0.0158, +0.0544] |
| `qwen3-embed-4b` | 0.9181 | +0.0569 [+0.0368, +0.0782] | **0.8397** | **+0.0476** [+0.0294, +0.0664] |
| `bge-m3` | 0.8870 | +0.0258 [+0.0023, +0.0498] | 0.8107 | +0.0186 [−0.0006, +0.0377] |
| `nemotron-embed-1b-v2` | 0.8783 | +0.0171 [−0.0110, +0.0445] | 0.6743 | **−0.1178** [−0.1498, −0.0863] |
| `qwen3-embed-0.6b` | 0.8612 | — | 0.7921 | — |
| `granite-embed-ml-r2` | 0.8468 | −0.0144 [−0.0392, +0.0104] | 0.7547 | −0.0374 [−0.0609, −0.0143] |

In run 210207 the same `nemotron-embed-1b-v2` scores 0.9222 on hotpot (+0.0667) and 0.8164 on
MIRACL (+0.0240) — against 0.8783 and 0.6743 eight hours earlier. **The two runs cannot both
be right, and the registry says why it might happen:** every protocol carries a
`TODO verify prefix` note, and a missing `query: `/`passage: ` pair would hurt the model that
needs it on exactly the asymmetric case. Until one pair of runs is repeated with the prefix
asserted, treat the nemotron **v2** number as unknown and `nemotron3-embed-1b` as the
interesting one — it leads in *both* runs, on *both* datasets.

## Text rerankers — pooled over both datasets

Candidate pool: the top 50 of `qwen3-embed-0.6b`, documents truncated to 2,000 characters.
No-rerank nDCG@10 is 0.8565 (hotpot) and 0.7910 (MIRACL).

| Model | Pooled nDCG@10 | Δ vs no rerank | Δ vs bge | hotpot | MIRACL | Licence |
|---|---:|---:|---:|---:|---:|---|
| `qwen3-rerank-0.6b` | **0.8792** | +0.0664 [+0.0491, +0.0842] | +0.0011 [−0.0131, +0.0157] | 0.9062 | **0.8657** | Apache-2.0 |
| `bge-reranker-v2-m3` (today's default) | 0.8781 | +0.0653 [+0.0475, +0.0827] | — | 0.9530 | 0.8407 | Apache-2.0 |
| `nemotron-rerank-1b-v2` | 0.8769 | +0.0641 [+0.0474, +0.0809] | −0.0012 [−0.0127, +0.0104] | **0.9577** | 0.8366 | NVIDIA Open Model License |

Pooled, the three are a tie: every pairwise delta is inside ±0.002. Split by language they are
not: qwen3 is **+0.025 on MIRACL** over bge (significant), and bge is **+0.047 on hotpot** over
qwen3 (significant). So "best reranker" is a language choice, not a ranking — which is what the
tier preset's bge default already assumes for English and what a mixed or Indonesian corpus
should re-check.

## Page embedders — this run has no data

All four candidates (`qwen3-vl-embed-2b`, `nemotron-embed-vl-1b-v2`,
`nemotron-rerank-vl-1b-v2`, `qwen3-vl-rerank-2b`) are recorded as
`endpoint DOWN: <urlopen error [Errno 111] Connection refused>` for both datasets. Nothing was
measured; the empty tables are the evidence, kept so nobody mistakes the run for a result.

The page side was measured separately in [`../../plan-one-embedder.md`](../../plan-one-embedder.md): the 2B page
embedder on ViDoRe HR, its text and image vectors in one space, against the 0.6B text model on
the same pages.

## What this is not

- **Not reproducible from the repo.** The harness, the registry and the weights live on the
  ThinkStation (`~/llmhost/bakeoff`, 49 GB of models). The run folders are copied here; the
  code that made them is not. Porting `bakeoff.py` + `models.yaml` into `rust/crates/kb-eval`
  is the fix, and it has not been done.
- **Not latency.** `p50 ms` and `p95 ms` are `-` in every table: the runner did not record
  per-request timings. The `corpus tok/s` column exists only in run 210207.
- **Not cost.** Everything ran locally; nothing here is a paid-API number.
- **Not the VL models on text.** No run embeds *text* with a page (VL) embedder — that is
  exactly the question [`../../plan-one-embedder.md`](../../plan-one-embedder.md) exists for.
