# ING-6 — Ingest seats and OCR offload

## Why

The owner's working KB (paragon, `kb-bridge`) stays light under load. Two design choices do it:

1. **Seats.** Each pipeline step has a small slot pool (`cpu=2, embed=4, save=2`). A document
   holds a seat only while it is in that step, and the oldest upload is served first. OCR has
   its own pool, and no seat is held while a document waits on OCR (`kb-bridge/kb_progress.py:83-160`,
   `kb_ingest.py:2247`). Measured there: 20 × 50-page PDFs in 43 s, peaking at ~2 GB.
2. **Offload.** In production every PDF page and every image goes to Mistral OCR
   (`KB_OCR_MODE=full`), so the KB process never renders or reads pages itself. There is a
   per-page OCR cache, an outage breaker, a 300-page cap with a visible marker for the pages
   past it, and a per-page scan test for the `hybrid` mode.

rantai-kb today does neither:

- **One job at a time.** `IngestWorker::tick` claims up to `KB_INGEST_CONCURRENCY` jobs and
  runs them **one after another** (`kb-engine/src/ingest.rs:294-307`; `runtime.rs:91` spawns
  one worker). A 300-page scan holds every small file behind it.
- **Scanned or not, per document.** `is_pdf_scanned` averages characters over all pages
  (`kb-extract/src/scanned.rs:12-35`). A mostly-digital PDF with a few scanned pages loses
  those pages, and a sparse digital PDF (slides, one line plus a picture) is OCR'd whole.
- **OCR cached per document, all or nothing.** `PageOcrExtractor` fails the whole extraction
  when one page fails (`page_ocr.rs:21-24`). The cache key is the whole document, so a retry
  pays for every page again.

The goal behind the task: **a KB service that stays small and fast whatever is uploaded.**
Many documents are in flight at once, memory stays bounded, the heavy page work runs at an OCR
service (Mistral on Cloud, a GPU OCR server on the local tiers), a small file never waits
behind a big one, and every page that could not be read is reported.

## Part A — seats (kb-engine, kb-server)

- The worker admits up to `KB_JOB_WORKERS` jobs at once (default 6). Each admitted job runs
  as its own task, with its own heartbeat, cancellation and progress events, exactly as
  `run_job` does today.
- **Stage seats.** These are counting gates shared by all jobs in the process:
  - `extract`: local parsing, PDF splitting and rendering. Default 2.
  - `embed`: one document's embedding requests. Default 4.
  - `store`: vector upsert, document row, graph writes. Default 2.

  A job holds a seat only for the stage it is in, and releases it on every exit path. It is a
  guard, so an error or panic releases it too. Override with `KB_SEATS="extract=2,embed=4,store=2"`.
- **Remote pools are process-wide, not per job.** OCR page requests (`KB_OCR_CONCURRENCY`) and
  picture descriptions (`KB_IMAGE_DESCRIBE_CONCURRENCY`) are each one pool for the whole
  process. A job waiting on OCR or a description holds **no** stage seat.
- **Oldest first.** When a seat or a pool slot frees, the waiter whose job was created first
  gets it, not whoever asked first.
- `KB_INGEST_CONCURRENCY` stays accepted as an alias for `KB_JOB_WORKERS`, so existing
  deployments keep working. Tier defaults: Lite 2 jobs, the others 6.
- Reclaim, the stale-heartbeat recovery and graceful shutdown keep their contracts. On
  shutdown, admitted jobs drain up to `WORKER_DRAIN_TIMEOUT`.

## Part B — OCR offload (kb-extract, kb-cache, kb-server config)

`KB_OCR_MODE = off | hybrid | full`:

- **`off`.** No OCR. Scanned pages yield no text and are reported.
- **`hybrid`** (default when an OCR provider is configured). Decided **per page**. A page is a
  scan when raster images cover ≥ `KB_OCR_SCAN_AREA` (0.90) of it and it has ≤
  `KB_OCR_SCAN_TEXT` (50) visible characters. Text with render mode "invisible" (an existing
  OCR layer) does not count. Scan pages go to OCR; the other pages use the local text layer. A
  mixed PDF gets both, in page order, with a correct `page_map`.
- **`full`.** Every PDF page goes to OCR. The local text layer is that page's fallback when its
  OCR fails.

Shared rules:

- **Request shape per provider.** A provider that takes a PDF (Mistral `/v1/ocr`) gets sub-PDFs
  of `KB_OCR_BATCH` pages (default 8), built by pdfium copying pages into a new document. A chat
  OCR provider (`PageOcrExtractor`: a vLLM or llama.cpp OCR model) gets one rendered page image
  per request, as today.
- **Page cache.** kb-cache namespace `ocr-page-v1`. The key is sha256 over provider id, model,
  prompt, dpi and the page's bytes (the rendered image, or the one-page PDF). A re-upload or a
  retried job pays only for pages not yet read. Failures are not cached.
- **A failed page is reported, not fatal.** After retries, a page with no text gets the marker
  `[page N could not be read]` in the content. The document's `metadata.ocr` records
  `{mode, provider, pages, ocrPages, cachedPages, localPages, failedPages: [..], skippedPages: [..]}`.
  A document where **every** page failed fails the job with the provider's error. That keeps
  "a failed search is not an empty result".
- **Breaker.** After `KB_OCR_BREAKER` (3) consecutive provider failures (connection error or
  5xx after retries), no page is sent for `KB_OCR_BREAKER_COOLDOWN_S` (60). Pages in that
  window take the local text or the marker. This is logged and recorded in `metadata.ocr`.
- **Page cap.** At most `KB_OCR_MAX_PAGES` (300) pages per document go to OCR. Pages past it use
  local text or the marker, and are listed in `skippedPages`.
- **The same path for images.** Standalone images and TIFF pages go through the same page path,
  with the same cache, pool and breaker.
- **Unchanged.** Office, text and HTML never go to OCR.
- **Spend.** The Mistral path is tested against a fake HTTP server. No paid OCR run happens
  without the owner's go-ahead and a cost estimate (`AGENTS.md` rule 7). Mistral lists
  $0.004 per page (`kb-bridge/kb_ingest.py:1727`); this is not measured here.

## Target (acceptance; measured live, not in unit tests)

| What | How | Target |
|---|---|---|
| Throughput | 20 public PDFs × ~50 pages, `full` mode, OCR on the GPU box, same server and settings before and after | end-to-end ≥ 3× faster than the serial worker |
| No head-of-line blocking | A 1-page PDF uploaded right after a 300-page scan | the small file is `ready` in < 30 s |
| Memory | kb-server `VmHWM` during the throughput run | ≤ 1.5 GB |
| Per-page routing | A PDF with 3 scanned pages among 20 text pages, `hybrid` | exactly 3 pages OCR'd (today: 0 or 23) |
| Page cache | Re-upload of a `full`-mode document | 0 OCR requests |
| A failed page is visible | The OCR server stops answering for one page | the document is `ready`, the page carries the marker, `failedPages` names it |
| Isolation | `kb-server eval leak` | 0 leaks, unchanged |
