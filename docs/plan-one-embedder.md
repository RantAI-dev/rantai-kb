# Should Studio and Station run one embedder instead of two?

Plan for `EVAL-4`. Written 2026-11-17, before measuring.

## Result (2026-11-17): keep two embedders

Measured, and the rule above fires against collapsing on every clause. Write-up and runs:
[`eval-ab/one-embedder/`](eval-ab/one-embedder/README.md).

| Clause | Required | Measured |
|---|---|---|
| Text | within −0.015 of the 0.6B | **−0.143** [−0.179, −0.108] on MIRACL-id chunks (0.653 vs 0.796) |
| Pages | ViDoRe HR ≥ 0.695 | **0.221** one space, **0.277** image-only, against 0.705 shipped |
| Ingest | ≥ 60% of the 0.6B's chunks/s | not run — the page clause had already failed |

Two findings came out of it that are worth more than the answer itself: the VL model's text
arm collapses on *long* inputs (0.208 on whole pages, 0.44–0.60 once cut to 300–2,400
characters), and the local page embedder is much weaker than the cloud one on pages
(0.277 vs 0.575 image-only) — tracked as `EVAL-5`.

## Goal

**Literal ask.** Should the upper tiers embed text chunks and page images with one model
(`Qwen3-VL-Embedding-2B/8B`) instead of two (text with `Qwen3-Embedding-0.6B`, pages with the
VL model)?

**Goal behind it.** Fewer models to run, one query path, and no tuned fusion weight — the
two-space design joins the arms with weight ≈0.5 and RRF k=5, cross-validated on one English
domain (`docs/eval-ab/vidore3-hr-en-image`), which is a brittle knob that silently degrades on
another corpus.

**Goal beyond that.** The rule in `docs/kb-tiers.md` is "one model per role"; it is currently a
statement of intent without evidence for the local tiers. The cloud tier already runs one
embedder for text and pages and beats the two-space numbers (0.711 HR, 0.828 CS). The same
test, run the same way, is what makes the local tiers' claim checkable.

**Chosen level.** Decide the embedder for Studio and Station with a measured A/B, and ship the
harness, so the next "one model instead of two" question is an afternoon rather than a debate.
Not attempted here: collapsing the rerankers (a separate question with its own evidence gap).

## Target

Baselines that already exist (committed runs, same datasets):

| Metric | Baseline | Where |
|---|---|---|
| ViDoRe HR, page nDCG@10, two spaces + page rerank | **0.705** | `eval-ab/vidore3-hr-en-night` |
| ViDoRe HR, page nDCG@10, text only | 0.645 | same |
| MIRACL-id-200, document nDCG@10, 0.6B text + bge top 20 | **0.830** | `eval-ab/README` |
| hotpot-200-test, document nDCG@10, same | **0.928** | same |
| Ingest, 943-page PDF, provider-like latency | 163 s (target < 200) | `eval-ab/ingest-big-pdf` |

**Pre-registered decision rule** (fixed now, so the answer cannot be chosen after the fact):

Collapse to one embedder **only if all three hold**:

1. **Text does not lose:** `Qwen3-VL-Embedding-2B` on text is within −0.015 of
   `Qwen3-Embedding-0.6B` on *both* MIRACL-id-200 and hotpot-200-test, with the lower bound of
   the paired 95% CI no worse than −0.02.
2. **Pages do not lose:** the unified space reaches page nDCG@10 ≥ **0.695** on ViDoRe HR (within
   0.01 of the shipped two-space 0.705), with the same page reranker.
3. **Ingest does not fall over:** text embedding throughput at **≥ 60%** of the 0.6B model's
   chunks/s, so the 943-page case stays under 200 s.

Else keep two embedders and record the number that blocked it. Stretch: page nDCG@10 > 0.710
and a single query embedding.

## Depth

**Expert move not asked for.** Measure the *text* side of a multimodal embedder, which nobody has
done here — the model was chosen for pages and assumed adequate for text. Also measure what the
collapse buys: one query call instead of two, and the deletion of a cross-validated fusion
weight that no longer needs tuning.

**Raw material.** The real corpora already in `~/kbdata/bakeoff/datasets`: `vidore3-hr-en`
(14 PDFs, 1,110 pages, 318 queries, qrels), `vidore3-hr-en-pages` (rendered pages),
`miracl-id-200` (2,933 passages, 200 queries), `hotpot-200-test` (995 paragraphs, 100 queries),
`vidore3-cs-en` for a second domain. Not synthetic, not a summary.

**Instruments.** `kb-eval`'s metrics (nDCG@10, Recall@10, MRR@10) and its paired bootstrap
(10,000 resamples, seed 42); `page_image_arm.py`'s embedding cache (model + content hash, so
reruns are free); `nvidia-smi` dmon for VRAM; wall-clock chunks/s and pages/s.

**Outside validators.** (a) Reproduce three committed numbers before trusting any new one;
(b) score every run with `breakdown.py` independently of `kb-eval` and require agreement;
(c) 0 failed and 0 degraded queries, or the run is reported as failed; (d) any tunable (blend
weight, k) is chosen on even queries and reported on odd ones.

## Assumptions to break

1. **"A 2B VL model embeds text as well as a 0.6B text specialist."** Test: MIRACL-id-200 and
   hotpot-200-test, text-only, paired bootstrap. This is the assumption the whole collapse rests
   on, and the one most likely to be false.
2. **"One space removes the fusion weight."** Test: with both arms in one space, compare a single
   index against the two-space tuned fusion (w≈0.5, k=5) on the same page run. If the single
   index needs its own weight against text, the simplification is smaller than claimed.
3. **"Text embedding cost is irrelevant because pages dominate."** Test: count. The 943-page PDF
   is ~3,771 text chunks against ~945 pages; text is the dominant workload, so a 2B model there
   costs ~3× the FLOPs on 4× the units.

## Plan

Models run on the **ThinkStation PGX (GB10, 128 GB unified) over Tailscale**; the harness runs
here and talks HTTP, which is how `eval-ab/images` was measured. Offline first: no stack, no
Qdrant, because the embedding cache makes reruns free and the last two page-arm findings were
first measured this way and then confirmed live.

| # | Step | Check |
|---|---|---|
| 0 | Start two pooling servers on the box: `Qwen/Qwen3-VL-Embedding-2B` and `Qwen/Qwen3-Embedding-0.6B` under vLLM, both `--runner pooling`, on separate ports | `GET /v1/models` answers for both; a 2048-d and 1024-d vector come back |
| 1 | **Done 2026-11-17:** `rust/crates/kb-eval/scripts/embed_ab.py` — two models over the same items, both request shapes (OpenAI `input` and vLLM chat `messages`), one vector space option, disk cache, TREC out, paired CI. `--self-test` covers both shapes, cache reuse, an image payload, scoring and the CI. Dry run over the real ViDoRe HR set: 318 queries, 1,110 pages with text and image, `one-space` merge, `summary.json` | `python3 …/embed_ab.py --self-test` passes; the dry run wrote 1,110 page vectors and 3 runs |
| 2 | Reproduce the committed baselines on this harness: 0.6B text on MIRACL-id-200 (0.830 with bge) and pages-only on ViDoRe HR (0.575 image-only, from the offline run) | Within 0.01 of the committed number; if not, stop and fix the harness |
| 3 | **A — text quality.** MIRACL-id-200 and hotpot-200-test, text-only, 0.6B vs 2B, rerank off and bge on | Paired Δ and CI per set |
| 4 | **B — unified space.** ViDoRe HR with text chunks *and* pages both embedded by the 2B into one index; query as text | Page nDCG@10 vs 0.645 text-only and 0.705 two-space |
| 5 | **C — cost.** chunks/s and pages/s for both models, VRAM peak, one query round trip | The 60% floor; one query call versus two |
| 6 | Reproduce the winning run live, in the engine, with `KB_PAGE_EMBEDDING_FORMAT=vllm` and the text endpoint on the same model | `kb-server eval` agrees with the offline score within 0.01 |
| 7 | Record: `docs/eval-ab/one-embedder/` with runs, numbers and the decision; update `docs/kb-tiers.md` §"The one-model-per-role rule" and the README model table **only** if the rule fires | The decided default is the one the preset ships |

Every step is a separate commit; step 2 must pass before step 3 is believed.

### Running it (once the GPU box is reachable)

```sh
# on the ThinkStation: the two embedders, one port each
vllm serve Qwen/Qwen3-Embedding-0.6B --runner pooling --port 8201 --max-model-len 8192
vllm serve Qwen/Qwen3-VL-Embedding-2B --runner pooling --port 8202 --max-model-len 8192 \
     --gpu-memory-utilization 0.30

# A — text quality, the engine's own chunks (MIRACL-id-200)
python3 rust/crates/kb-eval/scripts/embed_ab.py \
  --dataset ~/kbdata/bakeoff/datasets/miracl-id-200 --chunks ~/rkdump/chunks.jsonl --mode text \
  --model qwen06b=http://127.0.0.1:8201/v1/embeddings#openai \
  --model qwenvl2b=http://127.0.0.1:8202/v1/embeddings#chat \
  --out docs/eval-ab/one-embedder/miracl --cache ~/.cache/kb-eval/embed-ab

# A — the same on English (hotpot-200-test), and on ViDoRe HR page text
# B — one space: every page's text and image with the 2B
python3 rust/crates/kb-eval/scripts/embed_ab.py \
  --dataset ~/kbdata/bakeoff/datasets/vidore3-hr-en \
  --pages ~/kbdata/bakeoff/datasets/vidore3-hr-en-pages/pages.jsonl --mode both \
  --model qwenvl2b=http://127.0.0.1:8202/v1/embeddings#chat \
  --out docs/eval-ab/one-embedder/vidore-hr --cache ~/.cache/kb-eval/embed-ab
```

Two approximations to state in the write-up, both in the *optimistic* direction for the
collapse: (1) experiment B ranks **pages** by their best vector, while the production text
index holds **chunks** — so its text arm is coarser than the shipped 0.645; (2) the offline
run has no BM25 or neighbour arm. Both are corrected by step 6, which reproduces the result in
the engine.

## Validation

```
python -I rust/crates/kb-eval/scripts/breakdown.py ...        # independent scorer
python -I rust/crates/kb-eval/scripts/score_deltas.py ...     # paired bootstrap, 10k, seed 42
kb-server eval --dataset ~/kbdata/bakeoff/datasets/vidore3-hr-en ...   # live confirmation
```

A run with any failed or degraded query is reported as failed, not as a low number.

## Out of scope / risks

- **No 8B run in the first pass.** The 2B decides whether the design is viable at all; the 8B
  (`station`) is only worth measuring if the 2B passes, and its FP8 checkpoint makes the VRAM
  question different.
- **The rerankers stay split** (bge/voyage for text, Qwen3-VL-Reranker for pages). Unifying them
  is `EVAL-5` and a different experiment.
- **Desk cannot collapse either way:** a 2B at FP16 is 4.26 GB on an 8 GB card that also holds
  the app's chat model. This is a Studio/Station decision.
- **One English domain.** ViDoRe HR is EU labour reports; CS is a second domain but the same
  language. MIRACL-id is Indonesian text, which covers the text side.
- **Ingest shape risk.** vLLM's Chat-Embeddings API cannot batch pages, and possibly not text
  either. If text needs one request per chunk, throughput may fail rule 3 — which is a result,
  not a nuisance: it says the local design cannot collapse until vLLM batch-embeds text.
- **A collapse is a re-index**, not a config flip: the vector width is fixed when the collection
  is created (1024-d text → 2048-d unified), so shipping it means a wipe-and-re-embed and a
  migration note in `docs/rust-cutover.md`.
- **Hardware caveat.** GB10 unified memory does not behave like a discrete 24/48 GB card;
  timings transfer as ratios, not absolute numbers.
