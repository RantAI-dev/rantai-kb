# Handoff: planner and reviewer

For the agent taking over the planner and reviewer role. Written 2026-11-04 and checked
against the repository and GitHub at that time. Re-check before relying on it, because other
agents work in this repo.

Read `AGENTS.md` first. This file tells you where things stand; that one tells you the rules.

## 1. Your role

- **Plan, review, keep `docs/core/` current.** Lead with the conclusion when you write to
  the product owner, and recommend one option instead of listing several.
- **Before writing product documents, confirm with the owner** in the form "from the repo
  I think X — correct, or adjust?".
- **Every number you report is one you ran** (`AGENTS.md` rule 1). "Not run" is an
  acceptable answer.

## 2. The product, as confirmed by the owner (2026-11-04)

- **What it is:** rantai-kb is a **shared retrieval engine** for RantAI's products (RantAI
  Agents and others): upload a document, then search it and get cited chunks and pages back.
- **Open source:** it is public under Apache-2.0 so the community can try it.
- **Not yet a product of its own:** it is not sold on its own today. That would need users,
  roles, folders and a UI (`ACC-*` in the backlog).
- **Full picture:** [PRODUCT.md](PRODUCT.md).

## 3. Where the documents are

| What | Where |
| --- | --- |
| Rules for agents | `AGENTS.md` |
| The product, coverage, blockers, roadmap | `docs/core/PRODUCT.md` |
| The one list of work | `docs/core/BACKLOG.md` |
| Specs and feature pages | `docs/core/specs/`, `docs/core/features/` |
| Tiers and what each one runs | `docs/kb-tiers.md`, `docs/tier-readiness-plan.md` |
| Deploying | `rust/deploy/README.md` |
| Cutover from Bun, and every deliberate difference | `docs/rust-cutover.md` |
| Measured retrieval results | `docs/eval-ab/<run>/README.md` |

## 4. What has been merged

- **`main`:** the v0.1.0 Bun service, plus PRs #1 and #2 (2026-09-23). It holds no Rust; the
  Rust service replaces it from v0.2.0 (`DEC-3`).
- **`feat/kb-sota`:** the whole Rust port. It is PR #3, still a **draft**, 281 commits ahead
  of `main`. The last 16 commits are local and not pushed (`REL-3`).

## 5. What is in flight

The P0 items in `BACKLOG.md`, which block the release:

- **`SEC-1`:** by-ID routes ignore a key's knowledge-base limit.
- **`REL-1`:** the eval gate is red.
- **`DEP-1`:** the lite tier is unproven on a fresh build.

**`DEC-1`** decides what the release is.

## 6. How to review

- **Run the checks in `AGENTS.md`'s table:** fmt, clippy, tests, and both boundary checks.
- **Tests for scope rules:** a change to any search arm or document route needs a test that
  another tenant's key, and a key limited to other knowledge bases, sees nothing.
- **Measure retrieval changes:** run `rust/scripts/eval-ab.sh` on a labelled set
  (`miracl-id-200`, `vidore3-hr-en`, `vidore3-cs-en`) and report the paired delta with a CI
  (`rust/crates/kb-eval/scripts/score_deltas.py`).

## 7. Check the developer agent's claims

Reproduce any test count, benchmark or "it works" before writing it into these documents.

Past examples:

- An eval run that died mid-query, and whose fallback answers were nearly reported as results.
- A tier marked ready that had never been started.

## 8. Known loose ends

- **Not ours:** `rust/crates/kb-eval/scripts/kf_vplane_ablate.py`, `rerank_bakeoff.py` and
  `ranx_check.py` are another agent's uncommitted work. Do not commit or delete them.
- **Bun was removed in v0.2.0:** the last Bun release is tag `v0.1.0`. Rust is the only
  runtime in the repo.
- **Spend:**
  - Paid APIs need a cost estimate and the owner's go-ahead before any corpus-sized job.
  - Embeddings and results are cached on disk under `~/.cache/kb-cache`.
- **The owner's workstation is also their daily machine:**
  - Builds run with `nice` and 2 jobs.
  - Never stop or delete other projects' containers or volumes.
- **KF is a comparison baseline:** run it unmodified, never fix it.
