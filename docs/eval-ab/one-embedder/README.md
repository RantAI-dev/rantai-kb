# One embedder or two? (EVAL-4, measured 2026-11-17)

**Answer: two. Do not collapse.** The pre-registered rule in
[`../../plan-one-embedder.md`](../../plan-one-embedder.md) was conjunctive and fails on every
clause, most of all on the page arm:

| Clause (fixed before measuring) | Required | Measured | |
|---|---|---|---|
| Text does not lose | within −0.015 of `Qwen3-Embedding-0.6B` | **−0.143** [−0.179, −0.108] on MIRACL-id chunks | ✗ |
| Pages do not lose | ViDoRe HR page nDCG@10 ≥ **0.695** | **0.221** one space; **0.277** image-only | ✗ |
| Ingest holds | ≥ 60% of the 0.6B's chunks/s | not run — the page clause already failed | — |

## Method

One harness (`rust/crates/kb-eval/scripts/embed_ab.py`), same items, same queries, same
scorer for every arm; every vector cached by model + content hash. Models on the ThinkStation
PGX (GB10, 128 GB unified) over Tailscale: `Qwen3-Embedding-0.6B` under llama.cpp Q8_0 with its
card's `Instruct: … Query:` prefix, `Qwen3-VL-Embedding-2B` under vLLM 0.33 bf16
(`--runner pooling --convert embed`) with no prefix — the server already applies the card's
instruction (verified: the same query with and without a `system` "Represent the user's input."
message is cosine **1.000000**).

The 0.6B reproduces the published baseline, which is the harness's own check: **0.796** nDCG@10
on MIRACL-id-200 against the **0.792** committed in
[`../bakeoff-2026-11-13`](../bakeoff-2026-11-13/).

## Result 1 — text, chunk-sized (MIRACL-id-200)

The engine's own chunks (`rkdump`, 3,593 chunks over 2,933 passages), ranked per chunk and
scored per document where the qrels are:

| Model | nDCG@10 | MRR@10 | Recall@10 |
|---|---:|---:|---:|
| `Qwen3-Embedding-0.6B` | **0.796** | 0.822 | 0.940 |
| `Qwen3-VL-Embedding-2B` | 0.653 | 0.680 | 0.805 |

Δ **−0.143** [−0.179, −0.108] against the VL model. This is the cleanest possible test of the
assumption the collapse rests on — the same chunks, the documented protocol for each model,
no reranker — and the multimodal model is clearly worse at text, not merely different.

## Result 2 — pages (ViDoRe HR, 318 queries, 1,110 pages)

| Arm | nDCG@10 | MRR@10 | Recall@10 |
|---|---:|---:|---:|
| `Qwen3-Embedding-0.6B` on the pages' text | **0.488** | 0.635 | 0.549 |
| `Qwen3-VL-Embedding-2B` on the pages' text | 0.058 | 0.107 | 0.064 |
| `Qwen3-VL-Embedding-2B` on the page images | 0.277 | 0.390 | 0.320 |
| `Qwen3-VL-Embedding-2B`, one space (best of the two per page) | 0.221 | 0.305 | 0.272 |

Two things die here. The text arm is at chance on whole pages (0.058), and the one space is
**worse than its own image arm** (0.221 vs 0.277, Δ +0.056 [+0.040, +0.073] for image-only):
the text vectors do not supplement the image vectors, they pollute them. Against what ships —
0.705 with the page arm and a page reranker, 0.629 rerank-free offline — there is no case.

The image-only number is also the first real measurement of the local page embedder (it was
"mock-tested only" in `docs/kb-tiers.md`), and **0.277 is well below the cloud page arm's 0.575
image-only** on the same set. That is a separate finding, with its own backlog item (`EVAL-5`).

## Result 3 — why the text arm is at chance: input length

The probe respected the same 20 queries and the 96 pages their qrels name, embedding each page
text uncut and cut to N characters (`probe_text_length.py`, `probe.json`):

| Page text | nDCG@10 |
|---|---:|
| first 300 chars | 0.478 |
| first 600 | 0.444 |
| first 1,200 | 0.548 |
| first 2,400 | **0.600** |
| the whole page (up to ~4k tokens) | 0.208 |

So the model is not incapable of text; it is incapable of *long* text. Between 300 and 2,400
characters the number is stable around 0.44–0.60 (20 queries, so those differences are noise);
at full page length it collapses. A last-token pooling read over a multi-thousand-token page
is not a document embedding.

This does not rescue the design: production text units are chunks, and Result 1 measures
exactly that — 0.653 against 0.796. It does say that anyone reaching for a VL embedder for text
should keep the unit small and measure, not assume.

## What stays, and what is now measured rather than assumed

- Studio and Station keep **two** embedders and the fusion between them, including the
  cross-validated weight (w≈0.5, RRF k=5). That knob is a real cost of the current design — it
  was tuned on one English domain — and it is now a documented cost with a measured reason.
- The **local page embedder is weaker than the cloud one** (0.277 vs 0.575 image-only). Whether
  that is the model or the inputs is open: the pages here are 96-dpi JPEG q80 from the old page
  pipeline, and the card's examples use larger PNGs. Resolution is the first lever to test.
- **The text side has a bigger, cheaper win than any collapse**, and it is not this change:
  `nemotron3-embed-1b` measured **+0.086** (hotpot) and **+0.035** (MIRACL) over
  `qwen3-embed-0.6b` in both November runs. It needs the prefix verified, the NVIDIA Open Model
  License accepted and a 2048-d index (a re-index), so it is its own piece of work.

## What would change the answer

1. A local page embedder that reaches ≥ 0.55 image-only on ViDoRe HR — i.e. near the cloud
   model — **and** whose text vectors survive at chunk length. Then the collapse is worth
   re-running; the harness is one command (`docs/plan-one-embedder.md`).
2. Evidence that the 0.221 is an input artefact rather than the model: higher-resolution page
   images are the obvious test, and they would lift the image arm, not the text arm.
3. If text units ever become page-sized (they are not), the length result says a VL embedder is
   the wrong tool for them regardless.

## Caveats

- **Page-level text, not chunks, for Result 2.** That is deliberate — a one-space design would
  put page vectors beside text vectors — but it means the 0.488 is not the shipped text
  pipeline (0.645/0.705 include chunks, BM25 and a reranker). Result 1 is the chunk-level
  comparison, and it is the one the text clause is judged on.
- **20 queries in the probe**, so its CI is wide and its non-monotonic middle is noise.
- **One domain for pages** (EU labour reports), one language split for text (Indonesian MIRACL,
  English hotpot).
- **No throughput run.** The rule's third clause was not measured because the second had
  already failed; `embed_ab.py` reports no timings.
- **GB10 unified memory**, so any timing would transfer as a ratio at best.

## Files

| File | What |
|---|---|
| `miracl/` | 200 queries: `summary.json` and per-query metrics for both models |
| `vidore-hr-pages/` | 318 queries: the 2B's text, image and one-space per-query metrics |
| `vidore-hr-06b-text/` | 318 queries: the 0.6B's text per-query metrics on the same pages |
| `probe_text_length.py`, `probe.json` | the length probe and its output |

The TREC runs (~9 MB of top-100 lists) are deliberately not in git; `embed_ab.py` regenerates
them from the cache with the command in the plan.
| `../../plan-one-embedder.md` | the plan, with the rule fixed before any number existed |
