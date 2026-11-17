# Measured results

Every number here was run and is copied from the committed run folder next to it. Each
folder has its script, queries and per-query output. Nothing in this file is an estimate;
where a tier has not been measured on its real hardware, it says so.

Status by tier, 2026-11-17:

| Tier | Retrieval | OCR | Run on its own hardware |
|---|---|---|---|
| `lite` | measured on 2 cores | measured on 2 cores | yes, end to end |
| `lite-en` | measured on 2 cores, end to end | same sidecar as `lite` | yes, 1,110 pages through compose |
| `desk` | not measured | OCR measured on a GB10, not on 8 GB | `docker compose config` only |
| `studio` | not measured | as `desk` | `docker compose config` only |
| `station` | not measured | as `desk` | `docker compose config` only |
| `cloud` | measured through the eval harness | Mistral OCR, benchmark numbers not ours | the compose profile is not run (`DEP-3`) |

## Cloud tier vs KF main

[`docs/eval-ab/rk-cloud-vs-kf/`](eval-ab/rk-cloud-vs-kf/), run 2026-10-31, nDCG@10, every
query answered:

| Dataset | Queries | KF main | rk-cloud-text | rk-cloud |
|---|---:|---:|---:|---:|
| MIRACL-id-200 (document) | 200 | 0.813 | — | **0.845** |
| ViDoRe v3 HR (page) | 318 | not run yet | 0.675 | **0.710** |
| ViDoRe v3 CS (page) | 215 | not run yet | 0.778 | **0.822** |

Paired bootstrap, 95% CI: MIRACL-id-200 +0.032 [+0.006, +0.057]; ViDoRe HR text-only →
multimodal +0.035 [+0.018, +0.052]; ViDoRe CS +0.044 [+0.030, +0.059]. The KF comparison is
against KF's default stack, and KF has no ViDoRe arm yet. The page arm costs about $0.0006
per page to index.

## Lite on 2 CPU cores

[`docs/eval-ab/lite-2core/`](eval-ab/lite-2core/), 2026-10-24, two pinned cores:

| Lite, MIRACL-id-50 | nDCG@10 | Recall@10 | MRR@10 | Ingest 1,485 passages | Search p50 / p95 |
|---|---:|---:|---:|---:|---:|
| `intfloat/multilingual-e5-small` (384-d) | 0.802 | 0.938 | 0.823 | ~126 s | 42 / 49 ms |

ViDoRe HR on the same two cores: `lite` (e5-small) ingests at 1.55 pages/s with page
nDCG@10 0.545; `lite-en` (MiniLM with a 512-character embedding cap) ingests at 6.43
pages/s with page nDCG@10 0.517. MiniLM is English-only: on Indonesian MIRACL-id-50 it
scores 0.679 against e5-small's 0.802.

## What the containers actually use (lite, lite-en)

The 14 ViDoRe HR PDFs (1,110 pages, 51 MB, 4,931 chunks) were ingested through the compose
stack, then the 318 HR queries were run. `docker stats` sampled roughly every 3 s; CPU is
percent of one core.

| Container | Idle RAM | Ingest CPU avg / peak | Ingest RAM peak | Search CPU peak | Search RAM peak | Disk after ingest |
|---|---|---|---|---|---|---|
| tei-lite-en (MiniLM) | 188 MiB | 181% / 201% | 299 MiB | 113% | 298 MiB | 87 MB model |
| kb-lite-en | 4 MiB | 7% / 104% | 120 MiB | 52% | 120 MiB | — |
| qdrant | 46 MiB | 1% / 8% | 95 MiB | 139% | 208 MiB | 38 MB |
| rustfs (S3) | 90 MiB | 2% / 71% | 161 MiB | 5% | 134 MiB | 51 MB (originals) |
| postgres | 31 MiB | 6% / 19% | 40 MiB | 20% | 52 MiB | 49 MB |
| **Total** | **~360 MiB** | **~2 cores** | **~715 MiB** | | **~810 MiB** | **~225 MB + 2.5 GB images** |

`lite-en` ingested at 6.45 pages/s and answered at 30 queries/s (p50 32 ms, p95 45 ms) with
0 degraded and 0 empty results. `lite` (e5-small) on the same corpus took 733 s (1.51
pages/s), held 916 MiB idle and peaked at 1,326 MiB for the embedder alone — its
multilingual vocabulary is large — with the whole stack near 1.8 GiB and 22 queries/s (p50
44 ms, p95 60 ms).

Embedding is about 95% of ingest, so the embedder's core count sets pages/s; e5-small scaled
4.9 → 9.7 → 17.1 chunks/s on 1 → 2 → 4 cores. A 0.6B embedder on two cores starved the API.

## Reading scans, per engine

`docs/eval-ab/ocr-tiers`, olmOCR-bench, the benchmark's official scorer. Numbers are score
percent for text-layer PDFs / tables in them / scans / image-only PDFs; only pages the model
read are scored.

| Engine | Speed | Text / tables / scans / image-only |
|---|---|---|
| Lite: `ocr-cpu` sidecar (PP-Layout + PP-OCRv6 tiny + SLANet-plus), 2 cores of a 2016 Xeon | 0.69 pages/s | — / 38.8 / 31.4 / 52.5 |
| Desk/Studio/Station: Xiaomi-OCR-0 on vLLM (fp8) | Desk 1.35 pages/s at 16 in flight, 4.4 GB peak; 1.50 at 32 on a GB10 | 87.0 / 77.9 / 73.7 / 77.4 |
| … with `--ocr-tables accurate` (PP-DocLayoutV3 table pass on CPU) | 0.90 pages/s, detector-bound | 89.0 / 87.9 / 73.1 / 78.2 |
| Cloud, or `--ocr mistral` anywhere: Mistral OCR | billed per page | 77.4 / 85.5 / 70.8 / 75.3 |

Lite cannot read formulas into LaTeX or handwriting inside its CPU budget. The full method,
the vendor-parity check and the operating notes are in
[`kb-tiers.md`](kb-tiers.md#local-ocr-per-tier).

## Retrieval quality of the page arm

ViDoRe v3 HR, live, page nDCG@10: text-only 0.645, with the page arm **0.705**, and the
cloud default (one voyage embedder + pages + voyage rerank) 0.711 — against the best
published retriever at 0.654 and the best published pipeline with a reranker at 0.665. On
ViDoRe CS the page arm adds +0.049 [+0.035, +0.065] over text-only. Run folders:
[`vidore3-hr-en-night`](eval-ab/vidore3-hr-en-night/),
[`vidore3-cs-en-night`](eval-ab/vidore3-cs-en-night/),
[`vidore3-hr-en-image`](eval-ab/vidore3-hr-en-image/).
