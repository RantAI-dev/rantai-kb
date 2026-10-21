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

## Target (decision rule, fixed before any number is seen)

Replace the V-plane **only if both hold** on `miracl-id-200`, against the **better** of
KF-as-is and KF-with-e5-prefixes:

1. **Quality:** rantai-kb nDCG@10 is non-inferior. The paired-bootstrap 95% CI lower bound of
   (rantai-kb − KF) is ≥ −0.02. Recall@10 and Hit@1 are reported beside it.
2. **Latency:** the plane-level p95 (query embedding + search, warm process, cold query cache)
   at concurrency 1 is under KF's own target of **150 ms** and not worse than KF's.

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
   - rantai-kb + Qwen3-Embedding-0.6B (current);
   - rantai-kb + e5-large via TEI (the ablation for assumption 2).
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
