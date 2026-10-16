# Plan: the best multimodal knowledge base, on local or cloud models

Written 2026-10-16 with `.claude/skills/ambitious-planning`. Inputs: the ParaGPT kb-bridge
reference (`~/kb-ref/kb`, spec + code), a map of our current pipeline, and the public state of
the art as of October 2026 (sources at the end).

## Goal

- **Literal ask:** make rantai-kb the best multimodal RAG/KB in the world, usable with local or
  cloud models.
- **Goal behind it:** apps built on rantai-kb answer from documents (text, tables, charts,
  flowcharts, scans) correctly, show the exact page or figure they used, never leak across
  tenants, and run the same way on an air-gapped GPU box or on cloud APIs.
- **Goal beyond that:** "best" is a claim other people can check. We prove it on public
  leaderboards and on our own labelled Indonesian set, and every change is gated by that
  measurement in CI, so quality can only go up.
- **Chosen level:** the measurement system plus the pipeline, not a feature list. Without the
  first, "best" is an opinion. All work lands in Rust; Bun is frozen.

## Constraints (decided 2026-10-16)

- **Rust only.** Bun is frozen. Its BM25 leak goes away at cutover, not by patching Bun.
- **Near-zero budget.** We have no paid API credit and this machine has no GPU (6 cores,
  11 GB RAM, about 35 GB of free disk). So:
  - **Local is the default profile.** Cloud stays an optional profile that is tested with mocks
    until someone pays for a run.
  - **Use the CPU for small models:**
    - text embedding: Qwen3-Embedding-0.6B or bge-m3, via ONNX or llama.cpp;
    - reranking: Qwen3-Reranker-0.6B or bge-reranker-v2-m3;
    - BM25, which is free.
  - **Use free GPU notebooks** (Kaggle about 30 h/week, Colab) for heavy batch jobs:
    - page-image embeddings;
    - the layout parser (PaddleOCR-VL or MinerU) over the eval corpus;
    - figure captions with an open vision model.

    Those jobs export their results as files; `kb-eval` imports them, so the GPU is never
    needed at query time during eval.
  - **Disk cache** of every embedding, caption and OCR output, keyed by (model, content hash).
    Each corpus is processed once, and every ablation reuses it.
  - **Eval on subsets.** Use 1–2 ViDoRe v3 datasets and about 300 queries, which is enough for
    bootstrap confidence intervals. The full benchmark is for the leaderboard run only, if
    ever.
  - **Free API tiers**, where they exist (e.g. OpenRouter `:free` chat models for drafting
    questions), are a bonus, not a dependency.

## Target

Baseline: **none exists today.** No retrieval-quality number has ever been measured for
rantai-kb. Step 0 produces it.

| Measure | Target | Stretch | External reference |
|---|---|---|---|
| ViDoRe v3 pipeline leaderboard, cloud profile | top 3 at submission | #1 | best single retriever 64.26 nDCG@10; ParaGPT reached 0.653–0.668 on the HR subset |
| Same, local-only profile (open weights) | within 3 nDCG points of our cloud profile | equal | Qwen3-VL-Embedding/Reranker, ColEmbed v2 are open |
| Own set: right page ranked first (≥ 200 labelled Indonesian questions) | ≥ 87% | ≥ 92% | ParaGPT: 55% → 87% |
| Chart/flowchart questions answered correctly | ≥ 90% | 100% | ParaGPT: 3/17 → 17/17 with captions |
| Right picture shown, measured separately from right page | ≥ 85% | ≥ 95% | ParaGPT: ~79% |
| MMLongBench-Doc answer accuracy, RAG with an open-weights generator | ≥ 55% | ≥ 62% | frontier long-context models: 60.5–62.0% |
| Unanswerable questions correctly refused (MMLongBench-Doc subset) | ≥ 80% | ≥ 90% | |
| Cross-tenant hits across the red-team matrix | 0 | 0 | |
| `/v1/search` p95 end-to-end, text + page arms, local profile, warm | < 300 ms | < 150 ms | |
| `/v1/search` p95 without rerank | < 80 ms | < 40 ms | |
| Vector store p95, filtered top-50 at 1M chunks | < 20 ms | < 5 ms | Qdrant filtered HNSW is in this range; measure on our data |
| Recall@50 lost to approximate search (HNSW + quantisation vs exact) | < 1 point | < 0.3 | |
| Ingest throughput, local profile on one 24 GB GPU | ≥ 1 page/s with layout + captions | ≥ 3 pages/s | |

## Depth

- **What an expert would do that wasn't asked for.**
  - Build the evaluation before any feature.
  - Ablate each component, so we know which ones earn their cost.
  - Report confidence intervals.
  - Treat locating the evidence inside a page (bounding boxes) as part of retrieval quality.
- **Raw material.**
  - Real customer PDFs: scans, letterheads, watermarks, tables split across pages, flowcharts.
  - Real user questions, with the pages a human marked as correct.
  - Raw store rows (Qdrant points, graph tables) and the raw extractor output for each page,
    rather than the API's summary.
  - The public ViDoRe v3 and MMLongBench-Doc corpora.
- **Instruments.**
  - Retrieval: nDCG@10, recall@k, MRR and right-page@1.
  - Answers: answer accuracy and the share of unanswerable questions correctly refused.
  - Uncertainty: bootstrap confidence intervals.
  - Speed and cost: p50/p95 latency and cost per 1k pages and per 1k queries.
  - Profiling: criterion benches and flamegraphs for ingest.
  - Isolation: a leak matrix.
- **Outside validators.**
  - The public leaderboards: the ViDoRe v3 pipeline framework (`BasePipeline.index/search`)
    and MMLongBench-Doc.
  - Human-labelled questions in our own set. An LLM may draft them; a person approves every
    one.
  - The existing parity and smoke harnesses. Nothing is graded by the model that produced it.

## Assumptions to break

1. **"Page-image retrieval beats our text pipeline."**
   - Counter-evidence: the most accurate pipeline when the ViDoRe v3 pipeline leaderboard
     launched was text-only (Jina v4 + ZeRank2). Image plus OCR text beat image alone by about
     6.5%.
   - Test: run text-only, image-only and image+text on ViDoRe v3 and on our set before
     building the page index for real.
2. **"Single-vector page embeddings are good enough."**
   - Late-interaction retrieval (several vectors per page, scored with MaxSim, as in ColPali) is
     the most accurate single-model approach, but it costs far more storage.
   - Test: on Qdrant at 100k pages, compare single-vector Qwen3-VL-Embedding with native
     multivector MaxSim, measuring nDCG, latency and memory.
3. **"Local models can match cloud."**
   - Test: run the same pipeline in both profiles on the same eval and publish the gap.
   - Hardware is a known blind spot: this dev box has no GPU (6 cores, 11 GB RAM), so local-
     profile numbers need a rented GPU or the target hardware.
4. **"Retrieval is our bottleneck, not extraction."**
   - Test: replace our extracted text with gold text on a subset. If the score jumps, fix
     extraction first.
5. **"Bigger chunks with a reranker beat our 800/200 chunks."**
   - Test: sweep chunk sizes 800, 1,800 and 3,000 with the reranker on and off.

6. **"Once the store is fast, search is fast."**
   - Decided (user, 2026-10-16): SurrealDB is a bottleneck, as seen in production on Bun, and
     we are moving off it. Our code agrees: the Rust vector query
     (`kb-store-surreal/src/queries.rs:6`) computes cosine for every chunk in scope and never
     uses the HNSW index from `schema.rs:28`, with 4096-d vectors and scope passed as an
     `IN [...]` list. Latency grows linearly with the corpus.
   - What could still dominate after the move: the cross-encoder reranker on CPU (24 × 1,500
     characters) and the query-embedding round trip.
   - Test: per-stage timings before and after the move, so the gain is proven and the next
     bottleneck is known.
7. **"Graph retrieval will improve answers."**
   - Decided: entities and relations move to Postgres tables, traversed in Rust memory
     (FalkorDB was rejected for its SSPL licence; see `docs/architecture-target.md` §11).
   - Not yet proven: that a graph *retrieval arm* helps. The Rust port has none
     (`hybrid:true` returns an empty result), and Bun's only matches entity names that appear
     literally in the query.
   - Test: with the arm built (HippoRAG2-style personalised PageRank), the eval must show a gain on multi-hop and
     cross-page questions before the arm is turned on by default. Until then it stays behind a
     flag and ingest keeps writing entities.

## Plan

Each step is measured against step 0, committed separately, and kept only if it moves a
target or a guardrail.

### Step 0: measurement (blocks everything else)
- **New crate `kb-eval` with a `kb-server eval` subcommand.**
  - Loads a dataset: queries, gold pages/chunks and optional answers.
  - Ingests the corpus into a throwaway tenant.
  - Runs `/v1/search` and writes metrics as JSON and markdown with bootstrap confidence
    intervals.
- **Datasets.**
  - A ViDoRe v3 subset, with the full set run nightly.
  - MMLongBench-Doc.
  - `eval/id-internal/`: at least 200 questions over real customer PDFs. An LLM drafts them; a
    human labels the gold pages and figures.
- **Red-team leak matrix.**
  - Every retrieval arm × every scope type (KB, category, documentIds, tenant).
  - The target is zero hits.
- **CI gate.** A 50-question smoke subset fails the build if right-page@1 drops by more than
  2 points.
- **Output.** Record the first baseline for both runtimes in `docs/eval-baseline.md`.

### Step 1: fix what we already know is broken (cheap; one commit each, each measured)
- Merge across the hybrid arms by normalising record ids. Today RRF never merges a chunk found
  by both arms.
- Turn the reranker on by default with a real cross-encoder.
  - Rerank 24 candidates at 1,500 characters each.
  - Hand the original scores out in the new order, so the 0.30 floor keeps its meaning.
  - On failure, fall back to vector order and record a degrade.
- BM25: fetch more candidates before applying the scope filter, so another tenant can't crowd
  out our results.
- Rust embedder: batch the calls (128 per call, 4 in parallel). Today it makes one HTTP call
  per chunk.
- Put page numbers in the context text.
- Make "search failed" distinct from "empty result" in the response.
- Fall back to the document list when the best score is low.
- Re-ingest:
  - Skip unchanged files by content hash.
  - Delete stale entities.
  - Keep page numbers on the OCR fallback path.

### Step S: restructure to the target architecture (`docs/architecture-target.md`)

Runs after step 0 and before the Qdrant and graph adapters in step L, because those
adapters need domain-shaped ports. Each part is shipped and measured on its own:

1. **Ports.**
   - Split `ChunkStore`/`DocumentStore` into the target ports.
   - Move figure, neighbour and parent logic into `kb-engine`.
   - Delete `bm25_search_global`.
   - The domain owns ids; adapters own id encoding.
   - Add contract test suites, and run the SurrealDB adapter against them in containers.
2. **Observability and API contract.**
   - Tracing: `TraceLayer`, request ids, per-stage spans.
   - `/metrics`.
   - OpenAPI spec generated from code (utoipa).
   - problem+json errors with stable codes.

   Step L's latency measurement needs these, so they come early.
3. **Document model.**
   - Postgres: `DocumentVersion` and `kb_index` tables (additive, so Bun is unaffected).
   - The parsed artifact in S3, with pages, labelled items and bounding boxes.
   - Furniture-aware, parent/child chunking with section paths.
4. **Staged, content-addressed ingest.** Each stage is cached by input hash and stage version.
5. **Standard APIs.** OpenAI-compatible `vector_stores/{kb}/search` and an MCP server.

**Check.** The contract suites pass for every adapter. The eval does not regress after the
port refactor (step 1 must be behaviour-neutral). The document-model work is judged by the
eval, including bounding-box grounding.

### Step L: latency, and the move off SurrealDB (decided)

Target end state: **Postgres (+ graph tables) + Qdrant + S3, with DuckDB embedded for
tables. SurrealDB is removed.** "Blazing fast"
is a measured number (see Target), not only a choice of database.

1. **Instrument.**
   - Add per-stage timings (query embed, vector, keyword, fuse, rerank, neighbours, figures,
     serialise) to `stats` and to tracing spans.
   - Add `kb-server bench search`: an hdrhistogram load test at 10k, 100k and 1M chunks, with
     p50/p95/p99 at concurrency 1, 8 and 32.
   - Record today's SurrealDB numbers once, only so the gain can be shown. No tuning work goes
     into SurrealDB.
2. **`kb-store-qdrant`** implements `ChunkStore`, which covers chunks, vectors, keyword search,
   neighbours and figures.
   - Qdrant is written in Rust, has an official Rust client and is Apache-2.0.
   - Dense vectors use HNSW with scalar or binary quantisation and rescoring. Use
     Matryoshka-truncated dimensions (4096 → 1024) where the eval shows no loss.
   - Keyword search uses sparse vectors with the IDF modifier, replacing SurrealDB BM25. We
     tokenise in Rust (the same tokenizer and Indonesian/English stemming for documents and
     queries).
   - Server-side hybrid search: the `query` API with prefetch (dense + sparse + page arm) and
     RRF, so all arms run in one round trip.
   - Native multivector with MaxSim for late-interaction page retrieval (assumption 2).
   - Payload indexes: `tenant_id` with `is_tenant: true`, plus keyword indexes on
     `document_id`, `knowledge_base_id`, `category` and `chunk_type`. Scope is enforced inside
     the HNSW traversal instead of an `IN` list.
   - Deterministic point ids: UUIDv5 of (tenant, document, chunk). Re-ingest overwrites, and
     stale tail chunks are deleted.
   - One collection per embedding model, so vector spaces are never mixed.
3. **Graph in Postgres + Rust CSR** (`GraphStore`). This covers entity, mention and relation
   tables, a per-tenant in-memory adjacency cache, and personalised PageRank with 1–2 hop
   expansion. Target: p95 < 10 ms at 1M edges. ArcadeDB (Apache-2.0) is the challenger if the
   target is missed. The research is in `docs/architecture-target.md` §11.
4. **Store-independent latency work.**
   - Run all arms concurrently.
   - Cache query embeddings.
   - Run the reranker as an int8 ONNX model, with candidate count and character budget tuned by
     eval and a per-request timeout that falls back to fused order.
   - Stream serialisation.
5. **Migration.**
   - Add `kb-server migrate surreal-to-qdrant` and `surreal-to-pg-graph`. They copy existing
     vectors, chunks and entities **without re-embedding**, so they cost nothing. Both can be
     resumed and are checked by count and checksum.
   - Bun reads chunks from SurrealDB, so the order is:
     1. Cut production over to Rust (still on SurrealDB).
     2. Run the migration.
     3. Flip `KB_CHUNK_STORE=qdrant` and `KB_GRAPH_STORE=pg`.
     4. Keep SurrealDB read-only for one release as a rollback path.
     5. Remove `kb-store-surreal`.
6. **Check.**
   - The latency targets.
   - Recall lost to approximate search under 1 point against exact search on the same eval.
   - The leak matrix at zero. Qdrant filters and the graph cache enforce scope, so they need
     their own red-team pass.
   - `smoke.sh` and the parity harness pointed at the new stores. Parity is now Rust on
     SurrealDB vs Rust on Qdrant: results should match up to approximate-search noise.

### Step T: tables as first-class data (`docs/architecture-target.md` §10)

- **Goal behind it:** numeric and matrix questions ("total ceiling for glasses at level 3",
  "who approves X above Y") are answered by computing from the real table, never by an LLM
  reading flattened text.
- **Eval first.**
  - A T²-RAGBench subset: text + table retrieval, Recall@5/MRR@3. The published best is
    hybrid + rerank, R@5 0.816.
  - 50+ Indonesian table questions from our documents with exact numeric answers.
  - Table-structure accuracy (TEDS) on our PDFs for parser choice.
- **Build, measuring each part:**
  1. `kb-tables` model and extraction rules: merged cells, multi-level headers, page
     stitching, furniture tables dropped, titles, manifest.
  2. The four retrieval units: summary, rows, cell values, and rendered-in-chunk with header
     carry-over.
  3. DuckDB-backed `POST /v1/tables/{id}/query` (restricted DSL) and the MCP `table_query`
     tool. Search hits carry exact table labels.
  4. Ablation: Markdown vs HTML vs key–value serialisation, on our set.
- **Targets:**

  | Measure | Target | Stretch |
  |---|---|---|
  | T²-RAGBench subset R@5 | ≥ 0.82 (beats the published best) | ≥ 0.88 |
  | Exact-match numeric answers on our table set, via `table_query` | ≥ 90% | ≥ 95% |
  | Invented table names in tool calls | 0 | 0 |

### Step 2: extraction as good as the best parsers, local or cloud
- **Layout port profiles.**
  - Local: PaddleOCR-VL 1.6, MinerU2.5-Pro or GLM-OCR (~1B parameters each, top of
    OmniDocBench at 95–96%).
  - Cloud: Mistral OCR or a Gemini vision model.
  - Every block keeps its page and bounding box.
- **Cleanup ported from ParaGPT.**
  - Remove repeated headers, footers and tables after their first appearance (repeated on 3 or
    more pages).
  - Strip diagonal watermarks.
  - Recover text the parser dropped.
  - Rebuild tables that a converter split into one paragraph per cell.
  - Stitch tables split across pages and repeat their header row.
  - Index glossary rows on their own.
- **Figures.**
  - Junk-image filter (`_raster_sampah` thresholds).
  - A page-sized image with readable text is a page, not a figure; apply this rule to both
    embedded pictures and vector drawings.
  - Vision-model captions that list every flowchart step and branch and turn chart values into
    a table, cached by prompt version.
- **Check.** Run the gold-text ablation from assumption 4 and the chart/flowchart subset.

### Step 3: page-image index (the multimodal arm)
- **Per page:** render, then embed the image plus its OCR text (the +6.5% finding).
  - Local: Qwen3-VL-Embedding 2B/8B. It supports Matryoshka, so dimensions can be cut for
    speed and memory where the eval shows no loss.
  - Cloud: voyage-multimodal-3.5.
- **Index stamping.** Every index is stamped with its model. Scores are never compared across
  embedding spaces (ParaGPT rule 5).
- **Search.** Run page search in parallel with text search, rerank with a multimodal
  reranker, and fuse by rank.
  - Local reranker: Qwen3-VL-Reranker.
  - Cloud reranker: Voyage, Cohere or a VLM.
- **API.** `/v1/search` returns pages as signed, expiring image URLs, plus figure crops and
  evidence bounding boxes.
- **Isolation.** Pages go through the same `DocumentScope` decision as the text arms.
- **Check.** Run the three-way ablation from assumption 1, plus a latency budget.

### Step 4: grounded answers
- **Optional `/v1/answer` endpoint and an SDK helper** implementing the citation-marker
  protocol:
  - The answering model sees the top pages.
  - Pages with 2 or more figures get set-of-mark boxes and numbers.
  - It cites `[Page p · Figure n · title]`.
  - Code resolves each citation to a (document, page) pair, shows only cited crops, and
    attaches nothing when unsure.
- **Images go to the model for one turn only.** History and caches store links.
- **Check.** Measure "right picture shown" separately from "right page retrieved", and run the
  MMLongBench-Doc answer and unanswerable checks.

### Step 5: hard questions
- Rewrite follow-up questions using the conversation, so page search doesn't run on the raw
  last message.
- Answer cross-page questions with multi-hop retrieval: decompose the question, then
  retrieve, then check.
- Detect unanswerable questions from calibrated scores instead of guessing.
- Add an agentic retrieval loop with self-correction, behind a flag. It must keep running
  within a latency budget.
- **Check.** The MMLongBench-Doc cross-page subset (33.7% of its questions) and its
  unanswerable subset (20.6%).

### Step 6: ship it as a product
- **Two deployment profiles.**
  - `docker-compose.local.yml`: vLLM/TEI serving the embedder, reranker, OCR/layout and
    captioner, with no outbound network.
  - `docker-compose.cloud.yml`: OpenRouter, Voyage or Cohere, with data retention denied.
- **Publish.** A public eval report, then a ViDoRe v3 leaderboard submission. Submitting is
  outward-facing, so it needs the user's approval.

## Validation
- `kb-server eval --dataset vidore-v3|mmlongbench|id-internal --profile local|cloud` produces
  the numbers above with confidence intervals. The official ViDoRe `vidore-benchmark pipeline
  evaluate-all` cross-checks our ViDoRe score.
- `search-parity.sh`, `mixed-fleet.sh`, `smoke.sh` and the red-team leak matrix stay green.
- Human-approved labels for the internal set. A model may draft labels but never grades the
  results.

## Out of scope / risks
- **Out of scope:**
  - Video and audio.
  - Training our own embedding model. Fine-tuning on our domain is a later option once the eval
    exists.
  - Bun feature work.
- **GPU access.** There is no GPU here, so local-profile numbers depend on getting one.
- **Licences.** Some top ViDoRe models aren't licensed for commercial use. Check each licence
  before adopting a model.
- **More services to run.** Only Qdrant is new. The graph lives in Postgres and DuckDB is
  embedded, so there is one service fewer once SurrealDB goes. Use quantisation and on-disk
  payloads on the 11 GB box.
- **Mixed fleet ends.** After the switch, Bun can no longer read chunks. Cut over to Rust before
  flipping the store.
- **Keyword parity.** Qdrant sparse/IDF scoring is not identical to SurrealDB BM25. Judge it by
  the eval, not by matching old scores.

## Sources
- [ViDoRe v3 pipeline framework and leaderboard](https://huggingface.co/blog/antoineedy/vidore-v3-pipeline-framework-and-leaderboard)
- [VultronRetriever ViDoRe v3 results](https://blogs.vultr.com/vidore-v3)
- [Nemotron ColEmbed V2](https://arxiv.org/pdf/2602.03992)
- [Qwen3-VL-Embedding and Qwen3-VL-Reranker](https://arxiv.org/pdf/2601.04720)
- [MMLongBench-Doc leaderboard](https://benchlm.ai/benchmarks/mmlongbenchdoc)
- [Open-source OCR on OmniDocBench](https://roboflow.com/blog/best-open-source-ocr-models), [MinerU2.5-Pro](https://arxiv.org/pdf/2604.04771)
- ParaGPT kb-bridge spec: `~/kb-ref/kb/MULTIMODAL-RAG-SPEC.md`
