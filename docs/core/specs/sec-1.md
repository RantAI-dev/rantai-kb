# `SEC-1` A key limited to some knowledge bases cannot open other documents by ID

| | |
| --- | --- |
| Backlog | `SEC-1` in [BACKLOG.md](../BACKLOG.md) |
| Module | Access & Security |
| Size | S (planner's estimate: S = days, M = one to two weeks, L = several weeks) |
| Priority and status | In [BACKLOG.md](../BACKLOG.md), the one place they are kept |

## Why

An API key can be limited to some knowledge bases (`create-api-key --kb kbA,kbB`). That limit
holds for search, the document list, upload and metadata edits (`restrict_kbs` in
`rust/crates/kb-server/src/routes.rs`).

The routes that take a document ID check only the tenant (`find_document`, `routes.rs:1047`):

- `GET /v1/documents/:id`
- `PATCH /v1/documents/:id`
- `DELETE /v1/documents/:id`
- `GET /v1/documents/:id/raw`
- `/assets`
- `/intelligence`

So a key limited to KB A can read, download, edit or delete a document that sits only in KB B,
if it knows or guesses the ID. The Bun v0.1.0 service had the same behaviour
(`src/service/api.ts` at tag `v0.1.0`), so this is not a regression from the port. The repo
is now public, which makes it a real hole: the knowledge-base limit is the only permission
model the product offers.

Found by reading the code on 2026-11-04. Not yet reproduced against a running server.

## What users get

The knowledge-base limit on a key means the same thing on every route.

## Target specs

"Today" is `feat/kb-sota` at `0840d55`, read from the code, not tested.

| Capability | Today | Target |
| --- | --- | --- |
| Get, raw, assets and intelligence of a document outside the key's knowledge bases | Returned (tenant check only) | 404, the same answer as a document in another tenant, so the ID's existence does not leak |
| Patch or delete such a document | Allowed | 404 |
| Job status and retry (`/v1/jobs/:id`), and ingest events (`/v1/events`), for such a document | *Not verified* | The same rule, or written down as out of scope with the reason |
| A document in no knowledge base, for a limited key | *Not verified* | Not visible *(proposed)*, matching how search treats it today; checked against `restrict_kbs` |
| An unlimited key (no `--kb`) | Sees every document in the tenant | Unchanged |

## Benchmark

Every hosted competitor with collection-level keys or permissions answers 404 or 403 for an
object outside the caller's scope. Competitor detail is in `PRODUCT.md` §2.

## Acceptance checklist

Run on a running server by the product owner. A step not performed is never a pass.

| ID | Do this | Expect | Result |
| --- | --- | --- | --- |
| `SEC-1-AC1` | Make KB A and KB B in one tenant, upload doc X into B only, and make a key limited to A | — | Pass (2026-11-04, live lite stack) |
| `SEC-1-AC2` | With the A key: `GET /v1/documents/<X>`, then `/raw`, `/assets`, `/intelligence` | Each is 404 | Pass: all four 404 (2026-11-04, live) |
| `SEC-1-AC3` | With the A key: `PATCH` and `DELETE /v1/documents/<X>` | 404, and X is unchanged | Pass: 404 and 404, title unchanged (2026-11-04, live) |
| `SEC-1-AC4` | With an unlimited key in the same tenant: the same requests | They work as before | Pass for GET (200, live 2026-11-04); PATCH and DELETE with an unlimited key are covered by the route tests, not run live |
| `SEC-1-AC5` | With the A key: search for X's exact text | X is not returned (unchanged behaviour) | Pass: 0 hits (2026-11-04, live) |
| `SEC-1-AC6` (operator) | `cargo test -p kb-server` | A new test covering AC2 and AC3 passes, and so do the old tests | Pass (2026-11-04: `cargo test -p kb-server -p kb-ports -p kb-store-pg` — kb-server 138 unit + 121 integration tests, all passing) |

## Not included

- Per-user or per-document permissions (`ACC-2`, `ACC-3`).

## Before anyone builds it

This is a spec, not a plan. Before building it needs the decision on the "document in no
knowledge base" row (product owner), and a test written first.
