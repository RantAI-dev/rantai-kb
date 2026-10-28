# AGENTS.md

Instructions for any coding agent working in this repo (Claude Code, OpenCode, Antigravity, Codex…).

## What this is

rantai-kb is a standalone document ingest and retrieval service: upload a file, it is extracted,
chunked, embedded and indexed; `/v1/search` returns scoped context with sources and figures.
Two runtimes share the Postgres tables and the API shape:

- **Bun/TS** (`src/`): the frozen production service.
- **Rust** (`rust/crates/`): the port that replaces it. Hexagonal: `kb-core` (pure logic) ←
  `kb-ports` (interfaces) ← adapters (`kb-inference`, `kb-store-pg`, `kb-store-qdrant`,
  `kb-store-blob`, `kb-extract`) and `kb-engine` (pipelines) ← `kb-server` (composition root).

**All new work happens in Rust.** Bun is frozen: no features, no refactors. Touch it only if
the user asks for a specific fix.

Stores: Postgres (documents, jobs, KBs, keys, and the entity graph when `KB_GRAPH_STORE=pg`;
Prisma migrations), Qdrant (chunks, vectors, BM25, page images — the only chunk store), and
S3-compatible blobs. Docs: `docs/porting-plan.md`, `docs/rust-cutover.md`, `rust/README.md`,
`rust/NOTES.md`.

## Commands

| Check | Command |
|---|---|
| TS typecheck / tests | `bun run typecheck` · `bun run test` |
| TS boundary | `bun run check:kb-boundary` |
| Rust format / lint | `cargo fmt --all --manifest-path rust/Cargo.toml --check` · `cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings` |
| Rust tests | `cargo test --manifest-path rust/Cargo.toml --workspace` (needs libpdfium: `./rust/scripts/fetch-pdfium.sh ./rust/.vendor/pdfium`) |
| Rust boundary | `cargo run --manifest-path rust/Cargo.toml -p kb-server -- check-boundary` |
| Live checks | `rust/scripts/smoke.sh` · `rust/scripts/ci-eval-gate.sh` |

## Working rules

1. **Evidence, not claims.** Paste real command output. Never report a test count, benchmark
   number or smoke result you did not just run. "Not run" is an acceptable answer; an invented
   one is not.
2. **Commit early and often** with conventional messages (`fix(retrieval): …`). Never leave a
   session with uncommitted work and no note of it. Do not push or open PRs unless asked.
3. **Respect the boundaries.** `kb-core` does no I/O; `kb-engine` knows ports, never adapter
   names. Both boundary checks must pass.
4. **Rust ↔ Bun share Postgres and the API, not the chunk store.** The PostgreSQL tables
   (documents, jobs, KBs, keys), their record ids, the job queue and the HTTP shapes must
   interoperate, so a key minted by one runtime works on the other and either worker can claim
   either job. The chunk store is **not** shared: Rust is Qdrant-only and Bun keeps its own
   SurrealDB store. Production data written by Bun is moved to Qdrant with
   `kb-server reindex` (rebuild from the S3 originals), not by reading SurrealDB. Where Rust
   deliberately differs from Bun (e.g. the BM25 scope fix), say so in `docs/rust-cutover.md`.
5. **Tenant scope is sacred.** Every retrieval arm (vector, BM25, entity, figure, neighbour)
   must be filtered to the caller's visible documents. Add a test for any new arm.
6. **A failed search is not an empty result.** Degrade visibly (`degrades`), never silently.
7. **Spend as close to zero as possible.** Local open-weights models come first; paid APIs are
   optional profiles. Cache every embedding, caption and OCR result on disk, keyed by model and
   content hash, so no input is paid for twice. Never start a paid batch job (embedding a whole
   corpus, captioning) without the user's go-ahead and a cost estimate.

## Be ambitious

Default to the most valuable version of the task, not the most literal one. Before planning any
non-trivial change, read and follow `.claude/skills/ambitious-planning/SKILL.md`. In short:

- **Altitude: solve the goal behind the goal.** Ask what should happen *next* after the task is
  done. "Port function X" serves "cut over to Rust safely"; "add a reranker" serves "users get
  the right page first". Plan toward that outcome and say when the literal task is the wrong one.
- **Depth: work like the best practitioner in the field, with their instruments.** A retrieval
  engineer measures recall@k and MRR on a labelled set; they don't eyeball three queries. Start
  from the rawest material (real PDFs, real queries, raw DB rows, profiler output) rather than
  someone's summary. Set a hard, specific target ("right page first ≥ 85%", "ingest 100 pages
  < 60 s"), not "improve it".
- **Test your own beliefs.** State the assumption, then try to reproduce or break it from raw
  data before building on it.
- **Validate outside yourself.** Never let the model grade its own work. Proof is a test, a
  benchmark, a parity harness, a live smoke run, or a human-labelled set.
- **Range: borrow methods from other fields, not costumes.** Reframe the noun as a verb ("a
  search API" → "getting the one paragraph that answers a question, under a deadline, without
  leaking another tenant's data") and ask which disciplines own that verb: IR evaluation,
  security review, SRE, typography/document layout. Use their *methods*; don't role-play them.
- **Permission to try.** If a target looks impossible, attempt it, measure how far you got,
  and report honestly. A measured partial result beats a safe, unmeasured one.

Ambition never overrides the working rules: evidence, scope isolation and compatibility come
first.
