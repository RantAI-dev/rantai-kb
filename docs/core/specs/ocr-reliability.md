# OCR reliability — plan for #28, #35, #29, #36

## Goal

Literal ask: four bug fixes in the OCR path (hybrid routing, retry after an
outage, an extraction quality gate, and parking jobs while OCR is down).

Goal behind the goal: **an uploaded book is either indexed as real text or the
job says, loudly, why it is not.** Today the service can mark a document
`ready` with 1 chunk of garbage, fail a job with "the PDF has no readable
text" when OCR was healthy, and fail a job with "no text for any page" when
the OCR backend was simply restarting. All three are the same class of
failure: a state the operator cannot see.

Chosen level: fix the four reported symptoms in the Rust runtime, with the
state machine that makes them visible (`waiting_for_ocr`, `needs_ocr`,
per-dependency health), rather than one narrow patch each. A higher level
(a whole ingest-state redesign) is out of scope for this session.

## Target

Baseline (measured in the repro tests below, not from the issue text):

- #28: a PDF whose pages carry no extractable text produces
  `OCR failed for "x.pdf": the PDF has no readable text` in `hybrid` mode with
  a healthy provider — **0 pages sent**.
- #35: a retry after an OCR outage fails with `the OCR provider returned no
  text for any page` while the breaker is still open — **0 pages sent, 0
  provider requests**, and the message does not name the breaker.
- #29: a document whose text layer is garbage is marked `ready` with a
  handful of chunks — **no signal at all**.
- #36: a job submitted while OCR is starting fails outright; `/v1/health`
  says nothing about OCR.

Target after the change:

- #28: a page with no visible characters is an OCR candidate regardless of
  raster coverage (`visible_chars <= max_text && (image_area >= area ||
  visible_chars == 0)`).
- #35: a breaker-skipped page yields a distinct, retryable error
  (`OCR breaker open`), and a transport error is never cached.
- #29: with `KB_EXTRACT_QUALITY_GATE=on`, a page whose text layer fails a
  sanity check is re-read with OCR, and a document that is still near-empty
  ends as a visible degraded state (`needs_ocr` / failed with a reason), never
  a silent `ready`.
- #36: a job that needs OCR waits with bounded backoff, then parks in
  `waiting_for_ocr` and resumes by itself; `/v1/health` reports embedder,
  reranker and OCR as `ready` / `starting` / `unavailable`.

## Depth

Expert move not asked for: the breaker is **process-wide**, so one document's
outage poisons every later document for the whole cooldown — the retry in
#35 hit a breaker opened by *another* job. The fix therefore includes closing
the breaker when a health probe sees the backend answer again, not only on a
successful page read.

Raw material: generated PDFs with pdfium (no binary fixtures), a fake
provider that counts requests, and the production error strings from the
issues.

Instruments: `cargo test` for the router contract, the engine's fake job
store for the queue, and the boundary check for the layering.

Outside validator: the issue reporter's own error strings — the repro tests
assert the *old* message and the *new* message, so the fix is measured
against the reported symptom, not against my reading of it.

## Assumptions to break

1. "The retry after an outage failed because empty results were cached."
   Test: run the router against a provider that fails with a transport error,
   then assert the page cache received **no** entry for that page. If the
   cache is clean, the cached-empty hypothesis is false and the breaker is
   the cause.
2. "A page with zero visible characters always needs OCR." Test: a page with
   an *invisible* OCR layer has zero visible characters but a usable local
   text layer. Check the cost of re-reading it before changing the rule.
3. "A quality gate can be on by default." Test: the gate's per-page check on
   real digital text — it must not flag ordinary prose. Off by default until
   that is shown.

## Plan

1. **#28** — `page_needs_ocr` in `kb-extract`; hybrid candidates use it.
   Test: a PDF with a text-less, image-less page is sent to OCR.
2. **#35** — the router distinguishes "breaker skipped this page" from "the
   provider answered with nothing"; transport errors are never cached; the
   health probe closes a stale breaker. Test: the repro (breaker open →
   retryable error naming the breaker) and the cache assertion.
3. **#29** — `KB_EXTRACT_QUALITY_GATE` (default off): a per-page sanity check
   in `kb-core` (pure logic), OCR forced for the pages that fail it, and a
   per-document near-empty check in the engine that ends the job in a visible
   degraded state. `pages` and `charsExtracted` on the document response.
   Test: a garbage-text-layer page is re-read; a document that stays empty
   fails with a reason.
4. **#36** — `waiting_for_ocr` job state (additive; the column is TEXT, so
   no migration), bounded backoff, automatic resume, and per-dependency
   health. Test: a job whose OCR is down is parked, and a later claim picks
   it up again.

## Validation

`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`,
`cargo run -p kb-server -- check-boundary`, each with its real output pasted.

## Out of scope / risks

- The Bun worker does not know `waiting_for_ocr`; the Bun service was removed
  in v0.2.0, so this only matters for a stale cutover deployment.
- The health probe adds up to three HTTP calls per `/v1/health`; the result
  is cached for a few seconds so a polling client does not hammer a backend
  that is down.
- The quality gate's thresholds are heuristics; they are behind a flag and
  off by default because a false positive costs a full OCR pass.

## Landed

#36 (2026-11-20). The job parks, the document says so, and the health route
names the backends:

- `IngestJob.status` and `Document.status` are TEXT columns, so
  `waiting_for_ocr` is a value, not an enum. One nullable column was added:
  `IngestJob.resumeAt` (`prisma/migrations/20261120130000_job_waiting_for_ocr`).
- `PARK_JOB` parks a job that is still `processing`, counts the attempt and
  sets `resumeAt`; `RESUME_PARKED_JOBS` moves the jobs whose wait is over
  back to `pending`; `CLAIM_NEXT_PENDING` claims `pending` only, so a waiting
  job holds no worker slot. The worker sweeps before every claim.
- The wait doubles every try, is capped by `KB_OCR_WAIT_MAX_S`, and after
  `KB_OCR_WAIT_MAX_ATTEMPTS` tries the job fails for good.
- `GET /health` and `GET /v1/health` carry `backends.embedder`,
  `backends.reranker` and `backends.ocr`, each `ready` / `starting` /
  `unavailable`. The status code of the route does not change.
- The probe closes the outage breaker when the OCR backend answers again.

Tests: `kb-engine` (`a_job_whose_backend_is_unavailable_waits_instead_of_failing`,
`the_queue_keeps_working_while_a_job_waits`,
`a_waiting_job_is_not_claimed_while_it_waits`,
`a_waiting_job_goes_back_to_pending_when_its_wait_is_over`,
`a_waiting_job_is_resumed_by_the_worker`, `a_job_that_keeps_waiting_fails_for_good`,
`the_wait_grows_with_the_attempts_and_is_capped`),
`kb-store-pg` (`a_waiting_job_holds_no_worker_slot`,
`a_waiting_job_comes_back_when_its_wait_is_over`, `a_requeued_job_forgets_its_wait`),
`kb-server` (`backends_tests.rs`, `waiting_tests.rs`).
