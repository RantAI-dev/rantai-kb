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
| `SEC-1` | Task | A key limited to some knowledge bases can still read, download, edit and delete a document outside them by ID. The by-ID routes check only the tenant. Bun does the same. [Spec](specs/sec-1.md) | Access & Security | P0 | Done | | Code read 2026-11-04, `routes.rs:1047` |
| `REL-1` | Task | PR #3's "Rust retrieval eval & scope isolation gate" is red. On the 12-query CI set, document hit@1 fell 0.500 → 0.417 (one query; max allowed drop 0.020), while page nDCG@10 rose 0.500 → 0.815. Either fix the query or refresh the gate's baseline with the reason written down | Delivery | P0 | Todo | | CI run 40703570765 (2026-10-28) |
| `REL-2` | Task | PR #3's Bun "End-to-end smoke test" is red: `docker pull` of its MinIO image is refused (unauthorized). Repoint the image, or retire the Bun job if `DEC-3` retires Bun | Delivery | P1 | Todo | | CI run 40703570765 |
| `REL-3` | Task | Push the 16 local commits on `feat/kb-sota` (tier presets, compose for every tier, the cloud eval, the lite OCR build fix) so PR #3 and CI see them | Delivery | P1 | Todo | | `git rev-list origin/feat/kb-sota..HEAD` = 16, 2026-11-04 |
| `DEP-1` | Task | Prove the lite and lite-en tiers on a fresh build: `kb-dev.sh up lite --build`, upload one scanned PDF, and search its text. The PP-OCRv6 sidecar has never been built or run, and the `up` fix (`0840d55`) is untested. The last lite-en run (2026-10-25) predates both | Deployment | P0 | Todo | | `rust/deploy/README.md` status; commit `0840d55` |
| `DEP-2` | Task | Run desk, studio and station once on an NVIDIA machine: health, one upload, one search. They are checked only by `docker compose config`, and every GPU memory split is an estimate | Deployment | P1 | Todo | | `rust/deploy/compose.yaml` header; `docs/kb-tiers.md` |
| `DEP-3` | Task | Run `kb-dev.sh up cloud` once through compose. Cloud search quality was measured only through the eval harness (`stack-up.sh`), not through the compose profile | Deployment | P1 | Todo | | `docs/eval-ab/rk-cloud-vs-kf/README.md` |
| `REL-4` | Task | Run clippy (`-D warnings`) on the tier preset and config changes (`7f6106f`); it has not been run since them | Delivery | P1 | Todo | | Session notes 2026-10-31 |
| `COM-1` | Task | Make the public repo usable by outsiders. `README.md` still describes the Bun service as the runtime and its quick start. There is no `CONTRIBUTING.md`, no `SECURITY.md` (where to report a hole like `SEC-1`) and no `CHANGELOG.md`. Rewrite the README around the Rust service and `kb-dev.sh`, and say plainly what is verified per tier | Community | P1 | Todo | | Repo root, 2026-11-04 |
| `DEC-1` | Decision | What "release" means: merge PR #3 into `main` (281 commits ahead) and tag `v0.2.0`, or tag the branch first. Recommended: merge after `SEC-1`, `REL-1`, `DEP-1` and `COM-1`, then tag | Delivery | P1 | Open | | PRODUCT §6 |
| `DEC-2` | Decision | Where Owner, Acceptor and Assignee are kept, since the repo is public: a Lark base like Lakehouse, or GitHub Projects so community contributors can see the board | Community | P1 | Open | | PRODUCT §6 |
| `DEC-3` | Decision | Retire Bun from `main` at the release, or keep it frozen beside Rust for one more release | Platform | P1 | Open | | PRODUCT §6; `docs/rust-cutover.md` §3 phase 4 |

## Next (P2)

| ID | Kind | Item | Module | Priority | Status | PR | Source |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ACC-1` | Feature | Folders: nest knowledge bases (or folders inside one) and filter search by a folder and everything under it. Today knowledge bases are flat, and a document may sit in several | Access & Security | P2 | Idea | | Owner question 2026-11-04 |
| `ACC-2` | Feature | Users, groups and roles per knowledge base (viewer, editor, admin), so a calling app passes "who is asking" instead of minting one key per permission set. Today permissions belong to API keys, not people | Access & Security | P2 | Idea | | Owner question 2026-11-04 |
| `ACC-3` | Feature | Per-document permissions filtered at search time, for documents inside a knowledge base that only some users may see. **Blocked by `ACC-2`** | Access & Security | P2 | Idea | | Owner question 2026-11-04 |
| `EVAL-1` | Task | Run KF main on ViDoRe v3 HR and CS (Docling + RapidOCR, pages mapped by text overlap), to complete the comparison. Needs an idle machine: about 2,470 pages of OCR on CPU | Retrieval | P2 | Todo | | `docs/eval-ab/rk-cloud-vs-kf/README.md` caveats |
| `RET-1` | Task | Measure a local tier's retrieval on the same sets as the cloud one (studio's Qwen3-VL page arm on a GPU), so each tier has its own number. Only cloud and local bge (0.661 HR) are measured. **Blocked by `DEP-2`** | Retrieval | P2 | Todo | | `docs/kb-tiers.md` |
| `ING-1` | Feature | Legacy binary Office files (`.doc`, `.xls`, `.ppt`) are accepted by the upload list but cannot be extracted (`Unsupported`, as in Bun). Extract them, or stop accepting them with a clear error | Ingest | P2 | Idea | | `docs/rust-cutover.md` §5.2; `kb-extract/src/mime.rs` |
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
