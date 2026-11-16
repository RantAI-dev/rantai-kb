# ING-6 live results: ingest seats and OCR offload (2026-11-14)

The measurements behind backlog item `ING-6` ([spec](../../core/specs/ing-6.md)).
Every number comes from `bench.py` against a live kb-server (the `stack-up.sh` stack:
Postgres, Qdrant, RustFS) through the HTTP API. The raw per-document results are in
`results.jsonl`, and `run.sh` reproduces a run.

## Method

- **Corpus** (`build_corpus.sh`): 20 image-only PDFs of 50 pages each (1,000 pages), plus a
  300-page and a 1-page scan. They are rendered at 100 dpi from public PDFs (annual reports,
  BPS publications, Indonesian laws) and have **no text layer**, so every page needs OCR in
  both builds.
- **OCR**: `fake_ocr.py`, an OpenAI-style chat-completions server that answers each page after
  1.5 s ± 0.3 s and serves at most 16 pages at once. The emulator isolates the KB pipeline,
  which is what ING-6 changes, from any OCR model's speed.
- **Embeddings**: `fake_embed.py` (0.5 s per batch, dim 1024).
- **Settings**: both builds get `KB_OCR_CONCURRENCY=8`, so the OCR capacity is the same. The
  new build runs `KB_JOB_WORKERS=6` and the default seats `extract=2, embed=4, store=2`.
- **Builds**:
  - old: `kb-server-head`, the pre-ING-6 release build (feat/legacy-office-tiff), with a serial
    worker;
  - new: the release build of this branch at `dfc6e22f`.
- **Peak memory** is `VmHWM` of the kb-server process.

## Results

| | Old (serial) | ING-6 | |
|---|---|---|---|
| 20 × 50-page scans, end to end | 372.9 s | **210.3 s** | **1.77× faster** |
| OCR-bound floor (1,000 pages × 1.5 s ÷ 8 in flight) | | 187.5 s | ING-6 is at 89% of it |
| 1-page scan uploaded 2 s after a 300-page scan: ready after | 114.4 s | **2.6 s** | |
| kb-server VmHWM, throughput run | 300 MiB | 711 MiB | |
| First document ready, throughput run | 18.7 s | 63.6 s | later, see below |
| Documents failed | 0 | 0 | |

### Against the targets in the spec

| Target | Measured | |
|---|---|---|
| ≥ 3× faster end to end | 1.77× | **not met.** At equal OCR capacity the run is OCR-bound, and ING-6 already reaches 89% of the OCR floor, so no worker change can make it 3× faster. The old build loses its time in the gaps between documents. |
| A 1-page file behind a 300-page scan ready < 30 s | 2.6 s | met |
| VmHWM ≤ 1.5 GB | 711 MiB | met |

### Known regression: the first document finishes later

The OCR pool is first-in-first-out, so six documents share it evenly and the first one is done
at 63.6 s instead of 18.7 s. paragon's
OCR pool serves the oldest upload first. The fix is to give the OCR router the job's priority
and order its pool the same way as the stage seats. This is open.

## Bugs the live runs found (fixed on this branch)

- **The worker never claimed while busy** (`dfc6e22f`). The concurrent loop woke only when a job
  finished, so a file uploaded during a long document waited for it (113 s in the first live
  run, with 6 slots free). The unit test had queued both jobs before the worker started, which
  hid this. It now queues the small job after the big one is claimed, and fails on the old loop.
- **`stack-up.sh` pinned every dev stack to one job** (`a89fa9d1`). It exported
  `KB_INGEST_CONCURRENCY=1`, which ING-6 reads as the old name for `KB_JOB_WORKERS`.
- **Seats were held while waiting at OCR** (`01fd7ac0`). Found in review, not live.
