# Backlog

The master list of rantai-kb work. Every item has a stable ID, and IDs are never reused. The
coding agents read this file, so it is the source of truth for the work.

Started 2026-11-04 from:

- reading the repository at `0840d55` (`feat/kb-sota`);
- PR #3's CI;
- the earlier plans (`docs/plan-best-multimodal-kb.md`, `docs/tier-readiness-plan.md`).

## The columns

- **Kind**: **Feature** (one thing a user or a calling app can do or rely on; it has an
  acceptance checklist and an Acceptor), **Task** (tracked work that is not itself a
  capability: a fix, a test, a check), or **Decision** (a choice the product owner makes).
- **Module**:
  - **Ingest**: upload, extraction, OCR, chunking.
  - **Retrieval**: search quality and speed.
  - **Access & Security**: tenants, keys, knowledge bases, permissions.
  - **Deployment**: tiers, compose, setup.
  - **Platform**: the Rust port, the stores, the cutover.
  - **Delivery**: CI, release.
  - **Community**: open-source repo health, docs for outside users.
- **Priority**: P0 (blocks the release), P1, P2, P3.
- **Status**, by kind:
  - Feature: **Idea** (no spec yet), **Planned** (has a spec, not started), **Building**,
    **In Acceptance** (merged, waiting for the owner's checklist), **Released**, **Killed**.
  - Task: **Todo**, **In Progress**, **Blocked**, **Done**, **Cancelled**.
  - Decision: **Open**, **Decided**.
- **PR**: the pull request(s) that delivered it.
- **Source**: where the item comes from, so it can be checked.

**Blocked by.** An item that cannot start until another merges says so in its text
(**Blocked by `REL-1`**). A blocker is a dependency, not a status.

## Now (P0–P1)

| ID | Kind | Item | Module | Priority | Status | PR | Source |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `SEC-1` | Task | A key limited to some knowledge bases can still read, download, edit and delete a document outside them by ID. The by-ID routes check only the tenant. The v0.1.0 Bun service did the same. [Spec](specs/sec-1.md) | Access & Security | P0 | Done | | Code read 2026-11-04, `routes.rs:1047` |
| `REL-1` | Task | PR #3's "Rust retrieval eval & scope isolation gate" is red. On the 12-query CI set, document hit@1 fell 0.500 → 0.417 (one query; max allowed drop 0.020), while page nDCG@10 rose 0.500 → 0.815. Either fix the query or refresh the gate's baseline with the reason written down | Delivery | P0 | Done | `433bd3d` | CI run 40703570765 (2026-10-28) |
| `REL-2` | Task | PR #3's Bun "End-to-end smoke test" is red: `docker pull` of its MinIO image is refused (unauthorized). Repoint the image, or retire the Bun job if `DEC-3` retires Bun. **Cancelled: Bun CI jobs removed with `DEC-3`** | Delivery | P1 | Cancelled | | CI run 40703570765 |
| `REL-3` | Task | Push the 16 local commits on `feat/kb-sota` (tier presets, compose for every tier, the cloud eval, the lite OCR build fix) so PR #3 and CI see them. **Done 2026-11-04** with the release push | Delivery | P1 | Done | | `git rev-list origin/feat/kb-sota..HEAD` = 16, 2026-11-04 |
| `DEP-1` | Task | Prove the lite and lite-en tiers on a fresh build: `kb-dev.sh up lite --build`, upload one scanned PDF, and search its text. The PP-OCRv6 sidecar has never been built or run, and the `up` fix (`0840d55`) is untested. The last lite-en run (2026-10-25) predates both. **Done 2026-11-04:** `kb-dev.sh up lite --build` healthy on a fresh image; a scanned PDF with no text layer was OCR'd by the PP-OCRv6 sidecar and ingested in 10 s, and `/v1/search` returned its passage with no degrades. lite-en shares the sidecar and was not re-run | Deployment | P0 | Done | | `rust/deploy/README.md` status; commit `0840d55` |
| `DEP-2` | Task | Run desk, studio and station once on an NVIDIA machine: health, one upload, one search. They are checked only by `docker compose config`, and every GPU memory split is an estimate | Deployment | P1 | Todo | | `rust/deploy/compose.yaml` header; `docs/kb-tiers.md` |
| `DEP-3` | Task | Run `kb-dev.sh up cloud` once through compose. Cloud search quality was measured only through the eval harness (`stack-up.sh`), not through the compose profile | Deployment | P1 | Todo | | `docs/eval-ab/rk-cloud-vs-kf/README.md` |
| `REL-4` | Task | Run clippy (`-D warnings`) on the tier preset and config changes (`7f6106f`); it has not been run since them | Delivery | P1 | Done | | Session notes 2026-10-31 |
| `COM-1` | Task | Make the public repo usable by outsiders. `README.md` still describes the Bun service as the runtime and its quick start. There is no `CONTRIBUTING.md`, no `SECURITY.md` (where to report a hole like `SEC-1`) and no `CHANGELOG.md`. Rewrite the README around the Rust service and `kb-dev.sh`, and say plainly what is verified per tier | Community | P1 | Done | | Repo root, 2026-11-04 |
| `DEC-1` | Decision | What "release" means: merge PR #3 into `main` (more than 280 commits ahead) and tag `v0.2.0`, or tag the branch first. **Decided 2026-11-04: release `v0.2.0` so RantAI products can use it; merge PR #3 then tag** | Delivery | P1 | Decided | | PRODUCT §6 |
| `DEC-2` | Decision | Where Owner, Acceptor and Assignee are kept, since the repo is public: a Lark base like Lakehouse, or GitHub Projects so community contributors can see the board. **Decided 2026-11-04: GitHub Projects** | Community | P1 | Decided | | PRODUCT §6 |
| `RET-3` | Feature | Metadata filters at search time: `language`, `documentIds` / `externalRefs`, and arbitrary metadata `key = value`, applied **inside every arm** (dense, BM25, page image, figure, neighbour, entity) before ranking, not after. A calling app that decides access itself can then search only what its user may see, without losing recall to a post-filter. **Targets:** the CI scope gate extended to probe every arm with a filter, 0 leaks; filtered recall@10 equal to searching a corpus that holds only the allowed documents (±0.005); search p95 +10% at most. Related to `SEC-1` and `ACC-3` **Live 2026-11-04 (cloud stack):** metadata scalar and array filters returned only matching documents; a bad key is a 400; `externalRefs: []` returns nothing; a KB-limited key plus a filter returns nothing outside its KBs. Recall parity and p95 not measured (`docs/eval-ab/retrieval-gaps/README.md`) | Retrieval | P1 | In Acceptance | | Feature comparison 2026-11-04 (a post-filter spends top-k on out-of-scope results) |
| `ING-3` | Feature | A document-structure read: `GET /v1/documents/:id/structure` returning pages (number, OCR text when scanned, has-text-layer), figures (key, page, bbox, caption, OCR text, crop asset key) and the chunk → page map, with `externalRef`. A calling app can then show, cite or keep its own record of a document's pages and figures without re-parsing the file. Additive API. **Targets:** every figure and page that search can return is listed; the endpoint enforces the tenant and the key's knowledge-base limit (`SEC-1`) **Live 2026-11-04:** a 111-page ViDoRe PDF lists 111 pages with `hasTextLayer` and 619 chunks with pages; other tenant, and a key limited to another KB, get 404. Page JPEGs (`imageAssetKey`) exist only when the vision rerank is on; figures were not extracted in that config, so the figure list is unit-tested only (`docs/eval-ab/retrieval-gaps/README.md`) | Ingest | P1 | In Acceptance | | Feature comparison 2026-11-04; today only `/v1/documents/:id/assets` (one crop) exists |
| `DEC-3` | Decision | Retire Bun from `main` at the release, or keep it frozen beside Rust for one more release. **Decided 2026-11-04: remove Bun (product owner)** | Platform | P1 | Decided | | PRODUCT §6; `docs/rust-cutover.md` §3 phase 4 |

## Next (P2)

| ID | Kind | Item | Module | Priority | Status | PR | Source |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ACC-1` | Feature | Folders: nest knowledge bases (or folders inside one) and filter search by a folder and everything under it. Today knowledge bases are flat, and a document may sit in several | Access & Security | P2 | Idea | | Owner question 2026-11-04 |
| `ACC-2` | Feature | Users, groups and roles per knowledge base (viewer, editor, admin), so a calling app passes "who is asking" instead of minting one key per permission set. Today permissions belong to API keys, not people | Access & Security | P2 | Idea | | Owner question 2026-11-04 |
| `ACC-3` | Feature | Per-document permissions filtered at search time, for documents inside a knowledge base that only some users may see. **Blocked by `ACC-2`** | Access & Security | P2 | Idea | | Owner question 2026-11-04 |
| `EVAL-1` | Task | Run KF main on ViDoRe v3 HR and CS (Docling + RapidOCR, pages mapped by text overlap), to complete the comparison. Needs an idle machine: about 2,470 pages of OCR on CPU | Retrieval | P2 | Todo | | `docs/eval-ab/rk-cloud-vs-kf/README.md` caveats |
| `RET-1` | Task | Measure a local tier's retrieval on the same sets as the cloud one (studio's Qwen3-VL page arm on a GPU), so each tier has its own number. Only cloud and local bge (0.661 HR) are measured. **Blocked by `DEP-2`** | Retrieval | P2 | Todo | | `docs/kb-tiers.md` |
| `ING-1` | Feature | Legacy binary Office files (`.doc`, `.xls`, `.ppt`) are accepted by the upload list but cannot be extracted (`Unsupported`, as in v0.1.0). Extract them, or stop accepting them with a clear error **Built 2026-11-13 (`feat/legacy-office-tiff`):** pure-Rust `.doc` (Word 6.0–2003), `.ppt` (PowerPoint 95–2003) and `.xls` (calamine) readers; legacy names holding RTF/HTML/XML/text/OOXML are routed by signature. **Measured:** LibreOffice-written copies of 24 real `.docx` and 25 `.xlsx`, and 11 python-pptx decks, against the modern parser: token recall/precision median 1.000 (`.doc` min recall 0.999, `.xls` min 0.998, `.ppt` 1.000 on all). Apache POI's 386-file corpus: no panics; `.doc` vs LibreOffice text on 87 files median 1.000 recall and precision; `.ppt` vs LibreOffice-converted `.pptx` on 76 files median 1.000, mean recall 0.93 (misses are footers, dates and slide numbers, not read). Not handled: Word 2.0, PowerPoint 4.0, encrypted files (clear errors); `.ppt` footer text | Ingest | P2 | In Acceptance | | `docs/rust-cutover.md` §5.2; `kb-extract/src/mime.rs` |
| `ING-4` | Feature | TIFF scans and unknown extensions. Accept `.tif`/`.tiff`; OCR every page of a multi-page TIFF (fax servers and scanners write one file per document) as PNG with its own page number. Refuse an unknown or missing extension in the extractor instead of indexing its bytes as text **Built 2026-11-13 (`feat/legacy-office-tiff`):** 4-page Pillow-written TIFFs decode pixel-for-pixel for CCITT G3 (1-D) and G4, LZW and Deflate (JPEG within its own loss); a contract test pins the upload list to the extractor's routing. Not live-tested against a vision OCR endpoint; palette and 2-D G3 pages refused with a reason | Ingest | P2 | In Acceptance | | Owner request 2026-11-13 |
| `ING-5` | Feature | Images are searchable by what they show: standalone photos/diagrams/charts, images inside DOCX/PPTX/XLSX/ODT/EPUB, PDF figure crops and (opt-in) PDF embedded images go through one visual path: normalise, filter, store as an asset, describe with an `ImageDescriber` (kind, description, visible text; cached), index as a figure chunk. An image upload with nothing searchable fails visibly. CPU VLM sidecar for Lite/Desk. [Spec](specs/ing-5.md) **Measured 2026-11-13 (`images-v1`, 200 human queries, live HTTP; `docs/eval-ab/images/README.md`):** baseline (OCR only) R@10 0.555, photos 0.230 (75/100 never found); with Qwen3-VL-2B (the Lite/Desk model file) R@10 0.985, photos 0.990, charts 0.980, scene-text 0.980, MRR 0.877; embedded in DOCX/PPTX/PDF R@10 0.980; Qwen3-VL-8B no better (ΔR@10 +0.000). Leak matrix `--docx`: figure row UNTESTED → PASS, 0 leaks / 504 probes. CPU speed 29–100 s per picture (not measured idle). **Targets:** on `images-v1`, photos without text Recall@10 ≥ 0.85 (baseline ≈ 0), charts ≥ 0.80, embedded within 0.05 of standalone, local CPU VLM ≥ 0.70 | Ingest | P1 | In Acceptance | | Owner question 2026-11-13: "do we support images with text only?" |
| `ING-6` | Feature | Ingest stays light under load: jobs run concurrently through per-stage seats (extract/embed/store, oldest first; OCR and picture description are process-wide pools that hold no seat), and PDF page work is offloaded to an OCR service with `KB_OCR_MODE=off|hybrid|full` (per-page scan test, page batches for PDF-taking providers, per-page cache, outage breaker, page cap, failed pages marked and listed in `metadata.ocr`). Today the worker runs one job at a time (`ingest.rs:294`) and decides "scanned" per document (`scanned.rs:12`). Modelled on paragon `kb-bridge` (seats `kb_progress.py:83`, OCR `kb_ocr.py`). [Spec](specs/ing-6.md) **Targets:** 20×50-page PDFs ≥ 3× faster than the serial worker, a 1-page file behind a 300-page scan ready < 30 s, kb-server VmHWM ≤ 1.5 GB, mixed PDF OCRs exactly its scanned pages, re-upload 0 OCR calls | Ingest | P1 | In Progress | | Owner 2026-11-14: "seat based so the queue is very efficient and fast; heavy lifting on mistral/ocr so we dont waste compute or ram on kb" |
| `RET-2` | Feature | Relevance floor: an optional `minScore` per request and a calibrated default, so an off-topic question returns nothing instead of the nearest of nothing. The floor must use a score that means the same across queries (dense cosine or the reranker score), not the fused RRF score, which is rank-only (moco-kf found the same, `design/09` §6). An exact-match / identifier query is exempt. **Targets:** on an off-topic set (≥ 50 questions with no answer in the corpus, to be built) ≥ 90% return empty; MIRACL-id-200 and ViDoRe v3 HR / CS nDCG@10 drop ≤ 0.005 **Measured 2026-11-04 (cloud, voyage rerank-2.5):** at `minScore` 0.42, off-topic empty 90.9% on MIRACL-id (nDCG@10 0.8380 → 0.8381) and 98.1% on ViDoRe HR (page nDCG@10 0.6398 → 0.6399). CS not run (spend). Off by default; 0.42 is the recommended cloud value, not a shipped default (`docs/eval-ab/retrieval-gaps/README.md`) | Retrieval | P2 | In Acceptance | | Feature comparison 2026-11-04 (moco-kf has it) |
| `RET-4` | Feature | Recency and authority boosts per request: boost by a date field (half-life) or a numeric metadata field (weight), general options rather than one caller's tenant settings. Off by default and rank-identical when off. **Targets:** build a dated labelled set (≥ 50 "latest version of…" questions over documents with several dated versions); MRR +0.05 there with the boost on, and no drop > 0.005 on MIRACL-id-200 **Measured 2026-11-04 (cloud):** dated set MRR@10 0.346 → 0.991 (+0.646 [+0.621, +0.664]; synthetic, versions near-identical, so the gain is largely by construction); MIRACL-id-200 with a `createdAt` boost nDCG@10 0.8450 → 0.8451 (`docs/eval-ab/retrieval-gaps/README.md`) | Retrieval | P2 | In Acceptance | | Feature comparison 2026-11-04 (moco-kf has source authority + freshness weighting) |
| `RET-5` | Feature | Adaptive arm choice: label the question (exact ID or quoted phrase, factual, structural, relational, aggregative, topical) and skip the arms that do not help it, e.g. no page-image arm for an ID lookup. Every skipped arm is named in the response, never silent. **Targets:** p50 search latency −20% on a MIRACL + ViDoRe mix at ≤ 0.005 nDCG@10 loss **Measured 2026-11-04 (ViDoRe HR, cloud, warm caches):** p50 −2.6% (48.4 → 47.1 ms), page nDCG@10 0.711 → 0.710. **Latency target missed**: only exact queries (24/318) skip arms (median 47 → 25 ms); MIRACL has no skippable query. Off by default (`docs/eval-ab/retrieval-gaps/README.md`) | Retrieval | P2 | In Acceptance | | Feature comparison 2026-11-04 (moco-kf routes planes by query class) |
| `RET-6` | Feature | Tree arm (DEC-4): a structural question ("what does section 4.2 say…", "chapter 3 of the joint employment report") navigates a document's section outline (PDF bookmarks, or numbered headings) instead of matching text. On only when `KB_TREE_ARM=1` **and** the classifier labels the question `structural`; chunks carry the `tree` provenance and `stats.treeArm` reports `{nodes, pages}`. Outlines are stored in `Document.metadata.outline` at ingest; existing documents get one on re-ingest. Prototype measured +0.086 nDCG@10 on structural questions, 0.000 on ordinary ones (`docs/eval-ab/retrieval-gaps/README.md`) **Live 2026-11-04 (stub stack, one ViDoRe PDF):** ingest stored a 30-node bookmark outline; a structural query reported `treeArm {nodes: 5, pages: 9}` with `tree` chunks on top; a topical query did not run it. Retrieval quality of the Rust arm not re-measured (the prototype's +0.086 is EVAL-2's) | Retrieval | P2 | In Acceptance | | `DEC-4` |
| `EVAL-2` | Task | Measure whether a tree index (navigating a document's section hierarchy) helps structural questions ("what does chapter 2 say about…"). Build a structural set (≥ 50 questions with gold pages), prototype the index in `kb-eval` (moco-kf's tree plane is a reference implementation to compare against), run search with it on and off, and report nDCG@10 and recall@10 with 95% CIs. Write a cost estimate before any paid run **Result 2026-11-04 (ViDoRe HR):** tree arm gated by the production classifier: structural set (41) nDCG@10 +0.086 [+0.038, +0.147]; regular HR (318) +0.000. Ungated it hurts regular questions (−0.193). CS not run live; set is template-made from the same outlines (`docs/eval-ab/retrieval-gaps/README.md`) | Retrieval | P2 | Done | | Feature comparison 2026-11-04; no one has measured moco-kf's tree plane |
| `EVAL-3` | Task | Measure whether graph community summaries ("global mode") help broad questions ("what are the main themes across these documents?"). Build an aggregative set (≥ 30 questions with judged answers), prototype the summaries in `kb-eval` (moco-kf's graph global mode is a reference implementation), run search with them on and off, and report answer-support recall with 95% CIs. Estimate the ingest LLM cost per 1,000 pages before running **Result 2026-11-04:** with deepseek-v4-flash summaries (159), even an oracle gate lowers aggregative recall@10 (HR −0.067 [−0.114, −0.021], CS −0.103 [−0.165, −0.049]). Summary cost about $0.07 (`docs/eval-ab/retrieval-gaps/README.md`) | Retrieval | P2 | Done | | Feature comparison 2026-11-04 |
| `DEC-4` | Decision | Build a tree index into rantai-kb? Decide from `EVAL-2`: build only if the tree plane's gain on structural questions has a 95% CI above zero and is worth its ingest cost. **Blocked by `EVAL-2`** **Planner's recommendation (2026-11-04):** build it, only behind the structural gate: +0.086 nDCG@10 on structural questions, no change on ordinary ones, no model calls at ingest (outline parse, 3.5 s per 2,470 pages). Confirm on CS and on a human-written structural set first **Decided 2026-11-04 (product owner): build it behind an env switch (`KB_TREE_ARM`), off by default.** | Retrieval | P2 | Decided | | Feature comparison 2026-11-04 |
| `DEC-5` | Decision | Build graph community summaries and a global mode into rantai-kb? Decide from `EVAL-3` and its cost estimate (an LLM pass at ingest). **Blocked by `EVAL-3`** **Planner's recommendation (2026-11-04):** do not build this design: summaries fused into search lower recall even on the questions they target. Revisit only with a different selection method (embedded summaries) and a fresh measurement **Decided 2026-11-04 (product owner): build it behind an env switch (`KB_GLOBAL_ARM`), off by default, although the prototype measured a loss.** | Retrieval | P2 | Decided | | Feature comparison 2026-11-04 |
| `DEP-4` | Task | Server start does not check the tier's GPU memory budget; only `setup` does | Deployment | P3 | Todo | | `docs/tier-readiness-plan.md` |

## Later (P3)

| ID | Kind | Item | Module | Priority | Status | PR | Source |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ING-2` | Feature | Connectors that sync documents from Google Drive, SharePoint, Confluence and similar sources. Today a calling app uploads every file itself | Ingest | P3 | Idea | | PRODUCT §2 |
| `DEP-5` | Feature | Parse and caption residency (`KB_PARSE_RESIDENCY`, `KB_CAPTION_RESIDENCY`) and a priority queue, as described in `docs/kb-tiers.md`. Documented, not built | Deployment | P3 | Idea | | `docs/tier-readiness-plan.md` |
| `RET-7` | Feature | Global community arm (`DEC-5`): graph community summaries for broad/aggregative questions, behind `KB_GLOBAL_ARM` (off by default), with the operator build `kb-server communities build` and a scoped read that never returns the summary text. **`EVAL-3` measured a loss** (aggregative recall@10 −0.067 HR, −0.103 CS with an oracle gate), so this ships switchable, not on. `docs/rust-cutover.md` §21 **Live 2026-11-04 (stub stack, heuristic graph):** `kb-server migrate` created `KbCommunity`; `communities build --dry-run` estimated $0.0013 and wrote nothing; the real build wrote 4 summaries (deepseek-v4-flash, reasoning off); an aggregative query reported `globalArm {communities: 3, chunks: 102}`; the summary text never appeared in a response; a key limited to another KB got 0 chunks and 0 communities. Off by default: EVAL-3 measured a loss | Retrieval | P3 | In Acceptance | | DEC-5 |

## Dates

Started is the date of the item's plan; Shipped is the merge date of the PR that delivered it.
Due dates are set by the product owner and stay empty until they are.

| ID | Started | Shipped | Due |
| --- | --- | --- | --- |
| `RET-2` | 2026-11-04 | | |
| `RET-3` | 2026-11-04 | | |
| `RET-4` | 2026-11-04 | | |
| `RET-5` | 2026-11-04 | | |
| `RET-6` | 2026-11-04 | | |
| `ING-3` | 2026-11-04 | | |
| `EVAL-2` | 2026-11-04 | | |
| `EVAL-3` | 2026-11-04 | | |
| `DEC-4` | 2026-11-04 | | |
| `DEC-5` | 2026-11-04 | | |
| `RET-7` | 2026-11-04 | | |

## Done and rejected

Measured and left off, so nobody re-runs them without a new reason:

| ID | Kind | Item | Status | Source |
| --- | --- | --- | --- | --- |
| `RET-90` | Feature | Live vision-LLM page rerank: +0.009 page nDCG@10 on HR [−0.002, +0.020], not significant, about +5.5 s per search. Built, left off (`KB_PAGE_VISION_RERANK_MODEL` unset) | Killed | `docs/eval-ab/vidore3-hr-en-image/README.md`, 2026-10-24 |
| `RET-91` | Feature | Page captions at ingest: −0.005 page nDCG@10 on HR [−0.011, +0.001]. Built, left off (`KB_PAGE_CAPTION_MODEL` unset) | Killed | Same file |

## Specs

| ID | Spec | Size |
| --- | --- | --- |
| [`SEC-1`](specs/sec-1.md) | A key limited to some knowledge bases cannot open other documents by ID | S |
