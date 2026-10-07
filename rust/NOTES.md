# Build & design notes

Running ledger of decisions taken while the plan left something open, plus the
budget counter for compiling cargo runs.

## Cargo compile budget

- 9 total: 6 `cargo clippy --workspace --all-targets --keep-going
  --message-format=short`, 3 `cargo test --workspace`.
- `cargo fmt`, `cargo generate-lockfile`, `cargo metadata` do NOT compile and
  do NOT count.

| # | command | wall | outcome |
|---|---------|------|---------|
| 0 | scaffold (no compile) | — | uncompiled |