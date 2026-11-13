# ING-5 — Images are searchable by what they show

## Why

Before this task an image was searchable only by the text OCR could read off it.

- A standalone photo, drawing or diagram with no text was OCR'd to nothing and
  finished as a `ready` document with **zero chunks**: it could never be found, and
  nothing said so.
- Images inside `.docx`, `.pptx`, `.xlsx`, `.odt`, `.ods` and `.epub` were dropped,
  alt text included.
- Figure crops from the PDF layout chain were indexed by their caption and the page
  text around them; no model ever looked at the pixels.
- The visual stages that exist (page-image embeddings, page captions, vision rerank)
  ran on PDF pages only, and only on the Studio, Station and Cloud tiers.

The goal behind the task: **a user who describes a picture finds it**, whether they
uploaded it alone or it sits inside a document, on every tier, inside their tenant,
and an image that cannot be understood is reported, not silently empty.

## Target

Measured with `kb-server eval` on `images-v1` (below), document level, through the
live HTTP API. Queries are human-written (COCO and TextCaps captions, ChartQA
questions); the describing model never sees them.

| Slice | Metric | Baseline | Target | Model |
|---|---|---|---|---|
| Photos, no text (COCO) | Recall@10 | to measure (≈ 0 expected) | ≥ 0.85, MRR ≥ 0.60 | API-class VLM (free Gemma 4 for the measurement) |
| Charts (ChartQA) | Recall@10 | to measure | ≥ 0.80 | same |
| Same images inside DOCX / PPTX / PDF | Recall@10 | 0 (dropped) | within 0.05 of standalone | same |
| Photos, no text | Recall@10 | ≈ 0 | ≥ 0.70 | CPU-only local VLM (Lite / Desk sidecar) |

Text-only documents must rank exactly as before (no images, no new chunks).

## Design

One path for every **visual** (an image a reader of the document can see):

| Source | Page | Caption seed |
|---|---|---|
| Standalone upload (`.png .jpg .gif .webp .tif` …; each TIFF page) | its page | file name |
| `.docx` / `.odt` media | 0 | alt text (`descr`, `svg:desc`) |
| `.pptx` slide media | slide index | alt text |
| `.xlsx` / `.ods` media | sheet index | alt text |
| `.epub` `<img>` | 0 | `alt` |
| PDF embedded images (text-layer PDFs, opt-in) | its page | — |
| PDF layout-chain crops (MinerU, Mistral) | its page | extractor caption |

Each visual is decoded and normalised (longest side ≤ 1568 px, JPEG or PNG), then
filtered: too small, extreme aspect ratio, duplicate within the document, or over
the per-document cap. Survivors are stored as assets with their true content type and
read by an **`ImageDescriber`** port: one call returns `{kind, description, text}`,
where `kind` is photo, diagram, chart, screenshot, table, document, logo or
decorative. The answer is cached by model, prompt version and image hash. A visual
becomes a `figure` chunk (`[Image] description`, its visible text, the alt text and
nearby page text) with its `assetKey`, so the dense, BM25 and figure arms find it
and the caller can fetch the picture. Logos and decorative images are counted, not
indexed. A standalone image also becomes a page for the page-image arm, so visual
embeddings and vision rerank apply to it.

An image upload whose OCR text and description are both empty fails with a reason.
`Document.metadata.visuals` records `{found, described, indexed, skipped{…}, failed}`.

## Acceptance

- [ ] `images-v1` built and checked in as a manifest + download script (images are not committed)
- [ ] baseline measured on the current code
- [ ] targets above measured, numbers in `docs/eval-ab/images/README.md`
- [ ] every new chunk is filtered to the caller's documents (scope tests for dense, sparse, figure arms over visual chunks)
- [ ] an image with nothing searchable fails visibly
- [ ] HTTP changes additive only

## Left out

HEIC decoding (no pure-Rust HEVC decoder; HEIC stays OCR-only). Video. Image-to-image
search (query by picture).
