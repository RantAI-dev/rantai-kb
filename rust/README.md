# Rust workspace for the KB read path.

Phase R1 of `docs/porting-plan.md`. Pure read path against the same Postgres
and SurrealDB schemas the Bun service uses. No ingest, no writes, no SSE, no
entity arm, no vision gate.

Layout under `crates/`:

- `kb-core` — pure domain logic (chunker, RRF, neighbour weave, figure policy,
  text-layer signals, page map, ingest policy, MIME registry). No I/O.
- `kb-ports` — store and inference traits, shared domain types. No deps.
- `kb-inference` — embedding and rerank HTTP clients (reqwest).
- `kb-store-pg` — Postgres / sqlx adapter for documents, knowledge bases,
  API keys. Constructs `DocumentScope` (the only place it can be built).
- `kb-store-surreal` — SurrealDB HTTP `/sql` adapter for chunks (vector, BM25,
  neighbours, figures).
- `kb-engine` — retrieval orchestration. No names of adapters.
- `kb-server` — axum composition root, config, auth, routes. The only crate
  that constructs adapters and reads environment.

## Build rules in effect

- `cargo build`, `cargo check`, `cargo doc`, `cargo clean`, `--release`,
  Docker and tool installations are banned by the task.
- One cargo process at a time, capped at two jobs.
- Hard budget of 9 compiling runs per phase: at most 6
  `cargo clippy --workspace --all-targets --keep-going --message-format=short`
  and at most 3 `cargo test --workspace`. See `NOTES.md` for the running count.
- No `surrealdb` crate, no AWS SDK. SurrealDB is reached over HTTP.

## Conventions

- Edition 2024, resolver 3, shared lints in the workspace root.
- Owned data at crate boundaries, no lifetime parameters on public types.
- `Arc<dyn Trait>` with `async-trait` for ports; concrete types over generics
  in adapters; explicit types on closures and collects.
- Module docs say why; comments explain constraints, not mechanics.
- Tests live beside the code, table-driven; engine tests use in-memory fakes
  of the ports; server tests use `tower::ServiceExt::oneshot`.