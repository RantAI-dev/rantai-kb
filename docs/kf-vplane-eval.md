# Should rantai-kb replace moco-kf's V-plane? An evaluation plan

Asked 2026-10-21: wire rantai-kb (Rust) into moco-kf as the engine behind its V-plane, but
evaluate first. This document is the plan and, once run, the record of the result.

## Goal

- **Literal ask:** replace KF's V-plane (`services/retrieval/vector_plane.py`: fastembed
  `multilingual-e5-large` + `Qdrant/bm25`, Qdrant RRF) with rantai-kb.
- **The goal behind it:** KF's Knowledge Packs serve the right Knowledge Unit first, under
  KF's own latency budget, with KF's governance untouched.
- **The decision this informs:** swap, don't swap, or swap only one part (for example the
  embedder) and keep the rest.

KF's governance stays in KF. The router, the OPA selection gate, citations, the activation
scope and traces don't move. rantai-kb would sit below the `VectorPlane` protocol
(`plane_protocol.py`), which already exists so the engine behind it can be swapped.

## Embeddings come from OpenRouter (decided 2026-10-21)

So the evaluation does not tie up this box's CPU, every dense embedding is fetched from
OpenRouter, for both engines. KF's own model is on OpenRouter (`intfloat/multilingual-e5-large`),
and so is production's (`qwen/qwen3-embedding-8b`). Both cost $0.01 per million tokens. One
pass over the 2,933 passages is about 0.4M tokens, so the whole evaluation costs **under
$0.05**. KF's sparse side (`Qdrant/bm25`) stays local: it is a tokenizer + stemmer, not a model.

What this changes:
- KF in production embeds in-process with fastembed (ONNX). Here, KF's dense half is the same
  model behind an HTTP call. Pooling may differ slightly between fastembed and OpenRouter's
  host. The test compares engines, so both sides use the same served model.
- A call to OpenRouter takes about 1 s and dominates end-to-end time on both sides. So the
  latency rule below compares **search time without the embedding call**, and reports the
  end-to-end total beside it.
- moco-kf is evaluated at `origin/main` `e7f4d0b` (pulled 2026-10-21), from a separate
  worktree. Its V-plane code and collection spec are used unchanged.
- rantai-kb runs as a **release** build. The stack script used to start a debug build, which
  would have penalised it.

## Target (decision rule, fixed before any number is seen)

Replace the V-plane **only if both hold** on `miracl-id-200`, against the **better** of
KF-as-is and KF-with-e5-prefixes:

1. **Quality:** rantai-kb nDCG@10 is non-inferior. The paired-bootstrap 95% CI lower bound of
   (rantai-kb − KF) is ≥ −0.02. Recall@10 and Hit@1 are reported beside it.
2. **Latency:** the search-only p95 (embedding call excluded, warm process) at concurrency 1
   is not worse than KF's, and the end-to-end p95 stays under KF's 150 ms target once the
   embedding call is subtracted.

If quality wins and latency loses (or the reverse), the result is a partial swap, recorded with
the numbers. **Stretch:** nDCG@10 +0.03 over the better KF variant, with the CI excluding zero.

## Depth

- **Expert move:** compare at the plane boundary, on identical units, and attribute the
  difference to its cause: the engine (Rust vs Python + Qdrant), the embedder (Qwen3-0.6B vs
  e5-large), and the sparse tokenizer (our Indonesian-aware one vs `Qdrant/bm25`). That needs
  ablations, not only a head-to-head.
- **Raw material:** the MIRACL passages and human qrels; every contestant writes a TREC run
  file; the latency data are per-query timings, not averages.
- **Instruments:**
  - `kb-eval score` and `ranx` (two independent scorers that must agree);
  - `score_deltas.py` for paired bootstrap CIs;
  - per-query timing at concurrency 1 and 8.
- **Outside validator:** MIRACL's human relevance labels, and `ranx` re-scoring every run file.

## Assumptions to break

1. **"KF's V-plane is a fair baseline as built."** It isn't. fastembed's `query_embed` for
   `multilingual-e5-large` adds no `query:`/`passage:` prefix: `PooledEmbedding` doesn't
   override the base method, which just calls `embed`. So KF runs e5 off its training format.
   *Test:* score KF both as-is and with the prefixes. The prefixed score is KF's fair
   baseline, and the gap is a KF bug worth reporting either way.
2. **"rantai-kb is better because of the engine."** Our earlier gains came from the keyword
   arm and fusion. *Test:* run rantai-kb with e5-large served by TEI. If it matches
   rantai-kb + Qwen3, the embedder isn't the cause.
3. **"rantai-kb is fast enough."** On this CPU box, rantai-kb pays about 340 ms to embed a new
   query through TEI, while KF's in-process ONNX e5 takes about 93 ms. *Test:* plane-level
   p95 with a cold query cache. If the embedding service dominates, the fix is how the
   embedder is served, not the engine.

## Plan

1. **Units:** export `miracl-id-200` (2,933 passages, 200 queries, 640 qrels) as one unit
   per passage, with `unit_id` = MIRACL docid. Median 357 chars; 215 of 2,933 are > 800 chars,
   so rantai-kb splits those into 2+ chunks. Scoring is per passage on both sides, so this is
   recorded rather than hidden.
2. **KF arm**, a script run in KF's own venv against a throwaway Qdrant collection, calling
   KF's `LocalEmbedder` and `vector_plane.hybrid_search` unchanged:
   - KF-as-is;
   - KF-with-e5-prefixes, with the same code and the prefixes added.

   No KF source is modified for the evaluation.
3. **rantai-kb arm:** `kb-server` on Qdrant, ingesting the same passages:
   - rantai-kb + e5-large, with prefixes. The same embedder as KF, so this compares engines;
   - rantai-kb + qwen3-embedding-8b (production's model);
   - KF + qwen3-embedding-8b, so a better embedder alone is not credited to the engine.
4. **Score:** `kb-eval score` + `ranx` per run, plus paired deltas against the better KF
   variant.
5. **Latency:** each arm's plane-level timings at concurrency 1 and 8, cold and warm query
   cache.
6. **Contract gap list:** what rantai-kb lacks to sit behind `VectorPlane`:
   - external unit ids (`ku_id`) with no re-chunking;
   - filters on `tenant_id`, `status`, `language` and `dataset_ids`;
   - payload patches (`permission_scope`, `dataset_ids`) without re-embedding;
   - idempotent upsert keyed by unit id.
7. **Decision**, recorded here with the raw outputs committed under
   `docs/kf-vplane-eval/`.

## Out of scope / risks

- No KF code changes until the decision. If the answer is "swap", the wiring is a separate,
  reviewed step on a KF branch.
- MIRACL is Wikipedia passages, not KF's books and regulations. KF's own `indonesian-core`
  set has queries but no gold KUs, so it can't score anything yet.
- One CPU box with 11 GB. Arms run one at a time. Ingest is the long pole (hours of CPU), and
  costs nothing.

## Result (2026-10-21): don't swap

`miracl-id-200`: 200 queries, 2,933 passages, 640 qrels, all 2,933 indexed by every arm, 0
failed queries. moco-kf is `origin/main` `e7f4d0b`; rantai-kb is `feat/kb-sota` `13c957e`
(release). Dense embeddings come from OpenRouter for every arm. Raw runs, latencies and reports
are in `docs/kf-vplane-eval/`. `ranx` reproduces every number below exactly.

| Arm | Dense model | nDCG@10 | Recall@10 | MRR@10 | Hit@1 |
|---|---|---:|---:|---:|---:|
| KF as built (`main`) | e5-large | 0.778 | 0.934 | 0.802 | 0.690 |
| KF + IDF modifier | e5-large | 0.812 | 0.966 | 0.820 | 0.720 |
| KF + e5 prefixes | e5-large | 0.780 | 0.937 | 0.802 | 0.690 |
| **KF + both fixes** | e5-large | **0.813** | 0.964 | **0.824** | **0.725** |
| KF + IDF, production model | qwen3-embedding-8b | 0.809 | 0.943 | 0.822 | 0.720 |
| **rantai-kb** | e5-large (+ prefixes) | 0.796 | 0.961 | 0.791 | 0.670 |

**Paired deltas, rantai-kb − KF fixed** (10,000 bootstrap resamples, seed 42):

| Metric | Delta | 95% CI |
|---|---:|---|
| nDCG@10 | −0.017 | [−0.036, +0.001] |
| MRR@10 | −0.033 | [−0.059, −0.008] |
| Recall@10 | −0.003 | [−0.024, +0.017] |

Against KF as built, rantai-kb is ahead: nDCG +0.018 [−0.000, +0.037], Recall@10 +0.027
[+0.008, +0.046].

**Latency, search only** (embedding call excluded), concurrency 1:

| | p50 | p95 |
|---|---:|---:|
| KF fixed (one Qdrant query) | 8.8 ms | 17.7 ms |
| rantai-kb, both arms | 28.8 ms | 37.9 ms |
| rantai-kb, whole engine (arms + neighbours + titles + figures) | 34.4 ms | 45.0 ms |

The OpenRouter embedding call (about 1 s median, with multi-second tails) dominates end-to-end
time on both sides. It is not a property of either engine.

**Verdict against the rule set before the run:**
1. Quality fails. The CI lower bound is −0.036, below −0.02. rantai-kb finds the same passages
   (Recall@10 equal) but ranks the right one first less often (MRR −0.033, significant).
2. Latency fails. Search p95 is 37.9–45 ms against KF's 17.7 ms.

So KF's V-plane stays. Two findings carry over regardless:
- **KF should take the IDF fix now**: +0.034 nDCG, one line, `"modifier": "idf"` on the
  `sparse` vector in `src/infra/qdrant/collections/knowledge_units.json`. That needs a
  re-index, since a modifier can't be added to an existing collection.
- The e5 prefixes and the larger qwen3-8b embedder do not move KF's score on this set.

**Why rantai-kb loses at the top: an ablation (2026-10-22).** Both engines' rankings were
rebuilt offline from the same vectors, so each design difference could be switched on alone
(`rust/crates/kb-eval/scripts/kf_vplane_ablate.py`).
- rantai-kb's chunks and keyword vectors come from `kb_core` itself, through the
  `scripts/rkdump` helper; nothing is re-implemented.
- Dense vectors come from OpenRouter e5-large and cost about $0.02 in total.
- Keyword scores use Qdrant's IDF formula.
- The model reproduces KF fixed exactly (nDCG 0.813). It puts rantai-kb at 0.802 with the
  live tie-break, against 0.796 measured live, so 0.006 is left unexplained (see below).
- `ranx` agrees with every number.
- Full table and runs: `docs/kf-vplane-eval/ablation/`.

| Change, one at a time | nDCG@10 | MRR@10 | Hit@1 |
|---|---:|---:|---:|
| KF fixed (reproduced) | 0.813 | 0.821 | 0.720 |
| **KF, fused the way rantai-kb fuses (k=60 re-sort)** | **0.803** | **0.804** | **0.700** |
| KF on rantai-kb's chunks | 0.813 | 0.821 | 0.720 |
| KF with rantai-kb's keyword encoder | 0.814 | 0.824 | 0.725 |
| KF with the file stem in the dense text | 0.811 | 0.818 | 0.715 |
| KF with 20 candidates per arm instead of 50 | 0.811 | 0.821 | 0.720 |
| rantai-kb (reproduced, live tie-break) | 0.802 | 0.801 | 0.690 |
| **rantai-kb, fused the way Qdrant fuses (k=2)** | **0.809** | **0.817** | **0.715** |
| rantai-kb + k=2 + the article title instead of the file stem + 50 per arm | 0.817 | 0.830 | 0.740 |

What this shows:
1. **The cause is fusion.** `kb-store-qdrant` asks Qdrant for its fused list (RRF, k=2), then
   re-sorts it with k=60. A large k flattens the difference between rank 1 and rank 3, so a
   passage both arms put near the top no longer clearly beats one only a single arm liked.
   That costs about 0.01 nDCG and 0.017 MRR, in either engine. It also accounts for the whole
   significant MRR gap.
2. **The other suspects are innocent:**
   - chunking (KF on our chunks scores the same);
   - our keyword encoder (it is slightly better than `Qdrant/bm25`);
   - the candidate pool size;
   - the tie-break.
3. **A second, smaller issue:** the eval uploaded passages without a title, so rantai-kb
   embedded the file stem (`1000580_1`) before every chunk. It also drops the article title
   from every chunk after the first. 639 of 2,933 passages are split. Replacing the stem with
   the real title helps only once fusion is fixed. Together the two changes reach 0.817 / Hit@1
   0.740. That is above KF fixed, but not significantly: +0.005 [−0.004, +0.014].
4. **0.006 is still unexplained** between the offline model (0.802) and the live run (0.796).
   It comes from something only the live engine does. Candidates are approximate HNSW search
   and extraction of the `.md` file. The fix has to be re-measured live.

So rantai-kb with k=2 fusion and real titles would plausibly tie KF on quality. It would still
lose on latency, which this ablation does not address.

**Not measured:** rantai-kb with `qwen3-embedding-8b`. KF's engine gained nothing from it here,
and a rantai-kb ingest takes about 2.5 hours through the job pipeline (about 22 passages/min),
so it was skipped.
