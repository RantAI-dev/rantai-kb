# RET-20 — Qdrant quantization A/B

**Status: measured on GB10 (2026-11-25) at 1M points for `off` and `scalar`;
`binary` at 1M did not complete (Qdrant optimizer space error).** The smoke
run measured all three modes at 20k points. Results and raw stdout are below.

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

## Results — measured 2026-11-25 on GB10

Box: ThinkStation GB10 (arm64, 20 cores, 128 GB unified). Check before the big
run: `df -h /` → `71G` free; `free -g` → `available 67`. Release `kb-server`
from `bench/tbl2-quant @ aa3f8294` in `~/kb-target-tq`, using the Qdrant
container of the fresh `tq` `stack-up.sh` stack on gRPC `127.0.0.1:26344`.

### Smoke — 20k points, 100 queries (sanity, not a result)

```sh
"$BIN" eval quant-bench --qdrant-url http://127.0.0.1:26344 \
  --collection-prefix tqqb --points 20000 --dim 2048 --queries 100 --seed 42 \
  --modes off,scalar,binary --out ~/tq/m2-smoke
```

Raw stdout (`~/tq/logs/m2-smoke.out`):

```text
quant-bench: 20000 point(s), 2048 dims, 100 queries, seed 42, modes off,scalar,binary
     off: p50    4.17 ms  p95    4.76 ms  recall@20 0.6545  RAM not measured
  scalar: p50    2.19 ms  p95    2.60 ms  recall@20 0.7225  RAM not measured
  binary: p50    1.05 ms  p95    1.46 ms  recall@20 0.5060  RAM not measured
```

### Real run — 1M points, 2048-dim, 500 queries

```sh
"$BIN" eval quant-bench --qdrant-url http://127.0.0.1:26344 \
  --collection-prefix tqqb --points 1000000 --dim 2048 --queries 500 --seed 42 \
  --modes off,scalar --out ~/tq/m2-1m-offscalar --keep
```

Raw stdout (`~/tq/logs/m2-1m-offscalar.out`):

```text
=== M2 1M off+scalar start Wed Nov 25 06:57:38 AM UTC 2026 ===
/dev/nvme0n1p2  938G  820G   71G  93% /
quant-bench: 1000000 point(s), 2048 dims, 500 queries, seed 42, modes off,scalar
     off: p50    8.80 ms  p95   10.77 ms  recall@20 0.0407  RAM not measured
  scalar: p50    3.77 ms  p95    4.86 ms  recall@20 0.0433  RAM not measured
quant-bench: wrote /home/rismana/tq/m2-1m-offscalar/results.json and /home/rismana/tq/m2-1m-offscalar/report.md
=== M2 1M off+scalar exit=0 Wed Nov 25 08:12:15 AM UTC 2026 ===
```

`report.md` from that run (`~/tq/m2-1m-offscalar/report.md`):

```text
| mode | p50 (ms) | p95 (ms) | recall@20 | RAM |
|---|---:|---:|---:|---|
| off | 8.80 | 10.77 | 0.0407 | not measured |
| scalar | 3.77 | 4.86 | 0.0433 | not measured |

Targets at 1M points: p95 < 20.0 ms and recall@20 >= 0.97 against exact float32.
```

| mode | p50 (ms) | p95 (ms) | recall@20 vs exact float32 | RAM |
|---|---:|---:|---:|---|
| off (float32) | 8.80 | 10.77 | 0.0407 | not measured |
| scalar int8 | 3.77 | 4.86 | 0.0433 | not measured |
| binary | — | — | not measured at 1M (see below); smoke 1.05 / 1.46 / 0.5060 at 20k | not measured |

**Targets:** p95 < 20 ms **met** for off (10.77) and scalar (4.86); recall@20
≥ 0.97 **missed** (scalar 0.0433). Important caveat: the float32 `off` arm
itself only reaches 0.0407 against `exact: true` brute force on the tool's
uniform-random 2048-d vectors — the HNSW index barely agrees with exact search
at 1M on this synthetic shape, so the absolute recall target is not reached
by the reference arm either. Scalar's recall is marginally *higher* than the
float32 arm's (0.0433 vs 0.0407), i.e. no measurable recall loss from
quantization on this run. RAM is **not measured**: `GET /telemetry` at
`~/tq/logs/qdrant-telemetry.json` exposes only request counts and a collection
count, no per-collection memory.

### What did not complete, and why

- `binary` at 1M: three-mode run created `tqqb_binary` but Qdrant 1.19.2
  marked it red with `optimizer_status: {"error": "Service runtime error:
  Not enough space available for optimization, needed: 1.54 GiB, available:
  794.18 MiB"}` (full log `~/tq/logs/m2-1m-3mode-failed.out`). The run was
  stopped; binary's 1M numbers are not claimed. The 20k smoke numbers above
  are the only binary measurement.
- The first 1M attempt hit the tool's fixed 600 s `wait_green` window while
  Qdrant was still building the 1M×2048 HNSW index. `wait_green` in
  `rust/crates/kb-store-qdrant/src/bench.rs` was extended from 600 to 3600
  **polls** (a wait only; no measurement logic changed) and the release binary
  rebuilt. First attempt log: `~/tq/logs/m2-1m.out`.

Raw artifacts: `~/tq/m2-smoke/{report.md,results.json}`,
`~/tq/m2-1m-offscalar/{report.md,results.json}` and the logs above.
