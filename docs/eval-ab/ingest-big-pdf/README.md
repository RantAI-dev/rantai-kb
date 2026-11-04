# Big-PDF ingest: 427 s → 71 s for 943 pages (2026-10-24)

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

## Page-embedding concurrency: 16 by default for hosted embedders

After the changes above, the page embedding itself was the critical path: 237 requests at 4 in
flight is ~150 s. `kb-eval/scripts/page_embed_ratelimit_probe.py` sent real 4-page requests to
`voyageai/voyage-multimodal-3.5` on OpenRouter at rising concurrency (paid, $0.55 total):

| In flight | Requests | Errors | Pages/s | Latency p50 / max |
|---:|---:|---|---:|---|
| 4 | 16 | none | 7.0 | 2.19 / 2.79 s |
| 8 | 32 | none | 11.5 | 2.35 / 4.10 s |
| 16 | 64 | none | 23.0 | 2.39 / 3.82 s |
| 32 | 128 | none | 33.0 | 3.31 / 6.14 s |

No 429 at any level; latency stays flat up to 16 and rises at 32. The default
`KB_PAGE_EMBEDDING_CONCURRENCY` is now **16 for hosted embedders** and stays 4 for a local vLLM
(`KB_PAGE_EMBEDDING_FORMAT=vllm`), whose GPU is shared with the chat model.

The same benchmark at 16 in flight (fake latency 2.4 s): **71 s** for the 943-page PDF
(427 s at the start of the day, 163 s after the changes above). Pages now finish ~57 s after
they start, which is the pdfium render time: rendering, single-threaded, is the critical path.

Not changed: `KB_PAGE_IMAGE_MAX_PAGES` defaults to 500, so a longer document gets page images
for its first 500 pages only (the evals set 2,000). It is also a spend cap (~$0.30 per 500
pages on voyage), so raising it is a product decision.

Tests: a failed window loses only its pages; an unrendered page is counted and never sent;
text batches overlap and keep their order; the outcome is on the document; a scan gets
pages; a failed text step leaves no pages. Workspace: 821 passed, 0 failed.
