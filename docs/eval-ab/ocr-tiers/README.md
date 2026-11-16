# OCR per tier: speed and structure (2026-11-16)

**Question.** Which OCR engine fits **Lite** (CPU only, 2 cores) and **Desk** (8 GB GPU) at a target of
**1–2 pages/s**, with what Mistral OCR and Xiaomi-OCR-0 offer: tables, figures, formulas, reading order?

**Method.** olmOCR-bench (1,403 single pages), the benchmark's official scorer, split into tracks:
text-layer PDFs (1,188 PDFs, 5,638 tests), scans (134 / 984) and image-only PDFs (81 / 397, of which 98
table tests). Lite only OCRs pages without a text layer, so Lite is scored on scans and image-only.
Only pages the model read are scored (`kb-server eval ocr` counts a text-layer fallback as failed).

- **Desk** is emulated on the ThinkStation (NVIDIA GB10) with vLLM capped to a Desk-sized share,
  `--gpu-memory-utilization 0.04` (≈4–5 GB). Spec-sheet memory bandwidth of the GB10 (≈273 GB/s) is
  close to an RTX 4060 8 GB (≈272 GB/s), and these models are bandwidth-bound, so pages/s should carry
  over roughly; **not run on a real 8 GB card**. Speed is timed on a fixed 281-page sample
  (every 5th page); quality on all 1,403.
- **Lite** is run on the dev box: Intel Xeon E5-2680 v4 (2016, Broadwell), the OCR sidecar **and**
  `kb-server eval ocr` pinned to the same two cores (`--cpuset-cpus 0,1`, `taskset -c 0,1`), so page
  rendering shares the budget. A current server core is likely faster; **not measured**.

## Desk (8 GB GPU)

| Engine (vLLM, Desk-sized share) | Pages/s | Peak GPU | Text-layer PDFs | Tables (922) | Scans | Image-only | Verdict |
|---|---|---|---|---|---|---|---|
| **Xiaomi-OCR-0** fp8, 32 in flight | **1.50** (1.08 at 16, 0.72 at 8) | 4.3 GB | **87.0** | **77.9** | **73.7** | **77.4** | **meets target** |
| Xiaomi-OCR-0 + layout fallback (detector on CPU) | 0.90 (detector-bound, full GB10) | — | 89.0 | 87.9 | 73.1 | 78.2 | best quality |
| PaddleOCR-VL-1.6, whole page `OCR:` | 1.21 | 4.5 GB | 59.8 | 9.4 | 64.6 | 43.8 | tables fail in whole-page mode |
| LightOnOCR-2-1B fp8 (card recipe) | 0.44 | 4.8 GB | not scored | — | — | — | too slow at this memory |
| Mistral OCR (API, reference) | — | — | 77.4 | 85.5 | 70.8 | 75.3 | paid, external |

**Desk footprint (Xiaomi-OCR-0, 16 pages in flight, same 281-page sample).** The vLLM share
does not track the process's real peak, so the shipped config fixes the cache in bytes:

| vLLM flags (all: fp8 weights + KV, prefix cache off) | Started | Peak process | Pages/s |
|---|---|---|---|
| share 0.025 (≈3.2 GB), with or without `--enforce-eager` | no ("no memory for the cache blocks") | — | — |
| share 0.02, `--enforce-eager`, context 12288 | yes, 0.55 GiB cache | 4.17 GB | 0.86 |
| **`--kv-cache-memory-bytes 1GiB`, `--max-num-batched-tokens 8192`, `max_pixels` 2048², context 16384** | yes | **4.44 GB** | **1.35** |
| the same with `--enforce-eager` | yes | 4.33 GB | 0.86 |

CUDA graphs cost 0.1 GB and buy 57% more pages/s, so the compose default keeps them.

Xiaomi's hybrid attention keeps its KV cache small, so 32 pages fit in ≈1 GB of cache; LightOnOCR's
full-attention decoder fits ~3 pages in the same share. PaddleOCR-VL's tables come from its own
layout → per-region pipeline, which a whole-page prompt does not run.

## Lite (2 CPU cores)

Per-page time on one core (110 dpi, 20 sample pages): layout (YOLOv8n, page shrunk to 1,024 px)
0.16 s, text detection (PP-OCRv6 tiny) 0.25 s, **recognition (PP-OCRv6 tiny) 1.0 s** (64 lines/page),
table structure (SLANet-plus, reusing the page's OCR lines) 0.13 s per table.

| Engine (2 cores, end to end through kb-server) | Pages/s | Scans | Image-only | Tables (98) | Has |
|---|---|---|---|---|---|
| PP-OCRv6 small (the current Lite default) | ≈0.03–0.15 | — | — | — | text |
| PP-OCRv6 tiny, 2 workers, 150 dpi | 0.67 | 30.4 | 31.6 | 0.0 | text |
| + XY-cut reading order, 110 dpi | 0.84 | 31.5 | 35.8 | 0.0 | text, order |
| + layout + tables, YOLOv8n layout (prototype), 110 dpi | 0.75 | 31.6 | 50.1 | 49.0 | text, order, headings, HTML tables, figure boxes |
| **shipped `ocr-cpu`: PP-Layout CDLA (Apache-2.0), headers/footers dropped, XY-cut inside regions, 110 dpi** | **0.69** | **31.4** | **52.5** | **38.8** | the same, without page headers/footers |
| Xiaomi-OCR-0 on a GPU, for reference | 1.50 | 73.7 | 77.4 | 93.9 (with fallback) | everything |

OCR-only engines timed on the same two cores (20 pages, OCR only): PP-OCRv6 small 0.14 p/s,
PP-OCRv6 small + OpenVINO 0.21, PP-OCRv5 mobile 0.15, Tesseract 5 (2 processes) 0.48,
OnnxTR db_mobilenet + crnn 0.57, **PP-OCRv6 tiny 0.55 → 1.04** (960 px detection, two 1-thread
workers). Layout models: YOLOv8n 0.10 s/page (at 1,024 px), PP-DocLayoutV2 2.1 s, V3 2.6 s,
DocLayout-YOLO 6.8 s. Recognisers: PP-OCRv6 tiny 1.0 s/page, v5 mobile 6.3 s, v4 mobile 13.3 s;
OpenVINO and larger batches did not help.

The YOLOv8n layout weights (360LayoutAnalysis) need an application to 360 for commercial use,
so the shipped default is Paddle's CDLA layout model: 52.5 vs 50.1 on image-only pages, 38.8 vs
49.0 on their tables. Dropping page headers and footers lifts the "absent" tests from 48.6 to
84.3; reading text in XY-cut order *inside* each region was needed because CDLA sometimes boxes
a whole dense page as one figure (present 17.8 → 30.0 with the fix).

What Lite cannot do inside the budget: **formulas** (no CPU LaTeX recogniser fits; equations come
out as text — scans with maths score 0) and **handwriting** (the scan track is mostly historic
handwritten letters).

## Reproduce

`lite-structure-sidecar/` is the prototype: `server.py` serves `ENGINE=rapidocr|tesseract|onnxtr|structure`
behind the chat-completions shape `kb-server` already sends (`KB_OCR_RECIPE=paddle`); the timing
scripts are the ones quoted above. Desk runs used `jalan_vlm.py` / `xserve.sh` on the ThinkStation
(`~/olmocr-bench/uji-olmocr`).
