# RET-20 — Qdrant quantization A/B

**Status: tool built, 1M-chunk run not run yet.** No numbers in this directory
are claimed until the big-RAM box run below has actually happened.

## Goal

At ~1M chunks, 2048-d float32 vectors consume ~8 GB of Qdrant RAM. Scalar int8
is ~4× smaller and binary ~32× smaller with rescoring, but both can lose
recall. We need a switch an operator can turn on **only** after measuring that
recall loss against exact float32 on their own tenant shape.

## Method

`kb-server eval quant-bench` is a Qdrant-level instrument. For each requested
mode (`off`, `scalar`, `binary`) it:

1. Drops and recreates a fresh collection `{prefix}_{mode}` with the named
   `dense` cosine vector and that mode's quantization config (scalar int8,
   quantile 0.99; binary; both pinned in RAM with `always_ram: true`).
2. Upserts `--points` unit-length random vectors in batches of 256, each with a
   `tenant_id` payload and a tenant keyword index. Every mode receives the
   **same** deterministic vectors (seed `--seed`).
3. Polls `collection_info` until the status is green before searching.
4. Generates `--queries` independent deterministic query vectors (a separate
   RNG stream, so changing `--queries` does not change the stored points).
5. Computes ground truth once: `exact: true` float32 top-20 search on the
   `off` collection, filtered to the bench tenant.
6. For each reported mode, runs the same queries with top-20, the tenant
   filter, and (when on) `QuantizationSearchParams { rescore: true,
   oversampling: 2.0 }`. It records wall-clock client latency per query and
   recall@20 = `|exact_top20 ∩ mode_top20| / 20`, averaged over all queries.
7. Writes `results.json` and `report.md` into `--out`, then deletes the
   collections unless `--keep` is given.

The `off` collection is always created, even if the caller only asks to report
`scalar`/`binary`, because it is the float32 reference.

RAM is **not measured**: qdrant-client 1.19.0 exposes no telemetry call. The
report writes the literal string `not measured` rather than inventing a
number. On the big-RAM box, read Qdrant's dashboard (or `GET /telemetry`) at
the same moments and record it next to the table.

### Targets (RET-20, at 1M points)

- p95 latency **< 20 ms**;
- recall@20 **≥ 0.97** against exact float32, over 500 queries.

The targets are copied verbatim into `results.json` under `targets`; they are
not a claim that this run met them.

## Exact commands

From the repo root, on a box with Qdrant's gRPC port at `localhost:6334`:

```sh
# Small sanity run — proves the tool works, not a result
export CARGO_TARGET_DIR=/home/shiro/kb-target-ret20 CARGO_BUILD_JOBS=2
cargo run --manifest-path rust/Cargo.toml -p kb-server -- eval quant-bench \
  --qdrant-url http://localhost:6334 \
  --collection-prefix qb \
  --points 2000 --queries 50 --seed 42 \
  --modes off,scalar,binary \
  --out docs/eval-ab/quantization/smoke
```

The real benchmark (run later, on the big-RAM box):

```sh
cargo run --release --manifest-path rust/Cargo.toml -p kb-server -- eval quant-bench \
  --qdrant-url http://localhost:6334 \
  --collection-prefix qb \
  --points 1000000 --dim 2048 --queries 500 --seed 42 \
  --modes off,scalar,binary \
  --out docs/eval-ab/quantization/run-1m \
  --keep
```

`--keep` leaves the collections in place so the RAM reading can be taken
after the run; delete them by hand when finished. Without it the tool removes
every collection it created, including on a failed measurement.

Pure unit tests (no Qdrant needed) cover the deterministic vector generator,
recall@20, percentiles and flag parsing:

```sh
cargo test --manifest-path rust/Cargo.toml -p kb-store-qdrant bench::
```

## Results

**Not run yet.** Small smoke against a local Qdrant: not run (no Qdrant was
available at `localhost:6334` when this tool was built). 1M run: not run.

When the run happens, paste the raw stdout of the command above plus the
contents of `report.md` here, including the RAM reading taken from Qdrant
outside the tool.
