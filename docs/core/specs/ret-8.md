# `RET-8` — our own CPU page embedder, in Nemotron's space

Asked by the product owner, 2026-11-18: an embedding model of our own that runs on CPU, reads
**page images**, and retrieves Indonesian and English documents well. Training on the
ThinkStation (GB10). Inference in Peregrine. Research: [`docs/research/cpu-page-embed/`](../../research/cpu-page-embed/)
(`A-recipes.md`, `B-backbones.md`, `C-data-eval.md`, written by GLM-5.3 researchers; the claims
this plan rests on were re-checked against the primary sources, see "Verified" below).

## Goal

- **Literal ask:** a CPU page-image embedder that is good on Indonesian and English documents.
- **Goal behind it:** Lite and other CPU-only deployments get the page arm that the GPU tiers got in
  0.6.0. Pages with charts, tables, forms and scans are found by what they *look like* and say,
  not only by their chunked text.
- **Chosen level:** a student that writes vectors **into `Nemotron-3.5-Embed-1B`'s 2048-d space**,
  not a new space of its own. Three things follow from that:
  - one query embedding searches pages from every tier;
  - a corpus indexed on a GPU tier and one indexed on Lite are the same index;
  - the GPU tiers' measured quality is the reference the student is graded against.
- **Not chosen (yet):** a new space that might score higher, or multi-vector. Both lose the shared
  space, and multi-vector costs ~1,000 vectors per page in Qdrant.

## Target

**Baseline, measured.**

- Teacher (`Nemotron-3.5-Embed-1B`, bf16, GB10) on ViDoRe V3 HR, page nDCG@10: **0.626** from the
  image, **0.611** from the page text (`docs/eval-ab/one-embedder`).
- Lite's page path today: **0.545** (e5-small, end to end).
- The teacher's text path on CPU through Peregrine 0.3.0 (Q8_0, Xeon E5-2680 v4, AVX2, pinned
  cores, about 1,000 tokens per page):

| Cores | Tokens/s | Seconds/page | Load average during the run |
|---|---:|---:|---|
| 2 | 40.7 | 25.4 | 5–6 |
| 4 | 71.5 | 14.5 | 5–9 |
| 8 | 97.1 | 10.3 | 9–28, so pessimistic |

At 2 cores that is about 40× slower than Lite's ingest. It is the reason for this project.

**Baseline, measured in step 0.** The teacher on MIRACL-Vision: Indonesian image **0.654**, text
0.769; English image **0.735**, text 0.809. The target row "share of the teacher's image score"
uses these image numbers. See "Step 0 results" below.

| | Target | Stretch |
|---|---|---|
| Quality: page nDCG@10 as a share of the teacher's *image* score, on MIRACL-Vision-id, MIRACL-Vision-en, ViDoRe V3 HR and `IndoDocEval-v0` | ≥ 90% on each (≥ 0.563 on HR) | ≥ 95% |
| Quality: page nDCG@10 against Lite's current page path on `IndoDocEval-v0` | ≥ +0.05 | — |
| Cost: per page, Peregrine, 2 pinned cores of the Lite test box | ≤ 3 s (8× faster than the teacher's text path) | ≤ 1 s |
| Cost: per page, 8 cores | ≤ 1 s | — |
| Cost: per query, 2 cores | ≤ 300 ms | ≤ 100 ms |
| Parity: Peregrine against the PyTorch reference | cosine ≥ 0.999 per page | — |

## Depth

- **The expert move nobody asked for.** Grade the student on *our* inputs: the text that Lite
  actually has for a page. That is the pdfium text layer, or `ocr-cpu` output for a scan.
  Clean ground-truth text is not what Lite sees. Also train and test on degraded renders (blur,
  skew, JPEG, low dpi), because Lite's hard pages are scans.
- **Raw material.** Rendered pages and the teacher's own vectors for them, cached by model and
  content hash. No labels are needed for distillation: the target is the teacher's image vector.
- **Instruments.**
  - `embed_ab.py` (nDCG@10, MRR@10, Recall@10, paired bootstrap CIs).
  - PyTorch CPU timing on `taskset`-pinned cores before any engine work.
  - `peregrine bench`, and a per-page cosine parity harness.
- **Outside validators.**
  - MIRACL-Vision (human-written queries, NVIDIA).
  - ViDoRe V3 (human-verified queries).
  - `IndoDocEval-v0`: a held-out set we build and freeze *before training*, with a 200-query
    subset checked by a person.

## Assumptions to break

1. **The teacher is good on Indonesian pages.** There is only indirect evidence: the card reports
   an 18-language average, and MIRACL-Vision has an `id` split, verified on the dataset card.
   *Test (step 0):* the teacher's image and text nDCG@10 on MIRACL-Vision-id against bge-m3 on the
   page text. If the teacher is not clearly ahead, distilling it into Indonesian is the wrong plan.
2. **A pixel-only student cannot read small text inside a CPU budget, but text plus a low-res image
   can.** The literature's resolution curve is steep: ModernVBERT scores 30.7 at 512 px against
   45.8 at 2048 px with a cooldown. And ~1,000 SigLIP2-B patches is only about 55 dpi on A4.
   *Test (step 2):* train three students on the same 20k pages and compare quality per CPU second.
   - **S0, text only.** A small ModernBERT encoder over the page text, plus an MLP to 2048-d.
   - **S1, text plus a low-res image.** S0 with SigLIP2-B/16 NaFlex at about 576 patches; the
     pixel-shuffled image tokens are prepended to the text tokens, ModernVBERT-style early fusion.
   - **S2, pixels only.** SigLIP2-B/16 at about 1,024–2,048 patches plus the same encoder; no OCR.
3. **If S0 already reaches 90%, the vision tower is not worth building in Peregrine.** That would be
   the cheapest win, and the plan must be willing to take it. Pages without a text layer still go
   through `ocr-cpu` first.
4. **Cosine alignment to cached teacher vectors works with a Pixtral-based teacher.** NanoVDR and
   DistilVDR used Qwen-VL teachers. *Test:* the 20k-page pilot must reach ≥ 80% retention before
   we scale to 300k.
5. **FLOP estimates turn into Peregrine seconds.** *Test (step 0):* time untrained S0, S1 and S2
   in PyTorch on 2 pinned cores. Cost depends on the architecture, not the weights, so this needs
   no training.

**Cost estimates (unmeasured).** These use ≈ 90 GFLOP/s effective on 2 cores, inferred from the
40.7 tok/s above:

| Student | Work per page | Time on 2 cores |
|---|---|---|
| S0, granite-97m-r2 (≈ 28M non-embedding parameters) over ~1,150 tokens | ≈ 65 GFLOP | ≈ 0.7 s |
| S1, adds SigLIP2-B at 576 patches | ≈ 180 GFLOP | ≈ 2 s |
| S2, SigLIP2-B at 2,048 patches | ≈ 450+ GFLOP | ≈ 5 s, over budget unless it earns it |

## Step 0 results (measured 2026-11-18, GB10)

Run by GLM-5.3 on the ThinkStation. The raw `summary.json` files were re-read and the page counts
re-checked by Claude. Full report, scripts and metrics:
[`docs/eval-ab/ret8-step0/`](../../eval-ab/ret8-step0/STEP0.md).

**The serving setup reproduces the earlier GPU eval exactly.** ViDoRe HR scored text 0.611, image
0.626 and one-space 0.621, the same as the committed numbers.

**MIRACL-Vision, page nDCG@10, full corpora (no pages dropped):**

| Model, input | Indonesian (603 queries, 23,842 pages) | English (447 queries, 42,971 pages) |
|---|---:|---:|
| Teacher, page text | **0.769** | **0.809** |
| Teacher, one space (text + image) | 0.762 | 0.812 |
| Teacher, page image | 0.654 | 0.735 |
| bge-m3, page text | 0.740 | 0.767 |
| multilingual-e5-small, page text (Lite's model) | 0.711 | 0.729 |
| granite-embedding-97m-multilingual-r2, page text | 0.671 | 0.716 |

**Text passages, MIRACL-id-200:** teacher **0.829** against granite **0.756**, paired
Δ **+0.073** [+0.052, +0.095].

**Assumption 1 holds.** The teacher's text arm beats bge-m3 on Indonesian by
**+0.030** [+0.015, +0.044], and on English by +0.042.

**Caveat on what the text arm measured.** MIRACL-Vision's page text is a clean Wikipedia passage
(median 375 characters), not OCR output. "Text beats image" here therefore says little about Lite's
scanned pages. The image arm (0.654 Indonesian) is the honest pixel-only number, and it is below
every text model on clean text. On scans, where Lite's text is worse, that ordering can flip. This
is why `IndoDocEval-v0` must include `ocr-cpu` text for scans.

**What this changes in the plan:**

- **The student's text tower is not granite.** It is 0.07–0.10 behind the teacher on Indonesian, and
  Peregrine 0.3.0 cannot load it (unsupported pre-tokenizer; this run served it with llama-server).
  The candidates now are mmBERT-small or mmBERT-base (ModernBERT, which Peregrine runs) and
  multilingual-e5-small (0.711, already in Lite).
- **Grade the students against the teacher's text arm too.** On clean text it is the strongest
  arm, so the S0 student (text only) has a high, real target.

**Teacher cost on the GB10.** Throughput measured on 200 ViDoRe HR pages, with the model holding
11.4 GB of GPU memory:

| Input | Pages/s | Requests in flight |
|---|---:|---:|
| Text | 50.6 | 8 or more |
| Image | 5.38 | 8 |

At those rates, 300k pages with both text and image take ≈ **17 GB10-hours**: 1.65 h for text and
15.5 h for images. Step 1's targets cost a day of the box, not a week.

**CPU parity: Peregrine runs the teacher faithfully, so speed is the only problem.**

- Peregrine 0.3.0 Q8_0 scores 0.611 on ViDoRe HR page text, the same as the GPU's 0.611.
- Cosine against the GPU vectors over 20 pages: mean **0.99987**, min 0.99976.
- The vectors came from two machines (800 pages on x86, the rest on ARM), which shows the result
  does not depend on the CPU.

## Schedule (estimate, 2026-11-20; not measured)

About 4–6 weeks of wall-clock time until Lite ships the page arm. The only measured cost is the
teacher cache: about 17 GB10-hours for 300k pages.

| Step | Estimate | What bounds it |
|---|---|---|
| 0, rest (CPU timing of S0/S1/S2, text tower) | 1 day | — |
| 1. Data + `IndoDocEval-v0` | 1–2 weeks | PDF fetch and render (FinePDFs dead-link rate unknown), 17 GB10-hours, the 200-query human check |
| 2. Pilot, 20k pages | 2–3 days | a few training hours per student, plus eval |
| 3. Full training, 300k pages | 3–5 days | 1–2 GPU days, eval, likely one rerun |
| 4. Query side | 1 day if the teacher meets 300 ms; about 1 week for a query student | — |
| 5. Peregrine | S0: a few days; S1/S2: 1–2 weeks (NaFlex ViT, image input) | the Peregrine developer's time; starts in parallel once step 2 picks the student |
| 6. rantai-kb | about 1 week | — |

- **Faster:** S0 wins (almost no new Peregrine work), giving about 3 weeks.
- **Slower:**
  - Xiaomi OCR must be off while the GB10 runs the teacher or training (one model role at a time).
  - A failed step-2 gate (below 80% retention) adds about 1 week.

## Range

| Field | Method borrowed |
|---|---|
| Knowledge distillation (NanoVDR, DistilVDR, LEAF, Nemotron's own 8B → 1B) | **pointwise cosine loss to cached teacher vectors**. It beat KL-ranking and InfoNCE across 22 ViDoRe sets, and adding contrastive on top gave nothing (NanoVDR, DistilVDR abstracts, verified) |
| IR evaluation | frozen held-out sets, doc-level splits, paired bootstrap |
| Document engineering | train on `ocr-cpu` text and degraded renders, not clean text |
| Systems | measure cost before building; parity harness between the training and inference engines |

## Plan

Each step names its check, and each is committed separately.

### 0. Baselines and go/no-go (ThinkStation, ~2 days)

1. Free the GPU. Stop our `xiaomi-vllm` (:8192) and the Qwen3-Embedding vLLM (:8201). Leave the
   Peregrine servers on 18397–18399 alone. Run one model role at a time, use
   `systemd-run --user`, and never use `/dev/shm`.
2. Measure the teacher's speed on the GB10 in pages/s, image and text. This prices step 1's cache.
3. Run the teacher on MIRACL-Vision-id and MIRACL-Vision-en, image and text. Baselines: bge-m3 on
   the text, and Lite's e5-small. **Check: assumption 1.**
4. Time the untrained S0, S1 and S2 architectures on the Lite box, on 2, 4 and 8 pinned cores.
   **Check: assumption 5.** Drop any variant over 3 s on 2 cores unless step 2 shows it worth it.
5. Choose the text tower: granite-embedding-97m-multilingual-r2 (Apache-2.0, ModernBERT, 180k
   vocabulary, 32k context) or mmBERT-small (MIT, ModernBERT, Gemma 2 tokenizer). Pick by
   MIRACL-id text nDCG@10; granite's Indonesian score is **unverified**, because its card gives
   none.

### 1. Data, then freeze the eval (ThinkStation, ~1 week)

**Pages, about 300k.**

- About 200k Indonesian:
  - Indonesian Wikipedia rendered to pages (CC-BY-SA).
  - FinePDFs `ind_Latn` URLs fetched and rendered (2.32M documents per the report; the dead-link
    rate is unknown).
  - Government PDFs: JDIH, BPS and OJK.
- About 100k English: Docmatix (MIT), and the ColPali and VisRAG training pages.
- About 15% rendered as degraded scans.
- PDFs are used for training only and never redistributed. Redistributing government documents
  needs a legal check under UU 28/2014.

**Inputs per page.** The pdfium text, and `ocr-cpu` text for scanned and degraded pages: the
inputs Lite really has.

**Targets per page.** The teacher's image vector and text vector (2048-d), cached by model and
content hash. AGENTS.md rule 7: no page is paid for twice.

**`IndoDocEval-v0`, built and frozen before any training.**

- About 600 queries over about 10k pages.
- Held-out documents, split by document, not by page.
- At least 10% scans.
- Queries in Indonesian and English, generated locally (Qwen3-VL-8B on the GB10).
- Queries kept only if the teacher ranks their source page in its top 10.
- 200 queries checked by a person.
- Leakage controls: document hashes, perceptual-hash dedup against every eval corpus (MIRACL-Vision,
  ViDoRe), and MinHash dedup between training and eval text.

### 2. Pilot distillation on 20k pages (S0, S1, S2)

- Loss: 1 − cos(student, teacher image vector). The teacher stays frozen and only cached targets
  are used.
- Train each student on the same 20k pages.
- **Check:** retention on the step-0 sets with paired CIs, and quality per CPU second from step 0.4.
- **Go/no-go:** the best variant reaches at least 80% retention. If not, change the student before
  spending on 300k pages.

### 3. Full training of the winner, 300k pages

- Keep the full 2048-d output, so the shared space holds; no Matryoshka cut.
- Also try the teacher's *text* vector as an auxiliary target: a page's two teacher vectors are
  within ±0.015 of each other, so the student can learn from both.
- **Check:** every target in the table above, with CIs.

### 4. The query side

1. Measure the teacher's query cost on CPU through Peregrine: short queries, 2 cores. If it is
   ≤ 300 ms, keep the teacher for queries; that is no quality loss and the exact space.
2. If it is over, distill a NanoVDR-style query student into the same space. Train it on
   Indonesian queries translated by a local model and re-encoded by the teacher. NanoVDR found
   cross-lingual transfer, not the modality gap, to be the bottleneck, and fixed it this way.

### 5. Peregrine (owner: the Peregrine developer)

1. Export the student to GGUF.
2. If S1 or S2 wins:
   - a plain ViT for SigLIP2 NaFlex, with variable aspect ratio and position-embedding
     interpolation;
   - pixel shuffle;
   - image inputs on `/v1/embeddings`, the same message shape kb-server already sends for
     `omniembed`.
3. The text tower is ModernBERT, which Peregrine already runs.
4. **Check:** cosine ≥ 0.999 against PyTorch on 200 pages, and `peregrine bench` against the cost
   targets.

### 6. rantai-kb

- A Lite page arm: Peregrine as the page embedder, run in the background queue like captions. The
  document is searchable before its page vectors land.
- Queries go through the teacher or the query student (step 4).
- Gate the release on `IndoDocEval-v0` plus the two public sets, in CI's eval gate.

## Validation

- `embed_ab.py` on MIRACL-Vision-id/en, ViDoRe V3 HR and `IndoDocEval-v0`, with paired bootstrap
  against the teacher and against Lite today.
- The 200 human-checked queries.
- A CPU timing script on pinned cores of the Lite test box: `taskset`, the load average recorded
  with every run, three rounds, medians.
- The Peregrine parity harness.

## Verified, 2026-11-18

**Checked against the primary sources:**

- DistilVDR (arXiv 2608.10636): 524M, single-vector, cosine loss, 61.74 average, 86.9% of an 8B
  teacher, 70M query side.
- NanoVDR (arXiv 2603.12824, EMNLP 2026): 69M, 95.1% retention, under 13 GPU-hours; cosine beats
  ranking and contrastive losses; cross-lingual transfer is the bottleneck, fixed with translated
  queries.
- MIRACL-Vision has `queries-id`, `corpus-id`, `qrels-id` and `images-id`, under CC-BY-SA-4.0.
- Licences:
  - SigLIP2 NaFlex B/16 and so400m: Apache-2.0.
  - mmBERT-small and base: MIT.
  - granite-97m-r2: Apache-2.0, 97.4M, 384-d.
  - Nemotron: OpenMDW-1.1, which says it "does not impose any restrictions … with respect to any
    use … of any outputs", so training on its vectors is allowed.

**Not verified:**

- granite's MIRACL-id 45.98 (not on its card).
- The GB10's training and caching throughput.
- Every CPU cost above that is not in the measured table.

## Out of scope and risks

**Out of scope:**

- multi-vector;
- the 8B teacher;
- training a vision tower from scratch;
- paid APIs;
- redistributing crawled PDFs.

**Risks:**

- The teacher may be weak on Indonesian pages. Step 0 finds out before any spend.
- S0 may win, which is good news, but then "page-image" means text plus OCR, and pages with no
  text and no OCR stay unembedded.
- The GB10 is shared with OCR and evals: one role at a time.
- The legality of crawled PDFs.
- Scans drift between `ocr-cpu` versions. The `ocr-cpu` version is part of the cache key.
