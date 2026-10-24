# Big-PDF ingest: 427 s → 163 s for 943 pages (2026-10-24)

**Result.** A cold ingest of the 943-page *Introduction to Computer Science* PDF (50.9 MB,
3,771 chunks, 945 page images), with the page-image arm on and provider-like latency, went
from **427 s to 163 s**. Target was under 200 s. Retrieval is unchanged: the live ViDoRe CS run
with the new binary scores 0.830 page nDCG@10 against 0.828 before (+0.001 [−0.000, +0.004]),
0 failed, 0 degraded.

## Method

`rust/scripts/bench-ingest.sh` ingests one PDF into a fresh stack with an empty cache, against
`kb-eval/scripts/fake_embed.py`: an OpenAI-shaped embeddings server that sleeps like the real
provider and logs every request with the number in flight. Latency was measured against
`voyageai/voyage-multimodal-3.5` on OpenRouter the same day: ~2.5 s per request of 4 pages
(text + image), ~1.8 s per request of 32 text chunks. No paid call is made; the numbers below
are for that latency, and a real provider adds its own variance and rate limits.

## Where the time went (before)

| Step | Time | Why |
|---|---:|---|
| Extract text | ~17 s | |
| Embed text, 118 requests | 211 s | **one request at a time**: `KB_EMBEDDING_CONCURRENCY` was read but never used (Bun runs 4) |
| Render pages | ~56 s | single thread (pdfium is not thread-safe) |
| Embed pages, 237 requests | ~149 s | 4 in flight |
| **Total** | **427 s** | every step waits for the one before |

## Changes

1. **Text batches run `KB_EMBEDDING_CONCURRENCY` at a time** (default 4), results kept in input
   order. Bun's name `KB_EMBED_CONCURRENCY` is honoured when the Rust name is unset.
2. **Page images run alongside text embedding**, not after it. Chunk ids are fixed before
   embedding, so the page arm does not need the stored chunks. If the text step fails, the
   pages stored alongside it are deleted, because search scope does not look at document status.
3. **Pages go in windows of 64**: the next window renders (`PageRenderer::render_range`) while
   the current one is embedded and stored. No step holds every page image or request body of a
   long PDF at once.
4. **A failure costs only its own pages.** Before, one failed request or one unrenderable page
   dropped every page of the document, and the only trace was a log line. That is how the first
   CS run lost its 943-page book's page arm and fell to 0.317. Now a failed window or page is
   counted, the rest are stored, and the result is written on the document:
   for example `metadata.pageImages = {"indexed": 881, "failed": 64, "error": "..."}`.
5. **A PDF with no text layer still gets page images.** Ingest used to stop before the page
   arm when there were no chunks. A scan is the document the page arm helps most.

| Run | Ingest | Notes |
|---|---:|---|
| before | 427 s | text requests always 1 in flight |
| + 1, 2 | 207 s | text 4 in flight, ~55 s, alongside pages; pages start after the full render |
| + 3 | **163 s** | pages start at 24 s instead of ~69 s |

What is left is the page embedding itself: 237 requests at 4 in flight is ~150 s at this
latency. Going faster means raising `KB_PAGE_EMBEDDING_CONCURRENCY`, which depends on the
provider's rate limits (not measured).

Tests: a failed window loses only its pages; an unrendered page is counted and never sent;
text batches overlap and keep their order; the outcome is on the document; a scan gets
pages; a failed text step leaves no pages. Workspace: 821 passed, 0 failed.
