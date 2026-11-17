# Benchmarks we have run

Every number below was measured, and its run folder is committed next to it with the script,
the queries and the per-query output. This page is the index: what was asked, what changed,
what the answer was, and whether it changed a default. If a measurement does not exist, it is
listed under [Not measured](#not-measured) rather than implied.

Confidence intervals are 95% paired bootstrap unless stated. "Shipped" means the change is in
today's defaults.

## Retrieval — English page documents (ViDoRe v3 HR)

318 queries, 14 EU reports, 1,110 pages, page-level nDCG@10.

| When | What changed | Result | Shipped | Run |
|---|---|---|---|---|
| 10-23 | Three PDF-page bugs (page boundaries, page→chunk map, page rendering) | 0.477 → **0.561** (+0.085 [+0.061, +0.109]); MRR 0.599 → 0.719 | yes | [`vidore3-hr-en`](eval-ab/vidore3-hr-en/) |
| 10-23 | Header/footer/signature stripping before chunking | −0.003 [−0.013, +0.006] — **no effect** | no, off | [`vidore3-hr-en-furniture`](eval-ab/vidore3-hr-en-furniture/) |
| 10-23 | Page-image arm, offline replay (voyage-multimodal-3.5) | 0.561 → **0.629** (+0.068) with fusion tuned to weight ≈0.5, k=5; images alone beat local text 0.575 vs 0.561. Cost $0.581 for the run | yes (k=5, w≈0.5) | [`vidore3-hr-en-image`](eval-ab/vidore3-hr-en-image/) |
| 10-23 | Same arm, live in kb-server | 0.572 → **0.625** (+0.053 [+0.036, +0.070]); the offline replay predicted 0.62–0.63 | yes | [`vidore3-hr-en-pages-live`](eval-ab/vidore3-hr-en-pages-live/) |
| 10-23 | Text+image page vectors plus a page-level reranker | 0.645 → **0.705** (+0.060 [+0.041, +0.079]); best published retriever 0.654, best published pipeline with reranker 0.665 | yes | [`vidore3-hr-en-night`](eval-ab/vidore3-hr-en-night/) |
| 10-23 | **Two page rerankers, independently** | voyageai/rerank-2.5 **0.704**, `nvidia/nemotron-3.5-rerank-8b:free` **0.705** (+0.049 [+0.031, +0.069], $0) — they agree; the blend weight and k were cross-validated | voyage (one key) | [`vidore3-hr-en-image`](eval-ab/vidore3-hr-en-image/) |
| 10-24 | Score blend instead of RRF for the page rerank | 0.700 / 0.702 vs RRF's 0.704 — **no gain**, and nemotron the same | no, RRF stays | [`vidore3-hr-en-image`](eval-ab/vidore3-hr-en-image/) |
| 10-23 | Rebuilding tables as text from ViDoRe's own markdown | Table questions 0.500 vs 0.506 — **no gain** | no | [`vidore3-hr-en-md`](eval-ab/vidore3-hr-en-md/) |

## Retrieval — ViDoRe v3 CS (computer science)

215 queries, 1,360 pages, page-level nDCG@10.

| When | What changed | Result | Shipped | Run |
|---|---|---|---|---|
| 10-24 | Cloud default (one voyage embedder for text and pages + voyage rerank) | **0.828** against 0.779 text-only (+0.049 [+0.035, +0.065]); voyage instead of qwen3-8b for chunks is a wash (−0.006 [−0.021, +0.006]) | yes | [`vidore3-cs-en-night`](eval-ab/vidore3-cs-en-night/) |
| 10-24 | The big-PDF ingest rewrite | 0.830 vs 0.828 (+0.001 [−0.000, +0.004]) — **retrieval unchanged** | — | [`ingest-big-pdf`](eval-ab/ingest-big-pdf/) |

## Retrieval — Indonesian and English text

| When | What changed | Result | Shipped | Run |
|---|---|---|---|---|
| 10-21 | Should rantai-kb replace moco-kf's V-plane? | **No.** nDCG 0.796 vs KF's fixed 0.813 (−0.017 [−0.036, +0.001]); MRR −0.033; search p95 37.9–45.0 ms vs KF's 17.7. KF's own IDF fix is worth +0.034 nDCG and should ship there | no swap | [`kf-vplane-eval`](kf-vplane-eval.md) |
| 10-23 | Reranker bake-off on an RX 590 (llama.cpp), top 20 | bge: MIRACL-id 0.793 → **0.830**, hotpot 0.820 → **0.928**. Qwen3-Reranker-0.6B is +0.019 on Indonesian, **−0.038 on English**, 2.4× slower | bge | [`eval-ab/README`](eval-ab/README.md) |
| 10-23 | Fusion fixes: k=2 instead of k=60; title+heading-path in the embedded text | k=2 **hurts**: nDCG −0.021, MRR −0.031. Title+heading: no measurable effect | no, k=60 | [`miracl-id-200`](eval-ab/miracl-id-200/) |
| 10-24 | Lite on 2 pinned cores: e5-small vs Qwen3-Embedding-0.6B | e5-small **0.802** nDCG, ~126 s ingest, 42 ms search; 0.6B 0.811 (a tie on 50 queries) but **86 min** ingest and 802 ms/search | e5-small | [`lite-2core`](eval-ab/lite-2core/) |
| 10-28 | Two Qdrant searches instead of three, and `KB_RRF_K` | Search stage 56.4 / 85.5 ms → **25.7 / 38.1 ms** at equal quality (0.7915 → 0.7894) | yes | [`qdrant-two-search`](eval-ab/qdrant-two-search/) |
| 10-23 | RRF k sweep, offline replay (100 tune / 100 test) | Every k from 20 to 60 within 0.002; k=60 kept | k=60 | [`eval-tune/miracl-id-200`](eval-tune/miracl-id-200/fusion.md) |

## Retrieval — cloud tier

| When | What changed | Result | Run |
|---|---|---|---|
| 10-31 | Cloud (voyage text+pages, voyage rerank) against KF main | MIRACL-id-200 **0.845** vs 0.813; ViDoRe HR 0.710 vs text-only 0.675; ViDoRe CS 0.822 vs 0.778. Spend $0.29 | [`rk-cloud-vs-kf`](eval-ab/rk-cloud-vs-kf/) |
| 11-04 | Every new retrieval option off (floor, recency, adaptive arms, tree, graph) | MIRACL 0.845 (unchanged), HR 0.711 against 0.710 — **nothing regressed**. Spend $0.48 | [`retrieval-gaps`](eval-ab/retrieval-gaps/) |

## Retrieval — arms and options, measured one at a time

| When | Arm | Result | Default today | Run |
|---|---|---|---|---|
| 11-04 | `RET-2` relevance floor at `minScore` 0.42 (rerank score) | Off-topic queries come back empty **90.9%** (MIRACL, 55) and **98.1%** (HR, 54), while in-domain nDCG is unchanged: 0.8380 → 0.8381 and 0.6398 → 0.6399 | off (`KB_MIN_SCORE` unset); 0.42 is a recommendation for this reranker, not a default | [`retrieval-gaps`](eval-ab/retrieval-gaps/) |
| 11-04 | `RET-4` recency boost | Dated-versions set 0.3455 → **0.9909** (+0.6455 [+0.6212, +0.6636]); MIRACL +0.0001 (neutral) | opt-in | [`retrieval-gaps`](eval-ab/retrieval-gaps/) |
| 11-04 | `EVAL-2` tree arm (navigate a document's outline) | 41 structural queries: +0.163 ungated, **+0.086 gated** by RET-5's classifier; regular HR unchanged when gated, **−0.193 if left ungated** | off (`KB_TREE_ARM`) — it needs the gate | [`retrieval-gaps`](eval-ab/retrieval-gaps/) |
| 11-04 | `EVAL-3` graph community summaries | **A loss** on aggregative questions: HR −0.045 to −0.067, CS −0.091 to −0.103, even with an oracle gate | off (`KB_GLOBAL_ARM`) | [`retrieval-gaps`](eval-ab/retrieval-gaps/) |

## Ingest

| When | What changed | Result | Shipped | Run |
|---|---|---|---|---|
| 10-24 | Concurrency and page-rendering fixes for a 943-page PDF (3,771 chunks, 945 page images) | 427 s → **163 s** with provider-like latency (target <200 s), and **71 s** at 16 requests in flight; retrieval unchanged | yes | [`ingest-big-pdf`](eval-ab/ingest-big-pdf/) |
| 11-14 | `ING-6`: ingest seats, per-page OCR routing, one shared OCR pool and breaker | 20 × 50-page scans: 372.9 s → **210.3 s** (1.77×; the 3× target is **not met** — the run is OCR-bound at 89% of the OCR floor). A 1-page scan behind a 300-page one is ready in **2.6 s** (was 114.4 s). VmHWM 711 MiB (target 1.5 GB) | yes | [`ing6`](eval-ab/ing6/) |
| 11-16 | OCR engine per tier, olmOCR-bench's 1,403 pages, official scorer | Lite CPU sidecar 0.69 pages/s (image-only 52.5); Xiaomi-OCR-0 on vLLM 87.0 text-layer / 77.9 tables / 73.7 scans / 77.4 image-only; + layout table pass 89.0 / 87.9; Mistral OCR 77.4 / 85.5 / 70.8 / 75.3 | yes (Lite sidecar, Xiaomi for GPU tiers, Mistral opt-in) | [`ocr-tiers`](eval-ab/ocr-tiers/) |
| 11-13 | `ING-5`: read every picture with Qwen3-VL-2B-Instruct | Recall@10 0.555 → **0.985**; photos 0.230 → 0.990, charts 0.900 → 0.980. **Qwen3-VL-8B buys nothing** (ΔR@10 +0.000 [−0.015, +0.015]), so the 2B stays | yes | [`images`](eval-ab/images/) |
| 10-16 | Local CPU profile on an 11 GB box: Qwen3-Embedding-0.6B + bge | 0.6B: 2.5 GB RAM, 0.33–0.35 s per query; bge: 5.6 GB RAM, ~4 s per rerank. The plan's p95 targets are unreachable on CPU | led to the GPU/RX 590 profile | [`local-models`](local-models.md) |

## Tables

| When | What | Result | Run |
|---|---|---|---|
| 11-14 | `tables-v1`: can a table be found, and then answered exactly? | The set and its targets are committed (retrieval in top 5 ≥ 0.85, execution ≥ 0.98, end to end ≥ 0.80 and +0.30 over flattened text); the scored run is **not** in the repo yet (`TBL-1` is In Progress) | [`tables-v1`](eval-ab/tables-v1/) |

## Plumbing

| When | What | Result | Run |
|---|---|---|---|
| 10-16 | The harness end to end on stub embeddings (8 dimensions, no model, no network) | Ingest, search, isolation, deletes and the eval harness work over HTTP; the ranking is token overlap, **not** a quality claim | [`eval-baseline`](eval-baseline.md) |
| every PR | `rust/scripts/smoke.sh` and `rust/scripts/ci-eval-gate.sh` in CI | Ingest/search/delete and cross-tenant leak probes on a live stack; a retrieval-quality floor on the 12-query CI set | [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) |

## Models that were measured and not chosen

| Model | Where it was tried | Why it is not the default |
|---|---|---|
| `Qwen/Qwen3-VL-8B-Instruct` | pictures (`ING-5`) | No gain over the 2B: ΔR@10 +0.000 [−0.015, +0.015], MRR −0.027 [−0.062, +0.011] |
| `Qwen/Qwen3-Embedding-0.6B` on Lite | 2 CPU cores | Ties e5-small on quality, 41× slower to ingest and 19× slower per search |
| `Qwen/Qwen3-Reranker-0.6B` | MIRACL-id + hotpot, RX 590 | Better on Indonesian (+0.019), significantly worse on English (−0.038), 2.4× slower |
| `nvidia/nemotron-3.5-rerank-8b:free` | page reranking (ViDoRe HR) | **Not worse** — 0.705 vs voyage's 0.704, and free — but the cloud tier prefers one provider and one key; it is the documented alternative |
| `intfloat/multilingual-e5-large` as rantai-kb's embedder | KF V-plane comparison | 0.796 under rantai-kb vs KF's 0.813; the swap was rejected on quality and latency |
| `jina-reranker-v3.5`, `jina-embeddings-v4`, `colqwen2.5` | licence review | CC-BY-NC-4.0 or a non-commercial base: not shippable (`docs/model-licences.md`) |

`nvidia/nemotron-3.5-super-vl` **is** a default — it reads pictures on the cloud tier
(`KB_IMAGE_DESCRIBE_MODEL`), about $0.22 per 1,000 images. The local tiers keep
Qwen3-VL-2B-Instruct, which is what the numbers above are for.

## Run, but not written up

- [`docs/accuracy-rerank.md`](accuracy-rerank.md) — a reranker bake-off over the engine's own
  fused candidates (voyage, nemotron-8b, qwen3-8b, Qwen3-0.6B, bge) with the candidate table
  and prices filled in; its results and spend sections are still placeholders.
- [`docs/l4-bench-results/`](l4-bench-results/) — the pre-optimisation profiling runs behind
  [`eval-baseline.md`](eval-baseline.md) §7 (Bun-era, kept for the record).

## Not measured

- **Desk, Studio and Station on their own hardware.** `docker compose config` and unit tests
  only; the OCR numbers come from a GB10 at a Desk-sized share, not from an 8 GB card.
- **The local page embedder** (Qwen3-VL-Embedding-2B/8B under vLLM): request shape tested
  against a mock, never a real vLLM, so no local page-retrieval number exists.
- **Apple silicon end to end** — detected and suggested by `setup`, but compose reserves
  NVIDIA GPUs only.
- **Live vision rerank and page captions** on the cloud tier: offline quality measured
  (0.711 → 0.727 nDCG@10 at depth 20), latency not.
- **Cost per page on the local tiers** — the point is that there is none, but nobody has
  measured wall-clock throughput per GPU tier on a real card.

## How to re-run

Each run folder has its script or `run.sh` and pins its settings; the two live harnesses are
`rust/scripts/stack-up.sh` (stack) and `kb-server eval` (scoring). The judge is never the model:
every comparison is a paired bootstrap against the same candidate pool, and a run that would
cost money prints its estimate first.
