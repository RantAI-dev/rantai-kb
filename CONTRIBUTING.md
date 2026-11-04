# Contributing

rantai-kb is a Rust service. The Bun/TypeScript service was removed in `v0.2.0`; do not
add TypeScript to this repository.

## Before you start

- Read [`AGENTS.md`](AGENTS.md) for the working rules.
- Pick work from [`docs/core/BACKLOG.md`](docs/core/BACKLOG.md), the one list of work.
  Update its row (status, PR) in the same pull request that does the work.
- For a non-trivial change, read the product context in
  [`docs/core/PRODUCT.md`](docs/core/PRODUCT.md) and the relevant spec under
  `docs/core/specs/`.

## Build and test

The workspace lives under `rust/`. Install a stable Rust toolchain, then:

```bash
# libpdfium is needed by the extractor tests
./rust/scripts/fetch-pdfium.sh ./rust/.vendor/pdfium

cargo test --manifest-path rust/Cargo.toml --workspace
cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings
cargo fmt --all --manifest-path rust/Cargo.toml --check
cargo run --manifest-path rust/Cargo.toml -p kb-server -- check-boundary
```

Live checks (they start containers, so Docker must be available):

```bash
rust/scripts/smoke.sh          # ingest, search, tenant isolation, delete
rust/scripts/ci-eval-gate.sh   # retrieval quality and cross-scope leak gate
```

To run the service locally, use the tier flow in [`rust/deploy/README.md`](rust/deploy/README.md):

```bash
rust/scripts/kb-dev.sh setup
rust/scripts/kb-dev.sh up lite-en
rust/scripts/kb-dev.sh key my-tenant
```

## Scope changes need tests

Tenant and knowledge-base scope is the product's hard security rule. Any change to a
retrieval arm or a document route needs a test proving that:

- another tenant's key sees nothing;
- a key limited to other knowledge bases sees nothing;
- a failed arm is reported in `degrades`, not silently treated as an empty result.

## Commits and pull requests

- Use conventional commit messages, for example `fix(retrieval): …`,
  `feat(server): …`, `docs: …`, `ci: …`. Add `!` for a breaking change.
- Keep changes scoped and commit early.
- Say what you ran. Paste real command output; "not run" is acceptable, an invented
  result is not.
- Describe the evidence in the pull request: tests, benchmark or live smoke output, and
  the folder under `docs/eval-ab/` when a retrieval number changes.

## What not to change

- `kb-core` must stay free of I/O; `kb-engine` must not name an adapter. Both boundary
  checks must pass.
- Do not commit secrets, corpus data or model weights.

## Security

Report vulnerabilities privately through GitHub security advisories. See
[`SECURITY.md`](SECURITY.md).
