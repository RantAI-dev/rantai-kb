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
| `RET-3` | Feature | Metadata filters at search time: `language`, `documentIds` / `externalRefs`, and arbitrary metadata `key = value`, applied **inside every arm** (dense, BM25, page image, figure, neighbour, entity) before ranking, not after. A calling app that decides access itself can then search only what its user may see, without losing recall to a post-filter. **Targets:** the CI scope gate extended to probe every arm with a filter, 0 leaks; filtered recall@10 equal to searching a corpus that holds only the allowed documents (±0.005); search p95 +10% at most. Related to `SEC-1` and `ACC-3` | Retrieval | P1 | Idea | | Feature comparison 2026-11-04 (a post-filter spends top-k on out-of-scope results) |
| `ING-3` | Feature | A document-structure read: `GET /v1/documents/:id/structure` returning pages (number, OCR text when scanned, has-text-layer), figures (key, page, bbox, caption, OCR text, crop asset key) and the chunk → page map, with `externalRef`. A calling app can then show, cite or keep its own record of a document's pages and figures without re-parsing the file. Additive API. **Targets:** every figure and page that search can return is listed; the endpoint enforces the tenant and the key's knowledge-base limit (`SEC-1`) | Ingest | P1 | Idea | | Feature comparison 2026-11-04; today only `/v1/documents/:id/assets` (one crop) exists |
| `DEC-3` | Decision | Retire Bun from `main` at the release, or keep it frozen beside Rust for one more release. **Decided 2026-11-04: remove Bun (product owner)** | Platform | P1 | Decided | | PRODUCT §6; `docs/rust-cutover.md` §3 phase 4 |

## Next (P2)

| ID | Kind | Item | Module | Priority | Status | PR | Source |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ACC-1` | Feature | Folders: nest knowledge bases (or folders inside one) and filter search by a folder and everything under it. Today knowledge bases are flat, and a document may sit in several | Access & Security | P2 | Idea | | Owner question 2026-11-04 |
| `ACC-2` | Feature | Users, groups and roles per knowledge base (viewer, editor, admin), so a calling app passes "who is asking" instead of minting one key per permission set. Today permissions belong to API keys, not people | Access & Security | P2 | Idea | | Owner question 2026-11-04 |
| `ACC-3` | Feature | Per-document permissions filtered at search time, for documents inside a knowledge base that only some users may see. **Blocked by `ACC-2`** | Access & Security | P2 | Idea | | Owner question 2026-11-04 |
| `EVAL-1` | Task | Run KF main on ViDoRe v3 HR and CS (Docling + RapidOCR, pages mapped by text overlap), to complete the comparison. Needs an idle machine: about 2,470 pages of OCR on CPU | Retrieval | P2 | Todo | | `docs/eval-ab/rk-cloud-vs-kf/README.md` caveats |
| `RET-1` | Task | Measure a local tier's retrieval on the same sets as the cloud one (studio's Qwen3-VL page arm on a GPU), so each tier has its own number. Only cloud and local bge (0.661 HR) are measured. **Blocked by `DEP-2`** | Retrieval | P2 | Todo | | `docs/kb-tiers.md` |
| `ING-1` | Feature | Legacy binary Office files (`.doc`, `.xls`, `.ppt`) are accepted by the upload list but cannot be extracted (`Unsupported`, as in v0.1.0). Extract them, or stop accepting them with a clear error | Ingest | P2 | Idea | | `docs/rust-cutover.md` §5.2; `kb-extract/src/mime.rs` |
| `RET-2` | Feature | Relevance floor: an optional `minScore` per request and a calibrated default, so an off-topic question returns nothing instead of the nearest of nothing. The floor must use a score that means the same across queries (dense cosine or the reranker score), not the fused RRF score, which is rank-only (moco-kf found the same, `design/09` §6). An exact-match / identifier query is exempt. **Targets:** on an off-topic set (≥ 50 questions with no answer in the corpus, to be built) ≥ 90% return empty; MIRACL-id-200 and ViDoRe v3 HR / CS nDCG@10 drop ≤ 0.005 | Retrieval | P2 | Idea | | Feature comparison 2026-11-04 (moco-kf has it) |
| `RET-4` | Feature | Recency and authority boosts per request: boost by a date field (half-life) or a numeric metadata field (weight), general options rather than one caller's tenant settings. Off by default and rank-identical when off. **Targets:** build a dated labelled set (≥ 50 "latest version of…" questions over documents with several dated versions); MRR +0.05 there with the boost on, and no drop > 0.005 on MIRACL-id-200 | Retrieval | P2 | Idea | | Feature comparison 2026-11-04 (moco-kf has source authority + freshness weighting) |
| `RET-5` | Feature | Adaptive arm choice: label the question (exact ID or quoted phrase, factual, structural, relational, aggregative, topical) and skip the arms that do not help it, e.g. no page-image arm for an ID lookup. Every skipped arm is named in the response, never silent. **Targets:** p50 search latency −20% on a MIRACL + ViDoRe mix at ≤ 0.005 nDCG@10 loss | Retrieval | P2 | Idea | | Feature comparison 2026-11-04 (moco-kf routes planes by query class) |
| `EVAL-2` | Task | Measure whether a tree index (navigating a document's section hierarchy) helps structural questions ("what does chapter 2 say about…"). Build a structural set (≥ 50 questions with gold pages), prototype the index in `kb-eval` (moco-kf's tree plane is a reference implementation to compare against), run search with it on and off, and report nDCG@10 and recall@10 with 95% CIs. Write a cost estimate before any paid run | Retrieval | P2 | Todo | | Feature comparison 2026-11-04; no one has measured moco-kf's tree plane |
| `EVAL-3` | Task | Measure whether graph community summaries ("global mode") help broad questions ("what are the main themes across these documents?"). Build an aggregative set (≥ 30 questions with judged answers), prototype the summaries in `kb-eval` (moco-kf's graph global mode is a reference implementation), run search with them on and off, and report answer-support recall with 95% CIs. Estimate the ingest LLM cost per 1,000 pages before running | Retrieval | P2 | Todo | | Feature comparison 2026-11-04 |
| `DEC-4` | Decision | Build a tree index into rantai-kb? Decide from `EVAL-2`: build only if the tree plane's gain on structural questions has a 95% CI above zero and is worth its ingest cost. **Blocked by `EVAL-2`** | Retrieval | P2 | Open | | Feature comparison 2026-11-04 |
| `DEC-5` | Decision | Build graph community summaries and a global mode into rantai-kb? Decide from `EVAL-3` and its cost estimate (an LLM pass at ingest). **Blocked by `EVAL-3`** | Retrieval | P2 | Open | | Feature comparison 2026-11-04 |
| `DEP-4` | Task | Server start does not check the tier's GPU memory budget; only `setup` does | Deployment | P3 | Todo | | `docs/tier-readiness-plan.md` |

## Later (P3)

| ID | Kind | Item | Module | Priority | Status | PR | Source |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ING-2` | Feature | Connectors that sync documents from Google Drive, SharePoint, Confluence and similar sources. Today a calling app uploads every file itself | Ingest | P3 | Idea | | PRODUCT §2 |
| `DEP-5` | Feature | Parse and caption residency (`KB_PARSE_RESIDENCY`, `KB_CAPTION_RESIDENCY`) and a priority queue, as described in `docs/kb-tiers.md`. Documented, not built | Deployment | P3 | Idea | | `docs/tier-readiness-plan.md` |

## Dates

Started is the date of the item's plan; Shipped is the merge date of the PR that delivered it.
Due dates are set by the product owner and stay empty until they are.

| ID | Started | Shipped | Due |
| --- | --- | --- | --- |
| `RET-2` | 2026-11-04 | | |
| `RET-3` | 2026-11-04 | | |
| `RET-4` | 2026-11-04 | | |
| `RET-5` | 2026-11-04 | | |
| `ING-3` | 2026-11-04 | | |
| `EVAL-2` | 2026-11-04 | | |
| `EVAL-3` | 2026-11-04 | | |
| `DEC-4` | 2026-11-04 | | |
| `DEC-5` | 2026-11-04 | | |

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
