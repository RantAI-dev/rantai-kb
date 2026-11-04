# <Feature name>

One page. The agents build from it; the product owner accepts with it.
Copy this file to `<feature-slug>.md` and fill it in. Keep it to what a user
can observe; how it is built belongs in the engineering plan.

| | |
| --- | --- |
| Module | <Ingest, Retrieval, Access & Security, Deployment, Platform, Delivery or Community> |
| Backlog | <ID> |
| Kind | Feature |
| Spec | `docs/core/specs/<id>.md` (target numbers; every *(proposed)* one is signed under Decisions) |
| Status | Idea / Planned / Building / In Acceptance / Released / Killed ("merged" is **In Acceptance**, never Released) |
| Priority | P0 / P1 / P2 / P3 |
| Owner | Not named here (the repo is public; see `DEC-2`). |
| Acceptor | Who runs the acceptance checklist. Must not be the Owner. Not named here. |
| Plan | the plan's path or PR description |
| Started | <date of the plan>; also in `BACKLOG.md` Dates |
| Shipped | <merge date of the delivering PR>; also in `BACKLOG.md` Dates |
| Evidence | The PR(s), and a link to the accepted build or its checklist run |

## Problem

What is wrong today, in two or three sentences, with the evidence.

## What the user can do when this is done

Short, separate, checkable statements. One per line.

1. ...
2. ...

## Not included

What this deliberately leaves out.

## For calling apps

Which API routes or fields change, and is the change additive? A breaking change to
`/v1/*` needs a version note in `CHANGELOG.md` and a line in `docs/rust-cutover.md`.

## Decisions

Anything the product owner must choose. Until signed, the default is used.
Every number marked *(proposed)* in the task's spec is listed here.

| # | Decision | Default | Signed |
| --- | --- | --- | --- |

## Limits to tell a user

Plain statements of what it will not do. Copied into `PRODUCT.md` section 3
when accepted.

## Acceptance checklist

One row per check. Each has a stable ID (`<BACKLOG-ID>-AC<n>`, e.g. `BI-1-AC1`)
and keeps that ID for life. Run on a real
deployment. **Result** is **Pass**, **Fail**, or **Not run** (with the reason). A step not performed is never Pass. Steps needing
a terminal are marked (operator).

This list is the self-contained acceptance criteria. Do not write "every target
row works"; write the checks out in full here, so the row means something on its
own on its own.

Always include: the normal path, a failure being visible (`degrades`), a key from another
tenant or knowledge base, switching it off, and, for retrieval, the metric on a labelled set.

| ID | Do this | Expect | Result |
| --- | --- | --- | --- |
| `<ID>-AC1` | ... | ... | Not run |

**Acceptor** (not the Owner): __________ **Date:** ______ **Build:** ______

Exceptions, each with an owner and a date:

## After acceptance

- [ ] `PRODUCT.md` section 2 and 3 updated
- [ ] `BACKLOG.md` status set to **Released**, with the PR in the PR column; follow-ups added
- [ ] `BACKLOG.md` Dates has the Shipped date
- [ ] `CHANGELOG.md` entry an outside user can read
